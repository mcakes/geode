//! One market-data panel tile (market-data spec §8.2): asks for one
//! document by key through `DataHandle`, keeps the prepared
//! [`MatrixModel`] a frame paints from, and owns the cursor, the yank, the
//! `/` find and the `:` vocabulary over it.
//!
//! The BODY is gpui-component's table, driven by this crate's own
//! [`MatrixDelegate`] (user ruling 2026-09-14 — visual unity with the
//! blotter, superseding roadmap ruling 6's hand-painted uniform row list).
//! The chip header above it is this tile's own, unchanged. Two rules the
//! seam rests on: **the tile's cursor stays the truth** and the delegate
//! only mirrors it ([`Self::sync_cursor`]), and **every model swap goes
//! through [`Self::install_model`]**, which calls `TableState::refresh` —
//! the component caches `column()`'s answers in `col_groups` and paints
//! its header from that cache alone, so a swap without a refresh paints
//! the previous document's columns.
//!
//! What this tile is NOT is a blotter. There is no view, no grouping and
//! no scope: a document request is (dataset, key, as-of) and nothing else
//! (Part 1 §7), so the only frame counters it follows are `as_of` and
//! `data` — every publish bumps `data`, and a document select is one
//! key's worth of rows, cheap enough to just re-run. `flip` is
//! deliberately not among them, for the reason CLAUDE.md gives for the
//! blotter: `flip` never means "requery".
//!
//! It does still ANSWER the flip barrier for every change (Phase 4
//! §3.10), because `ShellView::visible_tile_keys` cannot know which tiles
//! follow which counters: a change it requeries for is answered by its
//! own delivery (`arrive`), and one it does not is answered on the spot
//! (`self_arrive`). Following fewer counters than the blotter is exactly
//! why that second door has to exist — a panel that stayed silent would
//! hold every blotter on screen to the 250 ms deadline on every scope
//! keystroke.
//!
//! Cell EDITING is here (spec §8.3/§8.6): `edit` opens one tile-owned
//! `InputState` in the cursor cell and `key_context` reports `mode ==
//! insert`, which is what makes the shell hand every bare keystroke to
//! that input instead of matching it; `commit` parses the text through the
//! column's declared type and writes the [`Draft`]; `:bump` and `:revert`
//! are the same draft from the `:` line.
//!
//! The draft STATES (spec §8.4) are here too: a delivery whose `as_of`
//! differs from the draft's own base goes `Behind`, and the panel keeps
//! painting the BASE generation (`base_snapshot`) under the edits rather
//! than the newer one it just received — `:rebase` moves the edits onto
//! the newer document by label (a dropped label is reported, never
//! silently lost) and starts painting it; `:discard` drops the edits and
//! shows the newer document, clean. Both are refused outside `Behind`
//! (there is no "newer" to move onto), and so are `edit`/`:bump`
//! (controller ruling 2026-09-14) — an edit made now would be keyed
//! against a grid `:rebase` is about to move away from underneath it.

use crate::commands::{self, BumpAxis, Command, KEY_DISPLAY_SEPARATOR};
use crate::core::{Draft, MatrixModel, PanelSpec, parse_cell};
use crate::delegate::MatrixDelegate;
use crate::header::{self, HeaderInputs, HeaderModel};
use geode_core::colour::readable_on;
use geode_core::document::split_key;
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::snapshot::Snapshot;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::shell::colours::{to_hsla, to_rgb};
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, Focusable as _, Hsla, IntoElement, SharedString, Window,
    div,
};
use gpui_component::input::InputState;
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme, v_flex};
use std::cell::Cell as StdCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many rows `ctrl+d`/`ctrl+u` step — `vimnav`'s own ±5, the same
/// fixed offset every list in this codebase uses, multiplied by the count
/// prefix rather than being viewport-relative.
const HALF_PAGE: isize = 5;

/// `/` over the row labels (spec §8.3). The vim jump model only: a
/// document's rows ARE its axis, in the desk's own order, so narrowing
/// them (`FindStyle::Fzf`) would hide rows a cell reference is counted
/// against — the panel moves its cursor instead, which is what
/// `find_match` is for.
pub struct FindState {
    /// Where the cursor was when `/` opened; `escape` returns here.
    origin: usize,
    /// The last committed query, for `n`/`N`.
    committed: Option<String>,
}

/// The two header tones a theme colour cannot be trusted to paint as
/// TEXT, floored to Part 2c's 3:1 readability ratio against the window
/// background (`geode_core::colour::readable_on`, lightness moved toward
/// the foreground, hue and chroma kept). The finding is 2c's own: a
/// theme's `warning` and `danger` are fills and tints, and on nine bundled
/// light themes `warning` reads under 2.3:1 as text — and
/// `warning_foreground`, which the first build painted `Warn` chips in
/// with no fill under them, is the BACKGROUND family (1.00:1 on twenty
/// themes: an invisible `3 edits`).
///
/// Memoised, not derived per frame: `readable_on` is a 16-step bisection
/// through OKLab, and PHILOSOPHY §6 forbids that per chip per frame.
/// `key` is EVERY colour `derive` reads and nothing else — the blotter's
/// theme-signature rule at the scale of four inputs — so a theme switch
/// recomputes on its first frame and every other frame is one compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FlooredTones {
    key: [Hsla; 4],
    pub(crate) warn: Hsla,
    pub(crate) error: Hsla,
}

impl FlooredTones {
    pub(crate) fn derive(theme: &Theme) -> Self {
        let (bg, fg) = (to_rgb(theme.background), to_rgb(theme.foreground));
        let floor = |c: Hsla| to_hsla(readable_on(to_rgb(c), bg, fg));
        Self {
            key: [
                theme.background,
                theme.foreground,
                theme.warning,
                theme.danger,
            ],
            warn: floor(theme.warning),
            error: floor(theme.danger),
        }
    }

    /// Re-derive only when one of the four inputs moved.
    fn refresh(&mut self, theme: &Theme) {
        let key = [
            theme.background,
            theme.foreground,
            theme.warning,
            theme.danger,
        ];
        if self.key != key {
            *self = Self::derive(theme);
        }
    }
}

/// What `edit` and `:bump` answer with nothing on screen (controller
/// ruling): an edit is keyed by a grid cell and recorded against the
/// generation that grid came from, so with neither there is nothing
/// honest to open an editor over.
const NO_DOCUMENT: &str = "no document to edit";

/// What a commit answers when the grid moved under the open editor — see
/// [`Editing::labels`] for how that happens and why it is refused.
const CELL_MOVED: &str = "the document changed under the edit — nothing was written";

/// What `edit` and `:bump` answer while the draft is `Behind` (controller
/// ruling 2026-09-14): an edit made now would be keyed against the BASE
/// generation's grid while a newer one already sits underneath it, and
/// `:rebase` would then map that parked value onto whatever cell the same
/// label resolves to in the newer document — a live edit and a restored
/// one are exactly the same risk here, so this checks the draft's state
/// and not how it got there. `:rebase`/`:discard` are the only doors
/// forward, and the notice names both.
const BEHIND_REFUSED: &str = "the draft is behind — :rebase or :discard first";

/// What `:rebase`/`:discard` answer outside `Behind` — there is no
/// "newer" document to move onto or fall back to.
const NOT_BEHIND: &str = "nothing to rebase — the draft is on the live document";

/// One cell a `:bump` writes: where it is, the labels that make the edit
/// portable across generations, and the value being added to — the shape
/// [`Draft::bump`] consumes.
type BumpCell = ((usize, usize), (String, String), f64);

/// The open cell editor (spec §8.6): the input the trader is typing into,
/// and which cell it belongs to.
struct Editing {
    /// Tile-owned, and PAINTED in the cell (see `render`): gpui installs a
    /// text-input handler only for a focused `Input` that has been drawn,
    /// so an editor kept off the element tree would take no characters at
    /// all.
    state: Entity<InputState>,
    /// The cell this editor was opened on, captured rather than read back
    /// off the cursor at commit time.
    cell: (usize, usize),
    /// That cell's labels when the editor opened.
    ///
    /// The grid can move underneath an open editor: a delivery lands while
    /// a trader is typing, a shorter generation clamps the cursor
    /// (`clamp_cursor`), and a commit that wrote to whatever the cursor now
    /// points at would file a typed number against a different term.
    /// `commit` compares these against the model's CURRENT pair for the
    /// same cell and refuses when they differ — one comparison, at the one
    /// moment the answer matters, rather than a cancel-on-delivery path
    /// (which `promote` could not take: it runs from the frame observer,
    /// where there is no `Window` to blur).
    labels: (SharedString, SharedString),
}

/// Which of the three yanks (spec §8.3) is being taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Yank {
    Cell,
    Row,
    Col,
}

pub struct MarketDataTile {
    id: TileId,
    spec: &'static PanelSpec,
    frame: Entity<Frame>,
    diagnostics: Entity<Diagnostics>,
    data: DataHandle,
    /// The document key, in the dataset's declared `key` order. The
    /// trader-facing word is "underlying" (`:underlying <value>`); the data
    /// tier's word is "key" (the field name and `DocumentParams.document_key`).
    /// `None` until `:underlying` names one — a fresh panel has no document
    /// to ask about, and guessing one would paint a document the trader never
    /// asked for.
    key: Option<Vec<String>>,
    tag: u64,
    /// The frame versions the last request was made under; `None` until
    /// the first.
    ///
    /// The WHOLE `FrameVersions`, even though only two of its counters
    /// decide a requery (`follows_changed` compares `as_of` and `data`
    /// and nothing else): the flip barrier is keyed by the flip identity
    /// — `(scope, grouping, as_of)` — so answering it
    /// (`Frame::arrived(key, acted)`) needs the versions the request was
    /// made under, not just the pair this panel follows. One field rather
    /// than two, because they are one fact: what the frame looked like
    /// when this panel last asked.
    acted: Option<FrameVersions>,
    visible: bool,
    /// The newest delivered snapshot. While the draft is `Behind` this is
    /// the newer generation the panel is NOT painting — `base_snapshot`
    /// is — kept so Task 8's `:rebase` has it in hand.
    snapshot: Option<Arc<Snapshot>>,
    /// The generation the draft's edits were made against, retained the
    /// moment a newer one arrives under them (roadmap ruling 9: a newer
    /// document never clobbers a draft). `None` whenever the panel is
    /// painting the newest snapshot — including a draft restored from a
    /// session, which lands `Behind` with no base generation ever having
    /// been received, and is honestly painted against the newest one
    /// until `:rebase`/`:discard` (Task 8).
    base_snapshot: Option<Arc<Snapshot>>,
    /// `Rc`, not a plain `MatrixModel`: the delegate paints from this on
    /// every frame — every shell repaint, not just this tile's own
    /// rebuilds — and a `MatrixModel` clone per paint would reallocate
    /// every row and bump every cell's `SharedString` (the diagnostics
    /// tile's own MAJ-4, same shape). Replaced wholesale by
    /// `rebuild_model` and never mutated in place; `install_model` is
    /// what hands the new `Rc` to the delegate.
    model: Rc<MatrixModel>,
    draft: Draft,
    /// A restored draft's edits are parked out of every grid's range
    /// (`Draft::from_toml`, which stores label pairs and not indices), so
    /// they paint nowhere until a model resolves them. Set at
    /// construction and cleared by the first delivery, which is the one
    /// that has a model to rebase against.
    ///
    /// `set_key` deliberately does not clear it: this being `true` implies
    /// a non-empty draft, and a key change with edits pending is refused
    /// outright (ruling 2026-09-14), so it can never be left set against
    /// a document its labels did not come from.
    unresolved_restore: bool,
    /// (row, column) into the model's grid — the same index a
    /// `Draft` edit is keyed by, and the truth the table's own selection
    /// mirrors (never the other way round).
    cursor: (usize, usize),
    /// The body: gpui-component's table over [`MatrixDelegate`]. Never
    /// focused (see `geode_marketdata::init`, which binds its context's
    /// keys to `NoAction` for the one frame a click gives it gpui focus).
    table: Entity<TableState<MatrixDelegate>>,
    /// `Some` while the cell editor holds the keyboard, which is the
    /// whole of what `mode == insert` means to the shell (spec §8.6).
    /// Opened by `marketdata::edit`, closed by `commit`/`cancel` through
    /// the one door [`Self::close_editor`] — blur, then drop.
    editor: Option<Editing>,
    find: Option<FindState>,
    /// The one line the header says about the last thing that went wrong
    /// or is not built yet. `SharedString` rather than `String`: `render`
    /// clones it, and a `String` clone is an allocation per frame.
    notice: Option<SharedString>,
    stale_after: Rc<StdCell<Duration>>,
    /// The prepared header, rebuilt by [`Self::rebuild_chrome`] — the one
    /// door every mutation on this tile ends at — so `render` clones
    /// refcounts and formats nothing.
    header: HeaderModel,
    /// The painted generation's source time, parsed once beside the
    /// header's own time text so the staleness rule is a comparison per
    /// frame rather than an RFC-3339 parse.
    source_at: Option<chrono::DateTime<chrono::Utc>>,
    /// An outcome that arrived while the flip barrier (Phase 4 §3.10)
    /// still wanted this panel's key — held here, NOT applied, until
    /// [`Self::promote`] puts it through [`Self::apply`] exactly as an
    /// un-barriered delivery would have been.
    ///
    /// Without this the panel would paint the new as-of — its grid, its
    /// header, its source-time chip — a frame ahead of every blotter,
    /// whose own heavier outcomes are still staged: the half-updated
    /// screen the barrier exists to prevent. A document select is cheap,
    /// which makes this panel the one most likely to win that race.
    ///
    /// Stamped with the versions it was delivered FOR: a second
    /// scope/as-of mutation inside the same 250 ms window replaces the
    /// barrier before this panel's own fresh requery lands, so a `flip`
    /// bump from the NEWER barrier must not promote a snapshot staged for
    /// the older one (the blotter's fix round 1, Finding 1, reached the
    /// same way). `requery` also clears it at its own top: a fresh
    /// question always supersedes whatever was staged before it.
    staged: Option<(Arc<Snapshot>, FrameVersions)>,
    /// `versions().flip` as of the last promotion — this panel's own half
    /// of the bump. Starts at `0` (the blotter's own seed): the first
    /// observer pass on a frame that has already flipped then calls
    /// `promote`, which is a no-op with nothing staged.
    last_flip: u64,
    /// The header's floored tone colours, refreshed at the top of `render`
    /// (see [`FlooredTones`]).
    tones: FlooredTones,
}

impl MarketDataTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TileId,
        spec: &'static PanelSpec,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        data: DataHandle,
        stale_after: Rc<StdCell<Duration>>,
        restored: Option<&toml::Table>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = restored
            .and_then(|t| t.get("underlying").or_else(|| t.get("key")))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .filter(|k| !k.is_empty());
        let draft = restored
            .and_then(|t| t.get("draft"))
            .and_then(|v| v.as_table())
            .map(Draft::from_toml)
            .unwrap_or_default();

        // The body. `row_selectable` so the cursor's row reads as the
        // blotter's does, `cell_selectable` because the panel's cursor is a
        // CELL and the component only reports which column was clicked in
        // that mode, `row_header(false)` so it adds no row-number column
        // of its own (the row labels are this panel's first column),
        // `col_selectable(false)`/`sortable(false)` because a document's
        // axes are the desk's own order and nothing here sorts them, and
        // `col_resizable(false)` because a dragged width has nowhere to
        // live and every `refresh` would undo it (see `LABEL_WIDTH` in
        // `delegate.rs`, which says it once for both halves).
        let table = cx.new(|cx| {
            TableState::new(MatrixDelegate::new(spec), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(true)
                .row_header(false)
                .loop_selection(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(false)
        });
        // The mouse's whole part in this panel: a click selects a cell.
        // The cursor moves to it, and that is all — a click on the
        // row-label column moves the row and leaves the column alone, and a
        // DOUBLE-click does exactly what the single click already did.
        //
        // **Editing is keyboard-only** (`i`/`enter`), by controller ruling
        // 2026-09-14: every tile mouse-down re-arms the shell's
        // `pending_focus_restore` (CLAUDE.md's focus rule), which the next
        // `ShellView::render` consumes by focusing the shell root — so an
        // editor opened from a mouse event would lose the keyboard on the
        // very next frame. Whether an occupant may deliberately keep focus
        // through that restore is a shell-side decision, deferred; until it
        // is made, this panel does not offer an affordance it cannot honour,
        // which is why `TableEvent::DoubleClickedCell` is not matched here.
        //
        // `SelectRow`/`SelectColumn` are deliberately not matched either:
        // `sync_cursor` emits both, so matching them would re-enter this
        // handler on every cursor move.
        //
        // `subscribe_in` (and so a `Window`) for the cancel below alone.
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            if let TableEvent::SelectCell(row, col) = event {
                // A click while the cell editor is open CANCELS it
                // (controller ruling 2026-09-14, review Minor 5) — through
                // `close_editor`, so blur then drop, and never a commit: a
                // click is not `enter`, and silently writing a half-typed
                // number because the trader clicked elsewhere is the one
                // outcome nobody asked for. Cancelling is not optional
                // either, because the same mouse-down has already re-armed
                // the shell's focus restore: left open, the editor would sit
                // painted on the cell the cursor just left, deaf to the
                // keyboard, with `mode == insert` still claimed.
                if this.editor.is_some() {
                    this.close_editor(window, cx);
                    // `cancel`'s own chrome step in `dispatch`: the header
                    // is re-prepared once, off the render thread.
                    this.changed(cx);
                }
                this.cursor_to(*row, MatrixDelegate::model_col(*col), cx)
            }
        })
        .detach();
        cx.observe(&frame, |this, frame, cx| {
            // A flip released (Phase 4 §3.10): promote whatever is staged,
            // and do it REGARDLESS of visibility — a panel hidden between
            // staging and the flip must not come back showing the old
            // generation. `flip` is checked here and never in
            // `follows_changed`: it means "you may promote", never
            // "requery" (CLAUDE.md).
            let now = frame.read(cx).versions();
            if now.flip != this.last_flip {
                this.last_flip = now.flip;
                this.promote(cx);
            }
            if !this.visible {
                return;
            }
            // Only `as_of` and `data` are followed (see the module doc);
            // a scope keystroke bumps `scope` on every character and must
            // not cost this panel a requery.
            if this.key.is_some() && this.follows_changed(now) {
                // The barrier is answered on delivery instead, with the
                // versions this request was made under.
                this.requery(cx);
            } else {
                this.self_arrive(now, cx);
            }
        })
        .detach();

        let mut this = MarketDataTile {
            id,
            spec,
            frame,
            diagnostics,
            data,
            model: Rc::new(MatrixModel::empty(spec, key.as_deref().unwrap_or(&[]))),
            unresolved_restore: !draft.is_empty(),
            key,
            tag: 0,
            acted: None,
            visible: false,
            snapshot: None,
            base_snapshot: None,
            draft,
            cursor: (0, 0),
            table,
            editor: None,
            find: None,
            notice: None,
            stale_after,
            header: HeaderModel {
                title: "".into(),
                underlying: None,
                dirty: false,
                attrs: Vec::new(),
                state: None,
                notice: None,
                time: None,
                stale: false,
            },
            source_at: None,
            staged: None,
            last_flip: 0,
            tones: FlooredTones::derive(cx.theme()),
        };
        this.rebuild_chrome();
        // The delegate starts with the model this tile starts with (review
        // Minor 3). Both are empty here, so nothing paints differently —
        // but "the delegate's model IS the tile's model" is an invariant
        // every other path maintains, and starting the two apart would
        // leave the one window in which it does not hold, for a future
        // constructor that seeds a model to fall through.
        this.install_model(cx);
        this
    }

    /// Pushed onto the keymap context stack while this tile is focused.
    /// `insert` exactly while the cell editor holds the keyboard — the
    /// shell's insert branch (spec §8.6) keys on this one pair, and
    /// `counts()` stays on in both modes deliberately: stopping a typed
    /// `3` from becoming a count prefix is the shell's job there, not
    /// this context's.
    pub fn key_context(&self) -> KeyContext {
        let mode = if self.editor.is_some() {
            "insert"
        } else {
            "normal"
        };
        KeyContext::new("marketdata").pair("mode", mode).counts()
    }

    // ---- the request -------------------------------------------------

    /// Whether the frame has moved in a way a document request depends
    /// on — `as_of` and `data`, never `scope`/`grouping`/`config`/`flip`
    /// (the module doc says why for each). `None` (nothing asked yet) is
    /// always a change.
    fn follows_changed(&self, now: FrameVersions) -> bool {
        let Some(acted) = self.acted else {
            return true;
        };
        Self::differs_on_followed(acted, now)
    }

    /// Whether `versions` and `now` disagree on any counter this panel
    /// follows. The one comparison [`Self::follows_changed`] and
    /// [`Self::promote`]'s own gate both go through, so "what this panel
    /// requeries for" and "what invalidates something it has already
    /// staged" can never drift apart (I-1, final whole-branch review).
    fn differs_on_followed(versions: FrameVersions, now: FrameVersions) -> bool {
        versions.as_of != now.as_of || versions.data != now.data
    }

    /// Answer an open flip barrier for a change this panel is NOT going
    /// to requery for (a scope or grouping bump, or no key to ask about).
    ///
    /// `ShellView::visible_tile_keys` cannot know which tiles follow
    /// which counters, so every visible occupant is in the barrier's key
    /// set (Phase 4 §3.10). Left unanswered, this panel would hold every
    /// blotter on screen open until `FLIP_DEADLINE` — 250 ms — on every
    /// scope keystroke, for a tile with nothing coming. The blotter's own
    /// `on_frame_changed` has this exact branch, for the exact same
    /// reason (a pinned tile under a grouping change).
    fn self_arrive(&mut self, now: FrameVersions, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        if self.frame.read(cx).barrier_wants(key, now) {
            self.frame.update(cx, |f, cx| {
                if f.arrived(key, now) {
                    cx.notify();
                }
            });
        }
    }

    /// Tell an open barrier this panel's own outcome has landed, with the
    /// versions the request was made under — a failed outcome counts too
    /// (the blotter's rule: one broken tile must never hold every other
    /// tile open until the deadline).
    fn arrive(&mut self, cx: &mut Context<Self>) {
        let _ = self.arrive_and_release(cx);
    }

    /// [`Self::arrive`], answering whether this arrival is what EMPTIED
    /// the barrier — the caller uses that to promote its own staged
    /// snapshot at once rather than waiting for the `flip` bump to reach
    /// its observer on a later notify pass.
    fn arrive_and_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(acted) = self.acted else {
            return false;
        };
        let key = QueryKey(self.id.0);
        self.frame.update(cx, |f, cx| {
            let released = f.arrived(key, acted);
            if released {
                cx.notify();
            }
            released
        })
    }

    /// Submit this panel's document request, keyed by the tile so two
    /// panels on one document never supersede each other.
    fn requery(&mut self, cx: &mut Context<Self>) {
        let Some(document_key) = self.key.clone() else {
            return;
        };
        // A fresh question always supersedes whatever was staged for the
        // old one, whether or not `promote`'s own version check would
        // have caught it.
        self.staged = None;
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions())
        };
        self.tag += 1;
        self.acted = Some(versions);
        let queued = self.data.document(DocumentParams {
            key: QueryKey(self.id.0),
            tag: self.tag,
            submitted: Instant::now(),
            dataset: self.spec.dataset.to_string(),
            document_key,
            as_of,
        });
        if !queued {
            self.notice = Some("document request refused: the data service is busy or gone".into());
            // A refused submit means nothing is coming (review fix round
            // 1, MIN-3): arrive, or an open barrier holds every other tile
            // to the 250 ms deadline waiting for an outcome that will
            // never exist — then clear `acted`, so the next frame change
            // retries rather than deciding this panel is already up to
            // date. In that order: `arrive` reads `acted`.
            self.arrive(cx);
            self.acted = None;
        }
        self.changed(cx);
    }

    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if outcome.tag != self.tag {
            // Stale: a newer request is out — and deliberately NOT an
            // arrival (the blotter drops one the same way). A barrier
            // waits for the versions this panel last ACTED under, which
            // is the newer request's; arriving here would answer for a
            // question that is still in flight, and that newer outcome's
            // own delivery is what answers it.
            return;
        }
        let acted = self.acted;
        match outcome.snapshot {
            Ok(snapshot) => {
                // The notice is cleared in `apply`, on the delivery that
                // PAINTS (M-5, final whole-branch review): clearing it
                // here wiped a `:rebase` dropped-edit report or a
                // `BEHIND_REFUSED` line on any unrelated publish — and
                // would wipe the restored-draft report `apply` itself
                // writes, on the very next `data` bump.
                //
                // Phase 4 §3.10 (review fix round 1, the Important): if a
                // barrier is open and still wants this key, STAGE rather
                // than apply. A document select is cheap, so this panel is
                // the one most likely to paint the new as-of — grid,
                // header and source-time chip — a frame before every
                // blotter promotes its own, which is exactly the
                // half-updated screen the barrier exists to prevent.
                let wants = acted.is_some_and(|acted| {
                    self.frame
                        .read(cx)
                        .barrier_wants(QueryKey(self.id.0), acted)
                });
                if wants {
                    let acted = acted.expect("`wants` is false without one");
                    self.staged = Some((snapshot, acted));
                    // `arrived` may empty the barrier right here — when it
                    // does, promote at once rather than waiting for the
                    // `flip` bump to come back round to this panel's own
                    // observer on a later notify pass.
                    if self.arrive_and_release(cx) {
                        self.promote(cx);
                    }
                } else {
                    self.apply(snapshot, cx);
                    self.arrive(cx);
                }
            }
            Err(e) => {
                // Last good stays on screen: a failed select says nothing
                // about the document already painted. It still counts as
                // an arrival — one broken tile must never hold every other
                // tile open until the deadline.
                self.notice = Some(e.into());
                self.arrive(cx);
            }
        }
        self.changed(cx);
    }

    /// Put a delivered snapshot on screen: the draft's own view of the
    /// generation, the model, the cursor and the scroll. Called by
    /// `deliver` for an un-barriered outcome and by [`Self::promote`] for
    /// a staged one, so the two paths cannot drift.
    fn apply(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let as_of = source_time_of(&snapshot);
        // Everything below is decided on a COPY of the draft and built
        // before a single field is committed (M-2, final whole-branch
        // review): a generation `MatrixModel::build` refuses must change
        // NOTHING but the notice. The last good model always stayed on
        // screen, but `self.snapshot` and the draft's own state used to
        // move anyway — so the panel went `Behind` against a generation it
        // never painted, and `:rebase` was left pointed at a document that
        // cannot be laid out as a grid.
        let mut draft = self.draft.clone();
        if let Some(as_of) = &as_of {
            draft.on_delivered(as_of);
        }
        // While `Behind`, keep painting the generation the edits were made
        // against; `snapshot` below still records the delivered one for
        // `:rebase` (Task 8). Retained only when the OUTGOING snapshot
        // really IS that base (M-1): a restored draft lands `Behind` with
        // its base never delivered at all, and pinning whatever happened
        // to be painted froze the panel on a generation that was neither
        // the base nor the newest, with the header naming a third.
        let retained = if draft.is_behind() {
            match &self.base_snapshot {
                Some(base) => Some(Arc::clone(base)),
                None => self
                    .snapshot
                    .clone()
                    .filter(|s| draft.base.is_some() && source_time_of(s) == draft.base),
            }
        } else {
            None
        };
        // The DELIVERED snapshot is what gets recorded, so it is what has
        // to build (M-2) — while `Behind` it is not the one painted, but
        // it is the one `:rebase` will be run against, and recording a
        // generation that cannot be laid out as a grid is how `:rebase`
        // came to fail against a document the panel never showed.
        let built = match MatrixModel::build(&snapshot, self.spec, &draft) {
            Ok(model) => model,
            Err(e) => {
                self.notice = Some(e.into());
                return;
            }
        };
        // With a base retained, the screen keeps the model it already has:
        // `self.model` is by construction the model of `painted_snapshot()`
        // under these very edits, and `on_delivered` moves only the
        // draft's STATE, which `MatrixModel::build` never reads (it reads
        // `edits` and `is_sent()`, and a delivery moves neither). Keeping
        // it is also what makes this one build per delivery rather than
        // two — the freshly built model above is a validation of the
        // delivered generation, not a paint.
        let model = match &retained {
            Some(_) => Rc::clone(&self.model),
            None => Rc::new(built),
        };

        // Committed from here down. The notice is cleared here rather than
        // in `deliver`'s `Ok` arm (M-5) — on the delivery that paints, and
        // ahead of every notice this method itself writes below.
        self.notice = None;
        self.draft = draft;
        self.base_snapshot = retained;
        self.snapshot = Some(snapshot);
        self.model = model;
        self.clamp_cursor();
        // A restored draft's edits have no grid position until a model
        // resolves them by label (Task 5's own note on `Draft::from_toml`)
        // — and only a NON-EMPTY, successfully built model can resolve
        // one (I-2's ruling, final whole-branch review). An empty snapshot
        // (no document for this key in this database, or a persisted
        // as-of that predates its first publish — `compile_document`'s
        // `and false` arm) and a refused build both leave the model empty,
        // and rebasing against one dropped every restored edit silently:
        // the one path on this branch that lost unsent work, which §8.5
        // says survives a restart. Until then the draft stays parked and
        // `rebuild_chrome` says so.
        if self.unresolved_restore && !self.model.rows.is_empty() {
            self.unresolved_restore = false;
            // A draft that landed `Behind` is not rebased here: moving
            // edits onto a generation the trader has not seen is exactly
            // the decision `:rebase` exists to ask for (§8.4).
            if !self.draft.is_behind() {
                let (_, dropped) = self.draft.rebase(&self.model);
                self.rebuild_model(cx);
                // Reported, never pruned in silence — `:rebase` names
                // every dropped pair on the same situation, and a reader
                // of §8.7.17 would expect the same disclosure here.
                // Written after the rebuild so a build failure's own
                // message wins by arriving first, exactly as `rebase`
                // orders its own two.
                if !dropped.is_empty() && self.notice.is_none() {
                    self.notice = Some(dropped_notice(&dropped).into());
                }
            }
        }
        // This method assigns `self.model` itself (the retained-base branch
        // deliberately keeps the model it already had), so the delivery's
        // own hand-off to the table happens here — and, being
        // `install_model`, it refreshes: a new generation can carry a
        // different node ladder, and the table paints its header from the
        // column groups `refresh` rebuilds.
        self.install_model(cx);
    }

    /// Apply a staged snapshot, if any — from the `flip` bump in the frame
    /// observer, or from this panel's own `deliver` when its arrival was
    /// the one that emptied the barrier. A no-op with nothing staged, so
    /// calling it on every `flip` costs nothing.
    ///
    /// The gate asks "does this still answer what I FOLLOW", never "is
    /// the barrier's identity unchanged" (I-1, final whole-branch
    /// review). `requery` and `set_key` both clear `staged`, so a staged
    /// snapshot is by construction the answer to this panel's latest
    /// question; the flip identity (`scope`, `grouping`, `as_of`) is the
    /// blotter's rule, and the blotter can afford it only because it
    /// follows scope and grouping. This panel follows neither — a barrier
    /// replaced by a scope keystroke or a grouping step comes with no
    /// requery at all, so the snapshot thrown away here was the only
    /// answer the panel would ever get for the new as-of, and what stayed
    /// painted was the PRE-as-of generation under the window-wide
    /// historical stripe, with `acted` claiming the panel was current.
    ///
    /// A stage IS dropped when a counter this panel follows has moved
    /// under it — reachable while hidden, where no requery replaces it —
    /// because it answers a question nobody is asking any more.
    fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((snapshot, versions)) = self.staged.take() else {
            return;
        };
        if !Self::differs_on_followed(versions, self.frame.read(cx).versions()) {
            self.apply(snapshot, cx);
            self.changed(cx);
        }
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            // The catalog is where `:key`'s completions come from, and
            // nothing else asks for one on this panel's behalf.
            self.request_catalog_if_needed(cx);
            let now = self.frame.read(cx).versions();
            if self.key.is_some() && self.follows_changed(now) {
                self.requery(cx);
            }
        } else {
            // An in-flight document nothing will paint is a round trip
            // spent for nothing.
            self.data.cancel(QueryKey(self.id.0));
            // And the cancelled request's own `acted` must go with it
            // (review fix round 1, MIN-2): it records "this panel has
            // already asked under these versions", which is no longer
            // true of anything that will arrive — left set, a panel hidden
            // mid-round-trip comes back and decides it is up to date, and
            // paints the generation it had before it was hidden until the
            // next publish happens along.
            self.acted = None;
        }
        self.changed(cx);
    }

    // ---- the model ---------------------------------------------------

    /// The snapshot on screen: the draft's own base generation while one
    /// is retained, else the newest delivered.
    fn painted_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.base_snapshot.clone().or_else(|| self.snapshot.clone())
    }

    /// Rebuild the prepared grid. Called on a delivery and on a draft
    /// change — never from `render`.
    fn rebuild_model(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.painted_snapshot() else {
            self.model = Rc::new(MatrixModel::empty(
                self.spec,
                self.key.as_deref().unwrap_or(&[]),
            ));
            self.clamp_cursor();
            self.install_model(cx);
            return;
        };
        match MatrixModel::build(&snapshot, self.spec, &self.draft) {
            Ok(model) => self.model = Rc::new(model),
            // A document that cannot be laid out as a grid (a hole, a
            // repeated pair, a missing axis) leaves the last good model
            // on screen and says what it was — never a half-built grid.
            Err(e) => self.notice = Some(e.into()),
        }
        self.clamp_cursor();
        self.install_model(cx);
    }

    /// Hand the current model to the delegate and refresh the table.
    ///
    /// **Every model swap ends here**, and the `refresh` is the reason: the
    /// pinned gpui-component caches each `column()`'s answer in
    /// `col_groups` at prepare time and paints its HEADER from that cache
    /// alone, so a document whose node ladder changed would keep the
    /// previous one's headers (and lay its cells out at the previous
    /// widths) until something else happened to refresh. The same trap
    /// CLAUDE.md records for the blotter's gutter.
    ///
    /// One `Rc::clone` — a refcount — never the model itself.
    fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.refresh(cx);
        });
        self.sync_cursor(cx);
    }

    /// Keep the cursor inside the grid — a new generation can be shorter
    /// than the one it replaces, and a cursor left past its end would
    /// yank, edit and paint nothing.
    fn clamp_cursor(&mut self) {
        self.cursor.0 = self.cursor.0.min(self.model.rows.len().saturating_sub(1));
        self.cursor.1 = self
            .cursor
            .1
            .min(self.model.columns.len().saturating_sub(1));
    }

    /// Mirror the cursor and the open editor into the delegate, and move
    /// the table's own selection to match — which is also what keeps the
    /// cursor row and column in view (`set_selected_row` and
    /// `set_selected_col` each scroll, non-strictly, so a cell already on
    /// screen never jumps).
    ///
    /// The column is shifted by one: the table's column 0 is the row-label
    /// column, which the cursor never enters. The column is set BEFORE the
    /// row deliberately — each setter switches the component's selection
    /// mode, and the row highlight is painted only in row mode, so ending
    /// on the row is what makes the panel read like the blotter (a
    /// highlighted row plus a bordered cursor cell) rather than painting
    /// nothing at all.
    fn sync_cursor(&self, cx: &mut Context<Self>) {
        let (row, col) = self.cursor;
        let editor = self.editor.as_ref().map(|e| (e.cell, e.state.clone()));
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.cursor = (row, col);
            d.editor = editor;
            t.set_selected_col(MatrixDelegate::table_col(col), cx);
            t.set_selected_row(row, cx);
            t.scroll_to_row(row, cx);
        });
    }

    /// Move the cursor to a clicked cell — the mouse's form of §8.3's
    /// motions, clamped into the grid. `col: None` is a click on the
    /// row-label column: the row moves and the column stays, since the
    /// cursor never enters that column.
    fn cursor_to(&mut self, row: usize, col: Option<usize>, cx: &mut Context<Self>) {
        if self.model.rows.is_empty() {
            return;
        }
        self.cursor.0 = row.min(self.model.rows.len().saturating_sub(1));
        if let Some(col) = col {
            self.cursor.1 = col.min(self.model.columns.len().saturating_sub(1));
        }
        self.sync_cursor(cx);
        cx.notify();
    }

    /// The one door every mutation ends at: re-prepare the header (which
    /// formats, and so must never happen in `render`) and notify.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_chrome();
        cx.notify();
    }

    /// Prepare the header (spec §4): the kind badge and underlying, the
    /// dirty dot, each header attribute, the one short state run, the
    /// notice, and the generation's source time.
    fn rebuild_chrome(&mut self) {
        self.source_at = self
            .model
            .source_time
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        self.header = HeaderModel::prepare(HeaderInputs {
            spec: self.spec,
            key: self.key.as_deref(),
            model: &self.model,
            badge: self.draft.badge(),
            unresolved_restore: self.unresolved_restore,
            notice: self.notice.as_ref(),
            source_at: self.source_at,
        });
    }

    /// Whether the painted generation is old enough to warrant the
    /// header's stale marker — the blotter's own rule (spec §6.5) at this
    /// panel's configured `stale_after`.
    fn is_stale(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.source_at.is_some_and(|at| {
            now.signed_duration_since(at).to_std().unwrap_or_default() > self.stale_after.get()
        })
    }

    // ---- keys --------------------------------------------------------

    /// `window` is here for the cell editor alone: creating an
    /// `InputState`, giving it the keyboard and giving the keyboard back up
    /// all need one (spec §8.6). Nothing else in this match touches it.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(verb) = action.0.strip_prefix("marketdata::") else {
            return false;
        };
        let n = count.unwrap_or(1).max(1) as isize;
        // Whether this action touched something the HEADER paints (review
        // fix round 1, MIN-5). The cursor is not in the header, so a
        // motion, a yank and an `n`/`N` step must not re-prepare the
        // chips — `changed` formats, and a held `j` would then format the
        // whole header per keystroke for a row number nothing shows.
        let chrome = match verb {
            "down" | "up" | "left" | "right" | "page_down" | "page_up" | "top" | "bottom"
            | "first_col" | "last_col" => {
                let (rows, cols) = match verb {
                    "down" => (n, 0),
                    "up" => (-n, 0),
                    "left" => (0, -n),
                    "right" => (0, n),
                    "page_down" => (HALF_PAGE * n, 0),
                    "page_up" => (-HALF_PAGE * n, 0),
                    "top" => (isize::MIN / 2, 0),
                    "bottom" => (isize::MAX / 2, 0),
                    "first_col" => (0, isize::MIN / 2),
                    _ => (0, isize::MAX / 2),
                };
                self.move_cursor(rows, cols);
                false
            }
            "yank" | "yank_row" | "yank_col" => {
                let what = match verb {
                    "yank" => Yank::Cell,
                    "yank_row" => Yank::Row,
                    _ => Yank::Col,
                };
                if let Some(text) = self.yank_text(what) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                false
            }
            "edit" => {
                self.begin_edit(window, cx);
                true
            }
            "commit" => self.commit_edit(window, cx),
            "cancel" => {
                // Only when there WAS an editor: `marketdata::cancel` is
                // bound in insert mode alone, so a normal-mode arrival is
                // the palette's, and it has nothing to say.
                match self.editor.is_some() {
                    true => {
                        self.close_editor(window, cx);
                        true
                    }
                    false => false,
                }
            }
            "find_next" | "find_prev" => {
                let dir = if verb == "find_next" {
                    FindDirection::Forward
                } else {
                    FindDirection::Backward
                };
                self.repeat_find(dir, count);
                false
            }
            "escape" => {
                self.find = None;
                // Only when there WAS one: `escape` on a clean header
                // changes nothing the chips show.
                self.notice.take().is_some()
            }
            _ => return false,
        };
        self.sync_cursor(cx);
        if chrome {
            self.rebuild_chrome();
        }
        cx.notify();
        true
    }

    // ---- the cell editor ---------------------------------------------

    /// The generation an edit is recorded against, or why there can be no
    /// edit at all.
    ///
    /// An absent `source_time` gives an EMPTY base rather than a refusal:
    /// `compile_document` always stamps one (Part 1 §4.5 — a document
    /// request reports its own resolved generation's source time), so this
    /// is unreachable from the real query path, and refusing here would
    /// turn a provenance gap into a panel a trader cannot type into at all.
    /// An empty base simply never matches a delivered `as_of`, so such a
    /// draft reads `Behind` on the next delivery — visible and
    /// recoverable, never a silently restamped edit.
    fn edit_base(&self) -> Result<String, String> {
        if self.model.rows.is_empty() || self.model.columns.is_empty() {
            return Err(NO_DOCUMENT.to_string());
        }
        Ok(self.model.source_time.clone().unwrap_or_default())
    }

    /// `marketdata::edit` (`i`/`enter`): open an input in the cursor cell,
    /// seeded with what that cell already reads — the draft's own value
    /// where one has been made, since that is what `MatrixModel::build`
    /// painted there — and give it the keyboard.
    ///
    /// Every painted cell is editable, in both pivots: `Columns::Axis`
    /// fills the grid from the document's one value column, and
    /// `Columns::Values` lays out the value columns and nothing else, so
    /// there is no attribute cell for a refusal to be about. A NULL cell is
    /// editable on purpose — filling a hole the desk left is an edit like
    /// any other, and it opens on the empty text it paints.
    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            // Already editing. `i` is not bound in insert mode, so this is
            // the palette's route in, and re-seeding would throw away what
            // the trader has typed.
            return;
        }
        if self.draft.is_behind() {
            self.notice = Some(BEHIND_REFUSED.into());
            return;
        }
        if let Err(e) = self.edit_base() {
            self.notice = Some(e.into());
            return;
        }
        let cell = self.cursor;
        let text = self.model.rows[cell.0].cells[cell.1].text.clone();
        let state = cx.new(|cx| InputState::new(window, cx));
        state.update(cx, |s, cx| s.set_value(text, window, cx));
        state.read(cx).focus_handle(cx).focus(window, cx);
        self.editor = Some(Editing {
            state,
            cell,
            labels: self.model.label_of(cell),
        });
        self.notice = None;
    }

    /// `marketdata::commit` (`enter` in insert mode). Answers whether the
    /// header needs re-preparing.
    ///
    /// The text is PARSED before anything is written, through the column's
    /// declared type ([`PanelSpec::value_type`]) — a `'wide'` refused
    /// inline, with the editor left open and focused, because retyping a
    /// value is one keystroke away where dropping the editor would throw
    /// the whole line back at the trader.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editor.as_ref() else {
            return false;
        };
        let text = editing.state.read(cx).value().to_string();
        let cell = editing.cell;
        let labels = editing.labels.clone();

        if self.model.label_of(cell) != labels {
            // The grid moved under the editor (see `Editing::labels`).
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        let value = match parse_cell(&text, self.spec.value_type) {
            Ok(value) => value,
            Err(e) => {
                // Stay in insert mode, with the text as typed.
                self.notice = Some(e.into());
                return true;
            }
        };
        let base = match self.edit_base() {
            Ok(base) => base,
            Err(e) => {
                self.close_editor(window, cx);
                self.notice = Some(e.into());
                return true;
            }
        };
        self.draft.set(
            cell,
            (labels.0.to_string(), labels.1.to_string()),
            value,
            &base,
        );
        self.close_editor(window, cx);
        self.notice = None;
        // The draft's values are what `MatrixModel::build` paints, so an
        // edit that does not rebuild is an edit nobody can see.
        self.rebuild_model(cx);
        true
    }

    /// Give the keyboard up, then drop the editor — in that order, and
    /// BOTH halves (Task 4's own note in `geode_shell::module::recording`,
    /// verified at the pinned gpui-component rev):
    ///
    /// `blur` is what the shell's dropped-focus net (`render`'s
    /// `focused(cx).is_none()`) is waiting for, and dropping the entity is
    /// NOT enough to produce it — `Root` registers the focused input as a
    /// strong `AnyInputState` (`input::state::sync_focused_input_registry`)
    /// and only ever unregisters it from the `Input`'s own render, which an
    /// input removed from the tree never reaches. So the last clone would
    /// outlive this call, `Window::focused` would stay `Some`, and the net
    /// could never fire — leaving every chord dead for the rest of the
    /// session. Blurring is a module GIVING UP focus, never taking the
    /// shell's: no module touches the shell's own handle (CLAUDE.md's focus
    /// rule), and the shell decides where focus lands next.
    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.blur(cx);
        self.editor = None;
    }

    /// `:revert` (spec §8.4) — drop every edit. The document's own numbers
    /// are back on the same keystroke, so the only thing worth reporting is
    /// the nothing-to-do case.
    ///
    /// **Controller ruling 2026-09-14:** a draft emptied by `:revert` while
    /// `Behind` is `Clean` afterwards — `Draft::revert`'s own doing — and
    /// `Clean` means "on the live document". `leave_behind()` makes that
    /// true of the PANEL too: without it, `base_snapshot` stayed set with
    /// no draft left to explain it, the panel kept painting a generation
    /// the header no longer said anything about, and `:rebase`/`:discard`
    /// were both refused (there is no draft to move or drop) — the only
    /// way out was a `:key` retype or waiting for the next delivery.
    fn revert(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft.is_empty() {
            return Err("no edits to revert".to_string());
        }
        self.draft.revert();
        self.leave_behind();
        self.rebuild_model(cx);
        self.changed(cx);
        Ok(())
    }

    /// Drop whatever generation was retained under `Behind` so the next
    /// [`Self::rebuild_model`] paints the newest delivered one instead.
    ///
    /// The one door both `:discard` and a `:revert` that empties a
    /// `Behind` draft leave through — harmless to call when the draft was
    /// never `Behind` (`base_snapshot` is already `None`), which is why
    /// `revert` above calls it unconditionally rather than guarding on
    /// `is_behind()` first.
    fn leave_behind(&mut self) {
        self.base_snapshot = None;
    }

    /// `:bump <delta> [row|col]` — add `delta` to every cell along the
    /// cursor's ROW by default (a term's whole node ladder is the shape a
    /// trader nudges) or down its column on request.
    ///
    /// Each cell's CURRENT painted value is what is added to, which is the
    /// draft's own value wherever one exists, so two bumps compose instead
    /// of the second reading through to the document underneath
    /// (`Draft::bump`'s own contract). A NULL cell is skipped: there is no
    /// number to add to, and inventing one would put a value on screen the
    /// document never carried.
    fn bump(&mut self, delta: f64, axis: BumpAxis, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft.is_behind() {
            return Err(BEHIND_REFUSED.to_string());
        }
        let base = self.edit_base()?;
        let (row, col) = self.cursor;
        let values: Vec<((usize, usize), f64)> = match axis {
            BumpAxis::Row => self.model.rows[row]
                .cells
                .iter()
                .enumerate()
                .filter_map(|(ci, cell)| cell.value.map(|v| ((row, ci), v)))
                .collect(),
            BumpAxis::Col => self
                .model
                .rows
                .iter()
                .enumerate()
                .filter_map(|(ri, r)| {
                    r.cells
                        .get(col)
                        .and_then(|cell| cell.value)
                        .map(|v| ((ri, col), v))
                })
                .collect(),
        };
        if values.is_empty() {
            return Err("no values to bump".to_string());
        }
        // Collected rather than handed to `Draft::bump` as a lazy iterator:
        // the labels come off `self.model` while the draft is borrowed
        // mutably, which the borrow checker refuses — and one `Vec` per
        // `:bump` line is a keystroke's worth of work, not a per-frame one.
        let cells: Vec<BumpCell> = values
            .into_iter()
            .map(|(cell, value)| {
                let labels = self.model.label_of(cell);
                (cell, (labels.0.to_string(), labels.1.to_string()), value)
            })
            .collect();
        self.draft.bump(cells.into_iter(), delta, &base);
        self.rebuild_model(cx);
        self.changed(cx);
        Ok(())
    }

    /// `:rebase` (spec §8.4): move every edit onto the newer document,
    /// by label, and start painting it.
    ///
    /// `self.snapshot` is the newer generation — while `Behind`,
    /// `base_snapshot` is what is on screen and `snapshot` is what just
    /// arrived (see the module doc and the two fields' own docs). Built
    /// against an EMPTY draft, deliberately: `Draft::rebase` reads only a
    /// model's row/column labels and its source time, never a cell's
    /// painted value, so the draft about to be replaced has nothing to
    /// contribute here and using it would only invite confusion about
    /// which draft a reader is looking at.
    fn rebase(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.draft.is_behind() {
            return Err(NOT_BEHIND.to_string());
        }
        // Not `.expect(..)`: `Behind` implies a newer generation was
        // delivered, but this is a render-thread module, and an invariant
        // break here must read as a `:`-line refusal, never a crash.
        let snapshot = self
            .snapshot
            .clone()
            .ok_or_else(|| NOT_BEHIND.to_string())?;
        let newer_model = MatrixModel::build(&snapshot, self.spec, &Draft::default())?;
        let (_, dropped) = self.draft.rebase(&newer_model);
        self.leave_behind();
        // Cleared before the rebuild so the check below can tell "this
        // rebuild set its own error" from "something else was already
        // showing" — `rebuild_model` only ever WRITES `notice` on a build
        // failure, never clears it on success.
        self.notice = None;
        self.rebuild_model(cx);
        // A build failure against the newer document means paint that did
        // not happen, which matters more than a report about edits that
        // did land — it wins over the dropped-edit message when both would
        // apply, by simply arriving second and this check yielding to it.
        if self.notice.is_none() && !dropped.is_empty() {
            self.notice = Some(dropped_notice(&dropped).into());
        }
        self.changed(cx);
        Ok(())
    }

    /// `:discard` (spec §8.4): drop the edits outright and show the newer
    /// document — a trader saying "show me the new document" rather than
    /// "move my numbers onto it".
    fn discard(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.draft.is_behind() {
            return Err(NOT_BEHIND.to_string());
        }
        self.draft.discard();
        self.leave_behind();
        self.rebuild_model(cx);
        // `rebuild_model` only ever WRITES `notice` on a build failure, so
        // one already wins by being left in place. On success, clear only
        // the BEHIND-refusal notice — the one thing this verb is itself
        // the escape from — and leave anything else (a delivery error,
        // say) alone: it has nothing to do with discarding a draft.
        if self.notice.as_deref() == Some(BEHIND_REFUSED) {
            self.notice = None;
        }
        self.changed(cx);
        Ok(())
    }

    /// Move by cells, clamped. The extremes are the same door with a
    /// saturating delta — `isize::MAX / 2` cannot overflow when added to
    /// any real index, and one clamp is one rule about where a cursor may
    /// be.
    fn move_cursor(&mut self, rows: isize, cols: isize) {
        let (nrows, ncols) = (self.model.rows.len(), self.model.columns.len());
        if nrows == 0 || ncols == 0 {
            self.cursor = (0, 0);
            return;
        }
        self.cursor.0 = (self.cursor.0 as isize)
            .saturating_add(rows)
            .clamp(0, nrows as isize - 1) as usize;
        self.cursor.1 = (self.cursor.1 as isize)
            .saturating_add(cols)
            .clamp(0, ncols as isize - 1) as usize;
    }

    /// The blotter's own tab-separated spelling (§8.3): a cell is its
    /// text, a row is its label then its cells, a column is its cells one
    /// per line. Always the PREPARED text, so what is yanked is exactly
    /// what is on screen — a NULL yanks as nothing, never as `0.0000`.
    fn yank_text(&self, what: Yank) -> Option<String> {
        let (r, c) = self.cursor;
        let row = self.model.rows.get(r)?;
        Some(match what {
            Yank::Cell => row.cells.get(c)?.text.to_string(),
            Yank::Row => {
                let mut out = row.label.to_string();
                for cell in &row.cells {
                    out.push('\t');
                    out.push_str(&cell.text);
                }
                out
            }
            Yank::Col => self
                .model
                .rows
                .iter()
                .filter_map(|row| row.cells.get(c))
                .map(|cell| cell.text.as_ref())
                .collect::<Vec<_>>()
                .join("\n"),
        })
    }

    fn row_labels(&self) -> Vec<String> {
        self.model
            .rows
            .iter()
            .map(|r| r.label.to_string())
            .collect()
    }

    pub fn find(&mut self, event: FindEvent, cx: &mut Context<Self>) {
        match event {
            FindEvent::Changed(query) => {
                let origin = match &self.find {
                    Some(find) => find.origin,
                    None => {
                        self.find = Some(FindState {
                            origin: self.cursor.0,
                            committed: None,
                        });
                        self.cursor.0
                    }
                };
                let labels = self.row_labels();
                // Every keystroke searches from the ORIGIN, not from
                // wherever the previous one landed: that is what makes a
                // lengthening query walk forward and a shortened one walk
                // back (vim's incsearch).
                if let Some(row) = find_match(&labels, origin, FindDirection::Forward, &query) {
                    self.cursor.0 = row;
                    self.clamp_cursor();
                }
            }
            FindEvent::Committed(query) => {
                if let Some(find) = self.find.as_mut()
                    && !query.is_empty()
                {
                    find.committed = Some(query);
                }
            }
            FindEvent::Cancelled => {
                if let Some(find) = self.find.take() {
                    self.cursor.0 = find.origin;
                    self.clamp_cursor();
                }
            }
        }
        self.sync_cursor(cx);
        // A plain notify, for MIN-5's reason at this site too: `/` moves
        // the cursor and nothing else, and it does so on every keystroke
        // of the query — re-preparing the header there would format the
        // whole thing per character for something no chip shows.
        cx.notify();
    }

    /// `n`/`N`, counted — the committed query stepped from the cursor,
    /// wrapping, exactly as the blotter's own repeat does.
    fn repeat_find(&mut self, dir: FindDirection, count: Option<u32>) {
        let Some(query) = self.find.as_ref().and_then(|f| f.committed.clone()) else {
            return;
        };
        let labels = self.row_labels();
        if labels.is_empty() {
            return;
        }
        let mut at = self.cursor.0;
        for _ in 0..count.unwrap_or(1).max(1) {
            let start = match dir {
                FindDirection::Forward => (at + 1) % labels.len(),
                FindDirection::Backward => (at + labels.len() - 1) % labels.len(),
            };
            match find_match(&labels, start, dir, &query) {
                Some(row) => at = row,
                None => return,
            }
        }
        self.cursor.0 = at;
        self.clamp_cursor();
    }

    // ---- the `:` line ------------------------------------------------

    pub fn command(&mut self, line: &str, cx: &mut Context<Self>) -> Result<(), String> {
        // `completions` takes `&App` and can queue nothing, so this is
        // the door an `underlying` line's own catalog request rides — the
        // next completion list is then the fresh one (spec §8.3's
        // "completions from the catalog's keys").
        //
        // UNCONDITIONALLY, not `request_catalog_if_needed` (review fix
        // round 1, MIN-4, controller ruling): a held catalog listing this
        // dataset is not a FRESH one, and documents arrive while the panel
        // is open — a subscribed feed publishes a new key every few
        // seconds. Gated on staleness, the panel asked once and then
        // offered a completion list that could never grow. `set_visible`'s
        // own request stays gated: there, one catalog is as good as
        // another and the point is only to have one at all.
        if matches!(line.split_whitespace().next(), Some("underlying" | "key")) {
            self.request_catalog(cx);
        }
        match commands::parse(line)? {
            Command::Key(key) => self.set_key(key, cx),
            Command::Revert => self.revert(cx),
            Command::Bump { delta, axis } => self.bump(delta, axis, cx),
            Command::Rebase => self.rebase(cx),
            Command::Discard => self.discard(cx),
            // Parsed, not executed: the grammar a trader types is the one
            // Part 4 wires up.
            Command::Upload => Err("upload is not built yet".into()),
        }
    }

    /// Point the panel at another document.
    ///
    /// **Refused while the draft has edits** (ruling 2026-09-14), never
    /// discarding them: a draft's cells are grid indices into the
    /// document they were made on, so carrying them across would paint
    /// one document's numbers onto another's ladder — and dropping them
    /// silently would throw unsent work away on a keystroke that reads
    /// like navigation. The notice names the count in the header's own
    /// spelling and the verb that clears it.
    fn set_key(&mut self, key: Vec<String>, cx: &mut Context<Self>) -> Result<(), String> {
        if self.key.as_deref() == Some(key.as_slice()) {
            return Ok(());
        }
        if !self.draft.is_empty() {
            return Err(format!(
                "{} pending — :revert first",
                self.draft.count_phrase()
            ));
        }
        self.key = Some(key);
        self.snapshot = None;
        self.base_snapshot = None;
        // Including anything STAGED for the old key: a key change bumps no
        // frame version, so `promote`'s own flip-identity check would
        // happily put the previous document's grid on screen under the new
        // key's header. (`requery` below clears it too, but only on the
        // visible path — a hidden panel would otherwise carry it.)
        self.staged = None;
        // A different document is a different question: the next
        // delivery is never the one already asked for.
        self.acted = None;
        self.cursor = (0, 0);
        self.rebuild_model(cx);
        if self.visible {
            self.requery(cx);
        } else {
            self.changed(cx);
        }
        Ok(())
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        commands::completions(line, cursor, &self.catalog_keys(cx), self.draft.is_behind())
    }

    /// This panel's dataset's document keys, as the catalog holds them:
    /// one partition per document (Part 1 publishes a document under its
    /// joined key as the `batch`), spelled back with the typeable
    /// separator.
    fn catalog_keys(&self, cx: &App) -> Vec<String> {
        let diagnostics = self.diagnostics.read(cx);
        let Some(dataset) = diagnostics
            .catalog
            .as_ref()
            .and_then(|c| c.datasets.iter().find(|d| d.name == self.spec.dataset))
        else {
            return Vec::new();
        };
        let mut keys: Vec<String> = dataset
            .partitions
            .iter()
            .map(|p| display_key(&split_key(&p.batch)))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Whether the held catalog can answer this panel's key completions at
    /// all. A catalog with no entry for this dataset is as good as none:
    /// the dataset exists, so the answer is missing, not empty.
    ///
    /// This is a "have I got one" test, never a "is mine current" one —
    /// nothing in a `CatalogSnapshot` could answer the second (review fix
    /// round 1, MIN-4), which is why the `:key` line asks unconditionally
    /// and only `set_visible` consults this.
    fn needs_catalog(&self, cx: &App) -> bool {
        self.diagnostics
            .read(cx)
            .catalog
            .as_ref()
            .is_none_or(|c| !c.datasets.iter().any(|d| d.name == self.spec.dataset))
    }

    /// Ask the bridge's drain for a fresh catalog.
    ///
    /// `cx.notify()` in the SAME update block is mandatory, not tidy:
    /// `Diagnostics::request_catalog` queues the request and deliberately
    /// bumps no version, and the bridge's drain never runs at all without
    /// a notify to wake it — the trap CLAUDE.md names.
    fn request_catalog(&self, cx: &mut Context<Self>) {
        self.diagnostics.update(cx, |d, cx| {
            d.request_catalog();
            cx.notify();
        });
    }

    /// [`Self::request_catalog`], but only when this panel has no catalog
    /// for its dataset at all — `set_visible(true)`'s door, where the
    /// point is to HAVE one rather than to have the newest.
    fn request_catalog_if_needed(&self, cx: &mut Context<Self>) {
        if !self.needs_catalog(cx) {
            return;
        }
        self.request_catalog(cx);
    }

    pub fn serialize(&self) -> toml::Table {
        let mut t = toml::Table::new();
        if let Some(key) = &self.key {
            t.insert(
                "underlying".into(),
                toml::Value::Array(key.iter().map(|s| toml::Value::String(s.clone())).collect()),
            );
        }
        // Unsent edits are work and survive a restart (spec §8.5), as
        // label pairs — never indices, so a restart onto a newer
        // generation lands `Behind` instead of against misaligned cells.
        if !self.draft.is_empty() {
            t.insert("draft".into(), toml::Value::Table(self.draft.to_toml()));
        }
        t
    }

    // ---- test accessors ---------------------------------------------

    #[cfg(test)]
    pub(crate) fn model(&self) -> &MatrixModel {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn draft(&self) -> &Draft {
        &self.draft
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Whether this panel considers itself to have an outstanding
    /// question — `false` is what makes the next frame change a real
    /// retry (the refusal and hidden-mid-flight rules).
    #[cfg(test)]
    pub(crate) fn acted_is_none(&self) -> bool {
        self.acted.is_none()
    }

    /// The open editor's own entity — a test seeds a value through it,
    /// which is the one thing it cannot do with a key press.
    #[cfg(test)]
    pub(crate) fn editor_state(&self) -> Option<Entity<InputState>> {
        self.editor.as_ref().map(|e| e.state.clone())
    }

    /// What the open editor holds, `None` when none is open.
    #[cfg(test)]
    pub(crate) fn editor_value(&self, cx: &App) -> Option<String> {
        self.editor
            .as_ref()
            .map(|e| e.state.read(cx).value().to_string())
    }

    #[cfg(test)]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The header exactly as painted, in order — the prepared strings, so
    /// a test asserts on what a trader reads rather than on the fields
    /// behind it. Staleness is applied here, as `render` applies it,
    /// since [`HeaderModel::prepare`] never reads the clock.
    #[cfg(test)]
    pub(crate) fn header_texts(&self) -> Vec<String> {
        let mut h = self.header.clone();
        h.stale = self.is_stale(chrono::Utc::now());
        h.texts()
    }

    /// Whether the draft has any edit — the dirty dot the header paints
    /// beside the underlying, rather than a text run a test can grep for.
    #[cfg(test)]
    pub(crate) fn header_dirty(&self) -> bool {
        self.header.dirty
    }

    /// The table this panel's body is — what a test reads the painted
    /// columns and the mirrored cursor off (`selected_row`/`selected_col`),
    /// exactly as the blotter's own tests read theirs.
    #[cfg(test)]
    pub(crate) fn table(&self) -> &Entity<TableState<MatrixDelegate>> {
        &self.table
    }
}

/// A document key in the panel's own typeable spelling (`SPX.Z`,
/// `SPX.Z/EOD`) — the storage separator is unprintable, so it is never
/// what a trader sees or types.
pub(crate) fn display_key(key: &[String]) -> String {
    key.join(&KEY_DISPLAY_SEPARATOR.to_string())
}

/// `:rebase`'s notice about the edits it could not carry over — a row or
/// column label the newer document no longer has. Each pair is spelled
/// `row/col` (the same display separator a document key uses), since a
/// bare pair of labels with nothing between them reads as one run-on word.
/// A delivered document's own source time — the identity a [`Draft`]
/// compares its `base` against (§8.4: per-document `as_of`, never the
/// dataset-wide `gen_id` a live query's provenance carries).
fn source_time_of(snapshot: &Snapshot) -> Option<String> {
    snapshot
        .provenance()
        .datasets
        .first()
        .and_then(|f| f.as_of.clone())
}

fn dropped_notice(dropped: &[(String, String)]) -> String {
    let n = dropped.len();
    let plural = if n == 1 { "" } else { "s" };
    let list = dropped
        .iter()
        .map(|(row, col)| format!("{row}{KEY_DISPLAY_SEPARATOR}{col}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("dropped {n} edit{plural} whose rows or columns the new document lacks: {list}")
}

impl gpui::Render for MarketDataTile {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        self.tones.refresh(theme);
        let tones = self.tones;
        // Staleness is a comparison per frame, never a format: `prepare`
        // never reads the clock, so `render` is the one place this is
        // set before the header is painted.
        self.header.stale = self.is_stale(chrono::Utc::now());
        let tile = cx.entity();
        let header = header::render(
            &self.header,
            None,
            None,
            false,
            theme,
            &tones,
            &tile,
            self.id.0,
        );

        // The body: one `DataTable` over this tile's own delegate, in the
        // blotter's chrome (`Size::XSmall`, unbordered, unstriped) so the
        // two read as one application — the whole point of the 2026-09-14
        // ruling. The column strip is the table's own header now, and the
        // rows, the cell styles and the cell editor are `MatrixDelegate`'s.
        // `min_h_0` beside `flex_1`: without it the table's own scroll area
        // cannot shrink below its content and the header scrolls away.
        let body = div().flex_1().min_h_0().w_full().child(
            DataTable::new(&self.table)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false),
        );

        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0))
            .child(header)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use crate::content::MarketDataFactory;
    use crate::core::{CVI, DraftState};
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::query::{
        CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog, QueryKey, QueryOutcome,
    };
    use geode_core::scopes::SavedScopes;
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_data::{DataHandle, Request};
    use geode_shell::actions::ActionId;
    use geode_shell::diagnostics::Diagnostics;
    use geode_shell::frame::{Frame, Publish};
    use geode_shell::module::{Delivery, FindEvent, ModuleFactory, TileContent};
    use geode_shell::tiling::TileId;
    use gpui::{Entity, Window};

    /// Every header tone this tile COLOURS ITSELF must be readable on the
    /// window background of EVERY bundled theme at Part 2c's 3:1 floor.
    /// Before `FlooredTones`, `Warn` painted `warning_foreground` (1.00:1
    /// on twenty themes — an invisible `3 edits`) and `Time`-while-stale
    /// and `Error` painted the raw `warning`/`danger`, under 3:1 on nine
    /// and eight light themes respectively.
    ///
    /// `Plain` and quiet `Time` are the theme's own `muted_foreground` —
    /// the secondary text every other surface (blotter header, status bar,
    /// dialogs) paints unchanged — and nine bundled themes ship it under
    /// 3:1 (Catppuccin Latte 2.20:1). Flooring it in one tile would make
    /// the panel disagree with the rest of the window; that is a theme
    /// authoring matter, not a pairing error, and deliberately not swept.
    #[gpui::test]
    fn every_header_tone_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        use crate::delegate::tests::ground;
        use crate::header::{Tone, tone_colour};
        use geode_core::colour::{READABLE_RATIO, contrast_ratio};
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let floored = FlooredTones::derive(theme);
                let bg = ground(theme);
                for (tone, stale) in [
                    (Tone::Key, false),
                    (Tone::Time, true),
                    (Tone::Warn, false),
                    (Tone::Error, false),
                ] {
                    let colour = tone_colour(tone, stale, theme, &floored);
                    let ratio = contrast_ratio(to_rgb(colour), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {tone:?} stale={stale} at {ratio:.2}:1"));
                    }
                }
                // The header's dirty dot paints `tones.warn` directly (it
                // has no `Tone` of its own to route through `tone_colour`)
                // — already covered by the `Tone::Warn` case above, but
                // asserted here explicitly since the dot is a fill, not a
                // text run, and a future change to `tone_colour` alone
                // would not touch it.
                let dot_ratio = contrast_ratio(to_rgb(floored.warn), bg);
                if dot_ratio < READABLE_RATIO {
                    failures.push(format!("{name}: dirty dot at {dot_ratio:.2}:1"));
                }
            });
        }
        assert!(
            failures.is_empty(),
            "unreadable chips:\n{}",
            failures.join("\n")
        );
    }

    /// The memo re-derives only when one of its four inputs moves: a
    /// theme with the same background, foreground, warning and danger
    /// leaves it untouched, a different warning replaces it.
    #[gpui::test]
    fn floored_tones_refresh_only_when_an_input_changes(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let light = service.resolve("Gruvbox Light").unwrap().clone();
        let dark = service.resolve("Gruvbox Dark").unwrap().clone();
        cx.update(|cx| {
            Theme::global_mut(cx).apply_config(&light);
            let mut tones = FlooredTones::derive(cx.theme());
            let before = tones;
            tones.refresh(cx.theme());
            assert_eq!(tones, before, "same theme: no re-derivation");
            Theme::global_mut(cx).apply_config(&dark);
            tones.refresh(cx.theme());
            assert_ne!(tones, before, "a theme switch re-derives");
            assert_eq!(tones, FlooredTones::derive(cx.theme()));
        });
    }
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::mpsc::Receiver;
    use std::time::{Duration, Instant};

    const TILE: u64 = 3;
    const BASE: &str = "2026-09-12T14:00:00Z";
    const NEWER: &str = "2026-09-12T14:07:00Z";
    const TERMS: [&str; 2] = ["2026-10-16", "2026-11-20"];
    const NODES: [f64; 3] = [-20.0, -1.0, 3.5];

    fn meta(name: &str, attribution: Attribution) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![attribution],
            scope_semantics: ScopeSemantics::Direct,
        }
    }

    fn provenance(as_of: &str) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: "cvi_params".into(),
                as_of: Some(as_of.into()),
                generation: 7,
            }],
            as_of_request: None,
        }
    }

    /// A CVI document in the shape `query::document::compile_document`
    /// delivers one — `document_columns()` order, the value column
    /// `DeterminedNonAdditive`, no grouping — `terms` × `nodes` rows with
    /// `param` running 0.1, 0.2, … in axis order.
    fn document_of(terms: &[&str], nodes: &[f64], as_of: &str) -> Snapshot {
        let mut cells: Vec<(String, f64, f64)> = Vec::new();
        for term in terms {
            for node in nodes {
                let i = cells.len() + 1;
                cells.push(((*term).to_string(), *node, i as f64 / 10.0));
            }
        }
        let n = cells.len();
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into()); n]),
                ),
                (
                    meta("term", Attribution::Additive),
                    TestColumn::Dict(cells.iter().map(|c| Some(c.0.clone())).collect()),
                ),
                (
                    meta("node", Attribution::Additive),
                    TestColumn::F64(cells.iter().map(|c| Some(c.1)).collect()),
                ),
                (
                    meta("param", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(cells.iter().map(|c| Some(c.2)).collect()),
                ),
                (
                    meta("anchor_date", Attribution::Additive),
                    TestColumn::Dict(vec![Some("2026-09-12".into()); n]),
                ),
                (
                    meta("spot_ref", Attribution::Additive),
                    TestColumn::F64(vec![Some(5000.0); n]),
                ),
            ],
            0,
            provenance(as_of),
        )
    }

    fn cvi(as_of: &str) -> Snapshot {
        document_of(&TERMS, &NODES, as_of)
    }

    /// The view under the window's `Root`: renders the tile, and nothing
    /// else — what a test reads comes out of [`Built`], not out of here.
    struct Host {
        tile: Entity<MarketDataTile>,
    }
    impl gpui::Render for Host {
        fn render(
            &mut self,
            _w: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div().size_full().child(self.tile.clone())
        }
    }

    /// What the window closure hands back: it can return only one value,
    /// so everything a test drives or reads is parked here on the way out.
    struct Built {
        content: Box<dyn TileContent>,
        tile: Entity<MarketDataTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
    }

    struct Harness {
        tile: Entity<MarketDataTile>,
        /// Driven through the trait, never by poking the entity: the
        /// shell's own door is what a key, a `:` line and a delivery all
        /// arrive through.
        content: Box<dyn TileContent>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        rx: Receiver<Request>,
        /// The panel's own handle. `DataHandle::shutdown` on it is how a
        /// test makes the next submit be REFUSED (the blotter's own
        /// refusal test and the bridge's picker test use the same trick):
        /// there is no service behind a `for_tests` handle, so this only
        /// drops the sender.
        data: DataHandle,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        let (data, rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data.clone(), &CVI, Duration::from_secs(15 * 60));
        let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let slot = slot.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(
                        TileId(TILE),
                        restored.as_ref(),
                        frame.clone(),
                        diagnostics.clone(),
                        window,
                        cx,
                    );
                    let tile = occupant.view.clone().downcast::<MarketDataTile>().unwrap();
                    *slot.borrow_mut() = Some(Built {
                        content: occupant.content,
                        tile: tile.clone(),
                        frame,
                        diagnostics,
                    });
                    let host = cx.new(|_| Host { tile });
                    // Wrapped in `Root`, exactly as `main.rs` wraps the
                    // shell — and load-bearing here rather than decorative:
                    // gpui-component registers the FOCUSED `InputState` on
                    // the `Root` as a strong `AnyInputState`
                    // (`input::state::sync_focused_input_registry`), so
                    // without one a dropped cell editor would read as
                    // blurred whether or not it was blurred, and
                    // `the_editor_gives_up_focus_before_it_is_dropped`
                    // could not tell R1's two-step order from a bare drop.
                    cx.new(|cx| gpui_component::Root::new(host, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let built = slot.borrow_mut().take().expect("the factory built one");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile: built.tile,
                content: built.content,
                frame: built.frame,
                diagnostics: built.diagnostics,
                rx,
                data,
            },
            vcx,
        )
    }

    impl Harness {
        fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
            vcx.update(|window, cx| self.content.command(line, window, cx))
        }
        fn visible(&self, vcx: &mut gpui::VisualTestContext, visible: bool) {
            vcx.update(|_window, cx| self.content.set_visible(visible, cx));
        }
        fn dispatch(&self, vcx: &mut gpui::VisualTestContext, verb: &str, count: Option<u32>) {
            let id = ActionId(format!("marketdata::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx));
        }
        fn deliver(&self, vcx: &mut gpui::VisualTestContext, tag: u64, snapshot: Arc<Snapshot>) {
            let outcome = QueryOutcome {
                key: QueryKey(TILE),
                tag,
                snapshot: Ok(snapshot),
                submitted: Instant::now(),
            };
            vcx.update(|window, cx| self.content.deliver(Delivery::Query(outcome), window, cx));
        }
        fn deliver_err(&self, vcx: &mut gpui::VisualTestContext, tag: u64, error: &str) {
            let outcome = QueryOutcome {
                key: QueryKey(TILE),
                tag,
                snapshot: Err(error.to_string()),
                submitted: Instant::now(),
            };
            vcx.update(|window, cx| self.content.deliver(Delivery::Query(outcome), window, cx));
        }
        /// The next DOCUMENT request, skipping the `Cancel` a
        /// `set_visible(false)` puts on the same channel — no test asserts
        /// on a cancel, and every one of them would otherwise have to know
        /// whether the panel had been hidden at some point.
        fn document_request(&self) -> Option<geode_core::query::DocumentParams> {
            loop {
                match self.rx.try_recv() {
                    Ok(Request::Document(params)) => return Some(params),
                    Ok(Request::Cancel { .. }) => continue,
                    Ok(other) => panic!("expected a document request, got {other:?}"),
                    Err(_) => return None,
                }
            }
        }
        fn versions(&self, vcx: &gpui::VisualTestContext) -> geode_shell::frame::FrameVersions {
            self.frame.read_with(vcx, |f, _| f.versions())
        }
        fn barrier_open(&self, vcx: &gpui::VisualTestContext) -> bool {
            self.frame.read_with(vcx, |f, _| f.barrier_open())
        }
        fn rows(&self, vcx: &gpui::VisualTestContext) -> usize {
            self.tile.read_with(vcx, |t, _| t.model().rows.len())
        }
        /// What the keymap engine is told — `insert` exactly while the cell
        /// editor holds the keyboard (spec §8.6), read through the trait
        /// rather than off the field, since the context is the only thing
        /// the shell ever sees.
        fn mode(&self, vcx: &gpui::VisualTestContext) -> String {
            self.tile.read_with(vcx, |_, cx| {
                self.content
                    .key_context(cx)
                    .get("mode")
                    .unwrap_or("")
                    .to_string()
            })
        }
        /// The open editor's text, `None` when none is open.
        fn editor_value(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
            self.tile.read_with(vcx, |t, cx| t.editor_value(cx))
        }
        /// Seed the open editor — the one thing a test cannot do through a
        /// key press. Typing for real is exercised in
        /// `typed_text_reaches_the_cell_editor`.
        fn set_editor(&self, vcx: &mut gpui::VisualTestContext, text: &str) {
            let state = self
                .tile
                .read_with(vcx, |t, _| t.editor_state())
                .expect("an open editor");
            vcx.update(|window, cx| {
                state.update(cx, |s, cx| s.set_value(text, window, cx));
            });
        }
        /// One cell as painted: its text and whether it reads as an edit.
        fn cell(&self, vcx: &gpui::VisualTestContext, row: usize, col: usize) -> (String, bool) {
            self.tile.read_with(vcx, |t, _| {
                let cell = &t.model().rows[row].cells[col];
                (cell.text.to_string(), cell.edited)
            })
        }
        /// Every cell's text along one row, and down one column.
        fn row_texts(&self, vcx: &gpui::VisualTestContext, row: usize) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model().rows[row]
                    .cells
                    .iter()
                    .map(|c| c.text.to_string())
                    .collect()
            })
        }
        fn col_texts(&self, vcx: &gpui::VisualTestContext, col: usize) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| {
                t.model()
                    .rows
                    .iter()
                    .map(|r| r.cells[col].text.to_string())
                    .collect()
            })
        }
        /// Key, shown, requested, delivered — the four lines every editing
        /// test starts with.
        fn with_document(&self, vcx: &mut gpui::VisualTestContext) {
            self.command(vcx, "key SPX.Z").expect("a valid key");
            self.visible(vcx, true);
            let tag = self.document_request().expect("one request").tag;
            self.deliver(vcx, tag, Arc::new(cvi(BASE)));
        }
        /// How many columns the table carries: the row-label column plus
        /// one per value column.
        fn columns(&self, vcx: &gpui::VisualTestContext) -> usize {
            self.tile.read_with(vcx, |t, cx| {
                gpui_component::table::TableDelegate::columns_count(
                    t.table().read(cx).delegate(),
                    cx,
                )
            })
        }
        /// Every column header in order, as the table itself answers.
        fn headers(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
            self.tile
                .read_with(vcx, |t, cx| t.table().read(cx).headers(cx))
        }
        /// The table's own (row, column) selection — the tile's cursor
        /// mirrored, in TABLE coordinates (so the column is one to the
        /// right of the model's). The blotter's tests read `selected_row`
        /// the same way.
        fn selection(&self, vcx: &gpui::VisualTestContext) -> (Option<usize>, Option<usize>) {
            self.tile.read_with(vcx, |t, cx| {
                let table = t.table().read(cx);
                (table.selected_row(), table.selected_col())
            })
        }
    }

    fn draw(vcx: &mut gpui::VisualTestContext) {
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// Paints the tile and hands back the centre of one painted element by
    /// its debug selector — the mouse tests below click real bounds, never
    /// a synthesised event, so a listener that is not actually wired to the
    /// painted element fails them (the blotter's own `centre_of`).
    fn centre_of(
        vcx: &mut gpui::VisualTestContext,
        selector: &'static str,
    ) -> gpui::Point<gpui::Pixels> {
        draw(vcx);
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    /// A left mouse-down/up pair at `at` carrying `click_count` — gpui's
    /// own `simulate_click` hardwires a count of 1, and a double-click is
    /// nothing but the second press of a pair with a count of 2.
    fn click_at(
        vcx: &mut gpui::VisualTestContext,
        at: gpui::Point<gpui::Pixels>,
        click_count: usize,
    ) {
        vcx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: gpui::Modifiers::default(),
            button: gpui::MouseButton::Left,
            click_count,
        });
    }

    /// The body is gpui-component's table (user ruling 2026-09-14): one
    /// row-label column carrying the row axis's own name, then one column
    /// per value column.
    ///
    /// The second delivery is the `refresh` probe. `columns_count` reads
    /// the delegate live, so it moves either way — but the HEADER is
    /// painted from `TableState`'s cached column groups
    /// (`prepare_col_groups`, re-run only by `refresh`), so a dropped node
    /// keeps a header cell unless the model swap refreshed the table.
    #[gpui::test]
    fn the_table_shows_one_label_column_plus_the_models_columns(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        assert_eq!(h.columns(&vcx), 1 + NODES.len());
        assert_eq!(
            h.headers(&vcx),
            vec!["term", "-20", "-1", "3.5"],
            "the row axis's own name, then the node labels in document order"
        );
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-th-3").is_some(),
            "the third node's header is painted"
        );

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&TERMS, &NODES[..2], BASE)),
        );
        draw(&mut vcx);
        assert_eq!(h.columns(&vcx), 3, "a node fewer");
        assert!(
            vcx.debug_bounds("marketdata-th-2").is_some(),
            "two nodes are still painted"
        );
        assert!(
            vcx.debug_bounds("marketdata-th-3").is_none(),
            "the dropped node's header is gone — every model swap must `refresh` \
             the table, which is where the painted header comes from"
        );
    }

    /// A single click on a value cell moves the cursor there, exactly as
    /// `j`/`l` would — and the table's own selection follows, since the
    /// tile's cursor is the truth and the delegate mirrors it.
    #[gpui::test]
    fn a_cell_click_moves_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (0, 0));

        // Row 1, the third node: table column 3.
        let at = centre_of(&mut vcx, "marketdata-cell-1-3");
        click_at(&mut vcx, at, 1);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (1, 2),
            "the click moved the cursor to that cell"
        );
        assert_eq!(
            h.selection(&vcx),
            (Some(1), Some(3)),
            "and the table's own selection is the cursor plus the label column"
        );
        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("0.6000"),
            "the cursor really is on the clicked cell, not merely painted there"
        );
    }

    /// The label column is the table's column 0 and the cursor never
    /// enters it: `h` at the first value column stays put, and a click on
    /// a row label moves the ROW while leaving the column alone.
    #[gpui::test]
    fn the_cursor_never_enters_the_label_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (0, 2));
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (0, 0));
        h.dispatch(&mut vcx, "left", None);
        assert_eq!(
            h.selection(&vcx),
            (Some(0), Some(1)),
            "`h` at the first value column stays on it — table column 1, never 0"
        );

        h.dispatch(&mut vcx, "right", Some(2));
        let at = centre_of(&mut vcx, "marketdata-cell-1-0");
        click_at(&mut vcx, at, 1);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (1, 2),
            "a click on a row label moves the row and leaves the column where it was"
        );
        assert_eq!(h.selection(&vcx), (Some(1), Some(3)));
    }

    /// A double-click does what the single click already did — move the
    /// cursor — and opens NO editor (controller ruling 2026-09-14).
    ///
    /// Editing is keyboard-only because a mouse-opened editor could not
    /// keep the keyboard: every tile mouse-down re-arms the shell's
    /// `pending_focus_restore`, and the next render focuses the shell root.
    /// Offering a double-click that opened an editor which then went deaf
    /// would be a broken affordance, so the panel does not offer it; `i`
    /// and `enter` are the edit keys, and this test is what stops a future
    /// change from reintroducing the mapping quietly.
    #[gpui::test]
    fn a_double_click_only_moves_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        let at = centre_of(&mut vcx, "marketdata-cell-1-2");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (1, 1),
            "the cursor moved to the clicked cell"
        );
        assert_eq!(
            h.editor_value(&vcx),
            None,
            "and no editor opened — editing is `i`/`enter` only"
        );
        assert_eq!(h.mode(&vcx), "normal");
        assert!(
            h.tile
                .read_with(&vcx, |t, cx| t.table().read(cx).delegate().editor.is_none()),
            "nothing to paint in the cell either"
        );

        // The keyboard still opens one on that same cell, so the cell the
        // mouse chose is the cell `i` edits.
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.5000"));
        assert_eq!(h.mode(&vcx), "insert");
    }

    /// A click while the cell editor is open CANCELS it and then moves the
    /// cursor (controller ruling 2026-09-14) — a cancel, never a commit:
    /// the typed text is dropped and the draft stays empty, because a click
    /// is not `enter`.
    ///
    /// Cancelling is what stops the editor being left painted on the cell
    /// the cursor just left, deaf to the keyboard (the same mouse-down has
    /// already re-armed the shell's focus restore) while `key_context` still
    /// claims `insert`.
    #[gpui::test]
    fn a_click_while_editing_cancels_the_editor_then_moves(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        // Typed but uncommitted: a commit would write 9.9 into (0, 0).
        h.set_editor(&mut vcx, "9.9");
        assert_eq!(h.mode(&vcx), "insert");

        let at = centre_of(&mut vcx, "marketdata-cell-1-3");
        click_at(&mut vcx, at, 1);
        assert_eq!(h.editor_value(&vcx), None, "the click cancelled the editor");
        assert_eq!(h.mode(&vcx), "normal", "and insert mode went with it");
        assert!(
            h.tile
                .read_with(&vcx, |t, cx| t.table().read(cx).delegate().editor.is_none()),
            "the delegate has nothing left to paint in the old cell"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (1, 2),
            "and the cursor moved to the clicked cell"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a click is not `enter`: nothing was written"
        );
        assert_eq!(
            h.cell(&vcx, 0, 0).0,
            "0.1000",
            "the cell the editor was on still reads the document's own value"
        );
    }

    /// The delegate mirrors the tile's cursor and its open editor — which
    /// is where `render_td` reads both from, so the cursor cell's border
    /// and the in-cell editor are painted off this mirror and nothing
    /// else. (The border itself is a style, invisible to `debug_bounds`:
    /// this pins its one input, and the painted border is a display
    /// check — spec §8.8.7.)
    #[gpui::test]
    fn the_delegate_mirrors_the_cursor_and_the_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let mirror = |vcx: &gpui::VisualTestContext| {
            h.tile.read_with(vcx, |t, cx| {
                let d = t.table().read(cx).delegate();
                (d.cursor, d.editor.as_ref().map(|(at, _)| *at))
            })
        };
        assert_eq!(mirror(&vcx), ((0, 0), None));

        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(
            mirror(&vcx),
            ((1, 2), None),
            "every cursor move reaches the delegate — in MODEL coordinates"
        );

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(
            mirror(&vcx),
            ((1, 2), Some((1, 2))),
            "and so does the open editor's own cell"
        );
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(mirror(&vcx), ((1, 2), None), "cancel clears the mirror too");
    }

    /// The editor is painted IN the cell it edits (spec §8.3), which is
    /// also what makes it typeable at all: gpui installs a text-input
    /// handler only for a focused `Input` that has been drawn.
    #[gpui::test]
    fn the_editor_is_painted_in_the_cursor_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "right", Some(2));
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-editor-1-3").is_none(),
            "no editor before `i`"
        );

        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-editor-1-3").is_some(),
            "the editor paints in the cursor cell (row 1, table column 3)"
        );
        assert!(
            vcx.debug_bounds("marketdata-editor-1-2").is_none(),
            "and in no other cell"
        );

        h.dispatch(&mut vcx, "cancel", None);
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-editor-1-3").is_none(),
            "cancel takes it off the tree again"
        );
    }

    /// The `as_of` mutation + `open_flip` a scope-bar as-of change makes,
    /// in one update block — the shell's own frame observer is registered
    /// before any occupant's, so this really is the order a panel sees.
    fn open_barrier_on_as_of(
        h: &Harness,
        vcx: &mut gpui::VisualTestContext,
        keys: &[QueryKey],
        secs: u32,
    ) {
        let keys = keys.to_vec();
        h.frame.update(vcx, |f, cx| {
            f.set_as_of(geode_core::query::AsOf::At(
                chrono::Utc::now() - chrono::Duration::seconds(secs as i64),
            ));
            f.open_flip(keys, Instant::now());
            cx.notify();
        });
    }

    #[gpui::test]
    fn a_tile_with_a_key_requests_its_document_when_shown(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        assert!(
            h.document_request().is_none(),
            "a hidden tile asks for nothing"
        );
        h.visible(&mut vcx, true);
        let params = h.document_request().expect("shown: one document request");
        assert_eq!(params.dataset, "cvi_params");
        assert_eq!(params.document_key, vec!["SPX.Z".to_string()]);
        assert_eq!(params.key, QueryKey(TILE), "keyed by the tile");
        assert!(params.as_of.is_live());
        assert!(
            h.document_request().is_none(),
            "one request, not one per notify"
        );
    }

    #[gpui::test]
    fn a_delivered_snapshot_builds_the_matrix_and_the_header(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        let (rows, columns, chips) = h.tile.read_with(&vcx, |t, _| {
            (
                t.model().rows.len(),
                t.model().columns.len(),
                t.header_texts(),
            )
        });
        assert_eq!(rows, 2, "two terms down the side");
        assert_eq!(columns, 3, "three nodes across the top");
        assert!(chips.iter().any(|c| c == "CVI"), "the title: {chips:?}");
        assert!(
            chips.iter().any(|c| c == "SPX.Z"),
            "the underlying: {chips:?}"
        );
        assert!(
            chips.iter().any(|c| c == "spot 5000"),
            "each header attribute: {chips:?}"
        );
        let local = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string();
        assert!(
            // `starts_with`, not an exact match: `BASE` is a fixed past
            // date, so whether it also reads " stale" depends on how far
            // real wall-clock `now` has drifted past it — this test is
            // about the time text itself, not the staleness marker.
            chips.iter().any(|c| c.starts_with(&local)),
            "the source time on the trader's own clock ({local}): {chips:?}"
        );
    }

    #[gpui::test]
    fn a_stale_tag_is_dropped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        // A five-term document under the PREVIOUS tag: a tile that
        // ignored the tag would repaint with it.
        h.deliver(
            &mut vcx,
            tag - 1,
            Arc::new(document_of(
                &["a", "b", "c", "d", "e"],
                &NODES,
                "2026-09-12T13:00:00Z",
            )),
        );
        let (rows, source) = h.tile.read_with(&vcx, |t, _| {
            (t.model().rows.len(), t.model().source_time.clone())
        });
        assert_eq!(rows, 2, "the stale outcome must not land");
        assert_eq!(source.as_deref(), Some(BASE));
    }

    #[gpui::test]
    fn an_error_outcome_keeps_the_last_model_and_shows_the_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.deliver_err(&mut vcx, tag, "the document select failed");
        let (rows, notice) = h.tile.read_with(&vcx, |t, _| {
            (t.model().rows.len(), t.notice().map(str::to_string))
        });
        assert_eq!(rows, 2, "last good stays on screen");
        assert_eq!(notice.as_deref(), Some("the document select failed"));
    }

    /// MIN-5 split the dispatch tail: a motion notifies without
    /// re-preparing the header (nothing there shows the cursor), while an
    /// action that writes or clears the notice must still rebuild it. This
    /// pins both halves of that decision — the two `true` arms — since
    /// getting one wrong leaves a header the trader can read that no
    /// longer matches the tile.
    ///
    /// The notice is written here by a REFUSED commit (Task 7): `edit`
    /// itself no longer writes one now that it opens a real editor instead
    /// of saying which task the editing lands in.
    #[gpui::test]
    fn a_notice_reaches_the_header_and_escape_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        let chips = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.header_texts());
        let before = chips(&vcx);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(
            chips(&vcx),
            before,
            "a motion changes nothing in the header"
        );

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "wide");
        h.dispatch(&mut vcx, "commit", None);
        assert!(
            chips(&vcx).iter().any(|c| c == "'wide' is not a number"),
            "a notice must reach the chips on the keystroke that set it: {:?}",
            chips(&vcx)
        );
        h.dispatch(&mut vcx, "cancel", None);
        h.dispatch(&mut vcx, "escape", None);
        assert_eq!(
            chips(&vcx),
            before,
            "and escape must take it back out again"
        );
    }

    /// Review fix round 1, the Important: while a barrier still wants this
    /// panel's key, a delivery is STAGED, not painted. A document select is
    /// cheap, so this panel is the one most likely to paint the new as-of
    /// — grid, header, source-time chip — a frame before every blotter
    /// promotes its own heavier outcome, which is the half-updated screen
    /// the barrier exists to prevent.
    #[gpui::test]
    fn a_delivery_under_an_open_barrier_is_staged_until_the_flip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        assert_eq!(h.rows(&vcx), 2, "the two-term document is on screen");

        // A second key in the set: a blotter that has not answered yet, so
        // this panel's own arrival cannot release the barrier.
        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().expect("an as-of change requeries");
        h.deliver(
            &mut vcx,
            second.tag,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(
            h.rows(&vcx),
            2,
            "staged, not painted: the five-term document must wait for the flip"
        );
        assert!(h.barrier_open(&vcx), "the other tile has not arrived yet");

        // The other tile answers; the barrier empties, `flip` bumps, and
        // this panel's observer promotes what it staged.
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.arrived(other, now), "that arrival empties the barrier");
            cx.notify();
        });
        assert!(!h.barrier_open(&vcx));
        assert_eq!(h.rows(&vcx), 5, "the flip is what puts it on screen");
    }

    /// Found while writing the staging path, not by the review: a key
    /// change bumps no frame version, so a snapshot staged for the OLD key
    /// still passes `promote`'s flip-identity check — and `promote` runs
    /// regardless of visibility (a panel hidden between staging and the
    /// flip must not come back stale), so a hidden panel could put the
    /// previous document's grid on screen under the new key's header.
    #[gpui::test]
    fn a_key_change_drops_what_was_staged_for_the_old_key(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "staged, as the test above pins");

        // Hidden, then pointed at another document, then the barrier
        // releases: the promote that fires must find nothing.
        h.visible(&mut vcx, false);
        h.command(&mut vcx, "key NDX.Z").unwrap();
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            f.arrived(other, now);
            cx.notify();
        });
        assert_eq!(
            h.rows(&vcx),
            0,
            "SPX.Z's document must not be painted under NDX.Z"
        );
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts().iter().any(|c| c == "NDX.Z")),
            "and the header is the new key's"
        );
    }

    /// The other half: when this panel's own arrival is what empties the
    /// barrier, it promotes on the spot rather than waiting for the `flip`
    /// bump to come back round to its observer on a later notify pass.
    #[gpui::test]
    fn a_delivery_that_releases_the_barrier_promotes_at_once(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], 60);
        let second = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert!(!h.barrier_open(&vcx), "the only awaited key has arrived");
        assert_eq!(
            h.rows(&vcx),
            5,
            "and nothing is left staged: the release promoted it in the same pass"
        );
    }

    /// MIN-2: `set_visible(false)` cancels the in-flight request, so no
    /// outcome will ever arrive for the versions `acted` records — leaving
    /// it set makes the re-shown panel decide it is already up to date and
    /// keep painting whatever it had before it was hidden.
    #[gpui::test]
    fn a_tile_hidden_mid_flight_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        // Hidden before the outcome lands, then shown again with nothing
        // about the frame having changed.
        h.visible(&mut vcx, false);
        h.visible(&mut vcx, true);
        let second = h
            .document_request()
            .expect("a cancelled request must be asked again");
        assert!(second.tag > first.tag);
    }

    /// MIN-3, the panel's half (the blotter's own is
    /// `a_refused_query_arrives_at_the_barrier_and_retries_on_the_next_change`):
    /// a refused submit means nothing is coming, so it answers the barrier
    /// at once and clears `acted` so the next frame change is a real retry.
    #[gpui::test]
    fn a_refused_request_arrives_at_the_barrier_and_retries(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        // Nothing can be queued from here on.
        h.data.shutdown();
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE)], 60);
        assert!(
            !h.barrier_open(&vcx),
            "a refusal must answer the barrier: nothing is coming for it"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("document request refused: the data service is busy or gone".to_string())
        );
        // The next frame change tries again rather than reading as
        // already-answered.
        h.frame.update(&mut vcx, |f, cx| {
            f.note_published(Publish {
                dataset: "cvi_params".into(),
                batch: "SPX.Z".into(),
                books: 0,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
        assert!(
            h.tile.read_with(&vcx, |t, _| t.acted_is_none()),
            "still nothing acted on: every submit is refused, and each one retries"
        );
    }

    /// Phase 4 §3.10: `ShellView::visible_tile_keys` puts every visible
    /// occupant in the barrier's key set, because it cannot know which
    /// tiles follow which counters. A scope change is not a change this
    /// panel requeries for — so if it does not answer the barrier, every
    /// blotter on screen waits out `FLIP_DEADLINE` (250 ms) on every
    /// scope keystroke. The shell's own observer is registered first, so
    /// the mutation and `open_flip` really do land before this panel's
    /// observer runs, which is what this update block reproduces.
    #[gpui::test]
    fn a_panel_self_arrives_on_a_scope_change_it_does_not_requery_for(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.frame.update(&mut vcx, |f, cx| {
            f.set_scope(geode_core::scope::Scope {
                text: Some("spx".into()),
                ..Default::default()
            });
            f.open_flip([QueryKey(TILE)], Instant::now());
            cx.notify();
        });
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "the panel must answer a barrier it has nothing coming for, \
             rather than holding every other tile to the deadline"
        );
        assert!(
            h.document_request().is_none(),
            "and it must not requery for a scope change either"
        );
    }

    /// The other half: a change the panel DOES requery for is answered by
    /// its own delivery, under the versions the request was made — never
    /// early, or the barrier would release before this panel had the
    /// document it is about to paint.
    #[gpui::test]
    fn a_panel_arrives_on_delivery_after_an_as_of_change(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        let at = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Utc);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_as_of(geode_core::query::AsOf::At(at));
            f.open_flip([QueryKey(TILE)], Instant::now());
            cx.notify();
        });
        let second = h
            .document_request()
            .expect("an as-of change is this panel's own requery");
        assert!(
            h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "still open: the panel has asked but has nothing to paint yet"
        );
        h.deliver(&mut vcx, second.tag, Arc::new(cvi(BASE)));
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "the delivery is the arrival"
        );
    }

    /// An error is an arrival too (the blotter's rule): one broken tile
    /// must never hold every other tile open until the deadline.
    #[gpui::test]
    fn a_failed_delivery_still_arrives(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let at = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Utc);
        h.frame.update(&mut vcx, |f, cx| {
            f.set_as_of(geode_core::query::AsOf::At(at));
            f.open_flip([QueryKey(TILE)], Instant::now());
            cx.notify();
        });
        let second = h.document_request().unwrap().tag;
        h.deliver_err(&mut vcx, second, "the document select failed");
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "a failure arrives as surely as a snapshot does"
        );
    }

    /// Ruling 2026-09-14: unsent edits are never discarded by a key
    /// change. A draft's cells are grid indices into the document they
    /// were made on, so the panel cannot carry them — and must not throw
    /// them away on a keystroke that reads like navigation.
    #[gpui::test]
    fn a_key_change_is_refused_while_the_draft_has_edits(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        assert_eq!(
            h.command(&mut vcx, "key NDX.Z"),
            Err("1 cell pending — :revert first".to_string()),
            "the count is spelled as the header spells it, and the verb is named"
        );
        let (key, edits) = h
            .tile
            .read_with(&vcx, |t, _| (t.serialize(), t.draft().len()));
        assert_eq!(edits, 1, "the edit is still there");
        assert_eq!(
            key.get("underlying")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            key["underlying"][0].as_str(),
            Some("SPX.Z"),
            "and the panel is still on the document those edits belong to"
        );
        assert!(
            h.document_request().is_none(),
            "a refused key change asks for nothing"
        );
    }

    #[gpui::test]
    fn a_publish_bumps_the_frame_and_the_tile_requeries(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        h.frame.update(&mut vcx, |f, cx| {
            f.note_published(Publish {
                dataset: "cvi_params".into(),
                batch: "SPX.Z".into(),
                books: 0,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
        let second = h
            .document_request()
            .expect("a publish bumps `data`, so the panel asks again");
        assert!(second.tag > first.tag, "a fresh tag supersedes the old one");
    }

    /// A generation can be shorter than the one it replaces — the desk
    /// drops an expiry and the whole ladder moves up. A cursor left past
    /// the new end would yank, edit and paint nothing at all, so the
    /// delivery re-clamps it; `move_cursor`'s own clamp cannot cover this,
    /// since nothing moved.
    #[gpui::test]
    fn a_shorter_document_clamps_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        h.dispatch(&mut vcx, "bottom", None);
        h.dispatch(&mut vcx, "last_col", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (4, 2));

        // Two terms and two nodes now: both axes shrank under the cursor.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&TERMS, &NODES[..2], BASE)),
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (1, 1),
            "the cursor is clamped into the new grid"
        );
        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("0.4000"),
            "and what it points at is a real cell"
        );
    }

    #[gpui::test]
    fn cursor_moves_with_counts_and_scrolls(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        let terms = ["t0", "t1", "t2", "t3", "t4"];
        h.deliver(&mut vcx, tag, Arc::new(document_of(&terms, &NODES, BASE)));

        h.dispatch(&mut vcx, "down", Some(3));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (3, 0));
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (3, 2));
        h.dispatch(&mut vcx, "down", Some(9));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            (4, 2),
            "a count past the end clamps"
        );
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (0, 0));
        h.dispatch(&mut vcx, "last_col", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (0, 2));

        // The scroll is the table's now: `sync_cursor` sets the selected
        // row (which scrolls it into view) and the selected column, so the
        // selection is what a test reads — the blotter's own tests read
        // `selected_row` exactly this way.
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(
            h.selection(&vcx),
            (Some(terms.len() - 1), Some(MatrixDelegate::table_col(2))),
            "G moves the table's selected row, which is what scrolls it into view"
        );
    }

    #[gpui::test]
    fn key_completions_come_from_the_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        // No catalog yet: showing the tile must ASK for one, or the
        // completions could never arrive.
        h.visible(&mut vcx, true);
        assert!(
            h.diagnostics
                .read_with(&vcx, |d, _| d.pending_catalog_request()),
            "a panel with no catalog requests one"
        );
        assert!(
            h.tile
                .read_with(&vcx, |t, cx| t.completions("key ", 4, cx))
                .is_empty(),
            "nothing to offer until one arrives"
        );

        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["SPX.Z", "NDX.Z"]));
            cx.notify();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, cx| t.completions("key ", 4, cx)),
            vec!["NDX.Z".to_string(), "SPX.Z".to_string()],
            "the dataset's own document keys, sorted"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, cx| t.completions("", 0, cx)),
            commands::completions("", 0, &[], false),
            "the verb position is the pure core's vocabulary"
        );

        // MIN-4 (controller ruling): a `:key` line asks for a fresh
        // catalog EVEN THOUGH one is already held — documents arrive while
        // the panel is open (a subscribed feed publishes a new key every
        // few seconds), and nothing in a held `CatalogSnapshot` can say
        // whether it is still current. Gated on staleness, the panel asked
        // once and then offered a list that could never grow.
        let already = h
            .diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());
        assert!(already, "the request from `set_visible` — drained");
        h.command(&mut vcx, "key SPX.Z").unwrap();
        assert!(
            h.diagnostics
                .read_with(&vcx, |d, _| d.pending_catalog_request()),
            "a `:key` line asks again regardless"
        );
    }

    fn catalog(batches: &[&str]) -> CatalogSnapshot {
        CatalogSnapshot {
            datasets: vec![
                DatasetCatalog {
                    name: "cvi_params".into(),
                    partitions: batches
                        .iter()
                        .map(|b| PartitionCatalog {
                            batch: (*b).to_string(),
                            book: None,
                            generations: vec![GenerationInfo {
                                gen_id: 1,
                                source_time: chrono::Utc::now(),
                                loaded_at: None,
                                file_rows: None,
                                live: true,
                            }],
                            resolved_gen: None,
                        })
                        .collect(),
                    ..DatasetCatalog::default()
                },
                DatasetCatalog {
                    name: "risk".into(),
                    partitions: vec![PartitionCatalog {
                        batch: "EOD".into(),
                        book: Some("BK0".into()),
                        ..PartitionCatalog::default()
                    }],
                    ..DatasetCatalog::default()
                },
            ],
            ..CatalogSnapshot::default()
        }
    }

    #[gpui::test]
    fn serialize_round_trips_key_and_draft(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
underlying = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, vcx) = open_with(cx, Some(restored.clone()));
        let written = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(
            written, restored,
            "the underlying and the draft survive a restart, labels and all"
        );
    }

    #[gpui::test]
    fn a_session_written_with_key_still_restores(cx: &mut gpui::TestAppContext) {
        let mut t = toml::Table::new();
        t.insert(
            "key".into(),
            toml::Value::Array(vec![toml::Value::String("SPX.Z".into())]),
        );
        let (h, mut vcx) = open_with(cx, Some(t));
        h.visible(&mut vcx, true);
        let req = h
            .document_request()
            .expect("a restored underlying requests its document");
        assert_eq!(req.document_key, vec!["SPX.Z".to_string()]);
    }

    #[gpui::test]
    fn a_restored_draft_is_rebased_onto_the_first_delivery(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let params = h.document_request().expect("a restored key requeries");
        assert_eq!(params.document_key, vec!["SPX.Z".to_string()]);
        h.deliver(&mut vcx, params.tag, Arc::new(cvi(BASE)));
        // `Draft::from_toml` parks every restored edit out of the grid's
        // range; without the tile's rebase it would paint nowhere.
        let cell = h
            .tile
            .read_with(&vcx, |t, _| t.model().rows[1].cells[1].clone());
        assert_eq!(cell.text.to_string(), "9.5000");
        assert!(cell.edited, "the restored edit paints as an edit");
    }

    #[gpui::test]
    fn a_restored_draft_lands_in_behind_on_a_newer_delivery(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        let (state, chips) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().state.clone(), t.header_texts()));
        assert!(
            matches!(state, DraftState::Behind { .. }),
            "a newer generation under a restored draft is Behind, got {state:?}"
        );
        assert!(
            chips.iter().any(|c| c.starts_with("update ")),
            "the header says so: {chips:?}"
        );
    }

    #[gpui::test]
    fn yank_writes_the_cell_row_and_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some("0.1000"));
        h.dispatch(&mut vcx, "yank_row", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("2026-10-16\t0.1000\t0.2000\t0.3000"),
            "the row is label then cells, tab separated"
        );
        h.dispatch(&mut vcx, "yank_col", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("0.1000\n0.4000"),
            "the column is newline separated"
        );
    }

    fn clipboard(vcx: &mut gpui::VisualTestContext) -> Option<String> {
        vcx.update(|_window, cx| cx.read_from_clipboard().and_then(|c| c.text()))
    }

    #[gpui::test]
    fn find_moves_the_cursor_to_a_matching_row_label(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("11-20".into()), window, cx)
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()).0,
            1,
            "the cursor jumps to the matching term"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()).0,
            0,
            "escape restores the origin"
        );
    }
    // ---- Task 7: cell editing ---------------------------------------

    /// The whole round trip (spec §8.3/§8.6): `edit` opens a tile-owned
    /// input seeded with the cell's own text, `commit` parses it through
    /// the column's DECLARED type and writes the draft, and the header
    /// counts it. The value assertion is the point — a commit that wrote
    /// the raw text without parsing it would put `0.0` in the cell and
    /// every marker assertion here would still pass.
    #[gpui::test]
    fn edit_commit_paints_the_cell_as_edited_and_the_header_counts_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert", "the shell is told to stop matching");
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("0.1000"),
            "seeded with the cell's own text, so a small correction is a small edit"
        );

        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        assert_eq!(
            h.cell(&vcx, 0, 0),
            ("0.5000".to_string(), true),
            "the parsed value, formatted by the panel's own format, marked as an edit"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.header_dirty()),
            "the header's dirty dot marks it"
        );
        assert!(h.editor_value(&vcx).is_none(), "the editor is gone");
        assert_eq!(
            h.mode(&vcx),
            "normal",
            "and the keyboard is the panel's again"
        );
        let (len, base) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().len(), t.draft().base.clone()));
        assert_eq!(len, 1);
        assert_eq!(
            base.as_deref(),
            Some(BASE),
            "recorded against the generation on screen, which is what makes it Behind-able"
        );
    }

    /// R1 (Task 4, verified at the pinned gpui-component rev): `blur`
    /// FIRST, then drop. Dropping the `InputState` alone does not make
    /// `Window::focused` `None` — `Root` holds the focused input as a
    /// strong `AnyInputState` and only ever unregisters it from the
    /// `Input`'s own render, which an input removed from the tree never
    /// reaches. Without the blur the window keeps handing focus to a dead
    /// editor, the shell's `render` net (`window.focused(cx).is_none()`)
    /// never fires, and every chord is gone for the rest of the session.
    /// A module gives focus UP; it never takes the shell's (CLAUDE.md).
    #[gpui::test]
    fn the_editor_gives_up_focus_before_it_is_dropped(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        // The draw matters: gpui installs a text-input handler only for a
        // focused `Input` that has been painted, and it is that paint which
        // registers it on the `Root`.
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let handle = h
            .tile
            .read_with(&vcx, |t, cx| {
                t.editor_state().map(|s| s.read(cx).focus_handle(cx))
            })
            .expect("an open editor");
        assert!(
            vcx.update(|window, _cx| handle.is_focused(window)),
            "`edit` must give the cell editor the keyboard"
        );

        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "commit must blur before dropping: focus has to be free for the shell's own net"
        );

        // And the same on the way out through `cancel`.
        h.dispatch(&mut vcx, "edit", None);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        h.dispatch(&mut vcx, "cancel", None);
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "cancel must blur before dropping too"
        );
    }

    /// The keys really do reach the cell (spec §8.6): the shell's insert
    /// branch hands a bare keystroke to the focused input, and this is the
    /// panel's own half of that — a painted, focused `Input` that takes
    /// characters.
    #[gpui::test]
    fn typed_text_reaches_the_cell_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        h.set_editor(&mut vcx, "");
        vcx.simulate_input("0.5");
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("0.5"),
            "every typed character must land in the cell's own input"
        );
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, 0), ("0.5000".to_string(), true));
    }

    #[gpui::test]
    fn cancel_drops_the_editor_without_a_change(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.75");
        h.dispatch(&mut vcx, "cancel", None);

        assert!(h.editor_value(&vcx).is_none(), "the editor is dropped");
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.cell(&vcx, 0, 0),
            ("0.1000".to_string(), false),
            "and the typed value went nowhere"
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    }

    /// A refusal is inline and the trader stays in the cell — retyping is
    /// one keystroke away, where dropping the editor would throw the whole
    /// value back at them.
    #[gpui::test]
    fn a_non_numeric_commit_refuses_inline_and_stays_in_insert_mode(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "wide");
        h.dispatch(&mut vcx, "commit", None);

        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("'wide' is not a number".to_string()),
            "the refusal quotes what was typed"
        );
        assert_eq!(h.mode(&vcx), "insert", "and the cell keeps the keyboard");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("wide"));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .iter()
                .any(|c| c == "'wide' is not a number"),
            "the notice reaches the header on the keystroke that set it"
        );
    }

    /// `:bump <delta> [row|col]` — the cursor's ROW by default, because a
    /// term's whole node ladder is the shape a trader nudges.
    #[gpui::test]
    fn bump_adds_to_the_cursors_row_by_default_and_to_its_column_on_request(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        h.command(&mut vcx, "bump 0.25")
            .expect("a bump with a document");
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec!["0.3500", "0.4500", "0.5500"],
            "the cursor's whole row moved"
        );
        assert_eq!(
            h.row_texts(&vcx, 1),
            vec!["0.4000", "0.5000", "0.6000"],
            "and nothing else did"
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 3);

        h.command(&mut vcx, "revert").unwrap();
        h.command(&mut vcx, "bump 1 col").unwrap();
        assert_eq!(
            h.col_texts(&vcx, 0),
            vec!["1.1000", "1.4000"],
            "`col` walks the cursor's column instead"
        );
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec!["1.1000", "0.2000", "0.3000"],
            "and only that column"
        );

        // It composes with an edit already made rather than reading through
        // to the document underneath it.
        h.command(&mut vcx, "bump 1 col").unwrap();
        assert_eq!(h.col_texts(&vcx, 0), vec!["2.1000", "2.4000"]);
    }

    #[gpui::test]
    fn revert_clears_every_edit(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "bump 0.25").unwrap();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 3);

        h.command(&mut vcx, "revert").expect("edits to clear");
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec!["0.1000", "0.2000", "0.3000"],
            "the document's own numbers are back"
        );
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.header_dirty()),
            "and the header says nothing about a draft"
        );
        assert_eq!(
            h.command(&mut vcx, "revert"),
            Err("no edits to revert".to_string()),
            "a second revert has nothing to do and says so"
        );
    }

    /// Controller ruling 2026-09-14: `:revert` while `Behind` reduces to
    /// `:discard` — `Clean` means "on the live document" — so the newer
    /// document paints, not the base the edits were made against with
    /// nothing left to explain why.
    #[gpui::test]
    fn revert_while_behind_shows_the_newer_document_clean(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));

        h.command(&mut vcx, "revert").expect("an edit to clear");

        let (state, rows, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert_eq!(state, DraftState::Clean);
        assert_eq!(
            rows, 1,
            "the newer document, not the base, is now on screen"
        );
        assert_eq!(cell.text.to_string(), "0.1000", "the document's own value");
        assert!(!cell.edited);
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.header_dirty())
                && !chips.iter().any(|c| c.contains("update")),
            "nothing left to explain a generation that is no longer retained: {chips:?}"
        );
        // Both are now refused again — there is no draft to move or drop.
        assert_eq!(
            h.command(&mut vcx, "rebase"),
            Err("nothing to rebase — the draft is on the live document".to_string())
        );
        assert_eq!(
            h.command(&mut vcx, "discard"),
            Err("nothing to rebase — the draft is on the live document".to_string())
        );
    }

    /// `mode == insert` is exactly "an editor is open", and the shell keys
    /// its whole insert branch on that one pair — so BOTH ways out have to
    /// be covered: an editor still open after a commit leaves the panel
    /// swallowing every keystroke as text with nothing on screen to say
    /// why, which is the same failure as never closing it at all.
    #[gpui::test]
    fn key_context_reports_insert_while_the_editor_exists(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        assert_eq!(h.mode(&vcx), "normal");

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal", "a commit closes the editor");

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal", "and so does a cancel");
    }

    /// Controller ruling: an edit needs a document. With no grid there is
    /// no cell to key an edit by and no generation to record it against, so
    /// `edit` and `:bump` refuse rather than opening an editor over
    /// nothing — a draft whose base were the empty string could never be
    /// told from one made against a real generation.
    #[gpui::test]
    fn editing_with_no_document_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);

        h.dispatch(&mut vcx, "edit", None);
        assert!(
            h.editor_value(&vcx).is_none(),
            "no editor over an empty grid"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("no document to edit".to_string())
        );
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.command(&mut vcx, "bump 1"),
            Err("no document to edit".to_string()),
            "and the `:` line says the same thing on its own surface"
        );
    }

    /// The defence beyond the brief: a delivery can land while the editor
    /// is open, and a shorter generation clamps the cursor under it
    /// (`a_shorter_document_clamps_the_cursor`). An edit belongs to the
    /// cell the trader OPENED, so `commit` checks that cell's labels are
    /// still the ones it opened on and refuses otherwise — writing to
    /// whatever the clamped cursor now points at would file a typed number
    /// against a different term.
    #[gpui::test]
    fn a_commit_whose_cell_moved_under_it_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), (4, 0));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");

        // Two terms now: the cursor is clamped to row 1 under the open
        // editor, which is a different term entirely.
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.dispatch(&mut vcx, "commit", None);
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "the typed value must not land on the term the cursor was clamped to"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("the document changed under the edit — nothing was written".to_string())
        );
        assert!(h.editor_value(&vcx).is_none(), "and the editor is dropped");
    }

    /// One committed edit, then a delivery for the SAME tag under a
    /// different `as_of` (a subscribed feed republishing without this
    /// panel ever requerying) — spec §8.4's core rule: `Behind` keeps
    /// painting the base generation under the edit rather than the newer
    /// one that just arrived.
    #[gpui::test]
    fn a_newer_generation_under_a_draft_goes_behind_and_keeps_painting_the_base(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        let (state, rows, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "got {state:?}"
        );
        assert_eq!(rows, 2, "still the base generation's two terms");
        assert_eq!(
            cell.text.to_string(),
            "0.5000",
            "the edit is still on screen"
        );
        assert!(cell.edited);
        let local = chrono::DateTime::parse_from_rfc3339(NEWER)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c == &format!("update {local}")),
            "{chips:?}"
        );
    }

    /// A SECOND newer generation arriving on top of an already-`Behind`
    /// draft moves the header's own `newer` marker forward — but the
    /// `base_snapshot` a trader is still looking at must not move: they
    /// have not chosen `:rebase`/`:discard` for the FIRST newer document
    /// yet, let alone this one.
    #[gpui::test]
    fn a_second_newer_generation_keeps_the_base_and_updates_newer(cx: &mut gpui::TestAppContext) {
        const NEWEST: &str = "2026-09-12T14:15:00Z";
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-25"], &NODES, NEWEST)),
        );

        let (state, rows, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWEST),
            "moves to the LATEST as_of, got {state:?}"
        );
        assert_eq!(rows, 2, "still the ORIGINAL base generation's two terms");
        assert_eq!(cell.text.to_string(), "0.5000", "the edit is untouched");
        let local = chrono::DateTime::parse_from_rfc3339(NEWEST)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c == &format!("update {local}")),
            "{chips:?}"
        );
    }

    /// `:rebase` moves each edit onto the newer document by label — one
    /// dropped (its term disappeared) and one kept at a NEW grid index
    /// (the newer document's only term sorts first) — and starts painting
    /// it.
    #[gpui::test]
    fn rebase_reapplies_edits_by_label_and_reports_dropped_ones(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        // Two edits on term 0 (dropped by the newer document) and one on
        // term 1 (kept — the newer document's only row).
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.6");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "left", None);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.7");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 3);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        h.command(&mut vcx, "rebase")
            .expect("behind: rebase applies");

        let (state, len, rows, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().len(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert_eq!(state, DraftState::Editing, "one edit survived the rebase");
        assert_eq!(len, 1);
        assert_eq!(rows, 1, "now painting the newer document");
        assert_eq!(
            cell.text.to_string(),
            "0.7000",
            "the kept edit, at its new index"
        );
        assert!(cell.edited);

        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c
                == "dropped 2 edits whose rows or columns the new document lacks: \
2026-10-16/-20, 2026-10-16/-1"),
            "{chips:?}"
        );
    }

    /// `:discard` drops the edits outright and shows the newer document —
    /// clean, no `base_snapshot` left behind.
    #[gpui::test]
    fn discard_shows_the_newer_document_clean(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));

        h.command(&mut vcx, "discard")
            .expect("behind: discard applies");

        let (state, rows, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert_eq!(state, DraftState::Clean);
        assert_eq!(rows, 1, "the newer document, its one term");
        assert_eq!(cell.text.to_string(), "0.1000", "the document's own value");
        assert!(!cell.edited);
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.header_dirty()),
            "no dirty dot left"
        );
    }

    /// Outside `Behind` — a clean panel and one with edits still against
    /// the live document alike — there is no "newer" to move onto or
    /// fall back to.
    #[gpui::test]
    fn rebase_outside_behind_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "rebase"),
            Err("nothing to rebase — the draft is on the live document".to_string())
        );
        assert_eq!(
            h.command(&mut vcx, "discard"),
            Err("nothing to rebase — the draft is on the live document".to_string())
        );

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.command(&mut vcx, "rebase"),
            Err("nothing to rebase — the draft is on the live document".to_string()),
            "edits present, but still on the document they were made against"
        );
        assert_eq!(
            h.command(&mut vcx, "discard"),
            Err("nothing to rebase — the draft is on the live document".to_string())
        );
    }

    #[gpui::test]
    fn completions_offer_rebase_and_discard_only_while_behind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        let before = h.tile.read_with(&vcx, |t, cx| t.completions("", 0, cx));
        assert!(!before.contains(&"rebase".to_string()));
        assert!(!before.contains(&"discard".to_string()));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        let behind = h.tile.read_with(&vcx, |t, cx| t.completions("", 0, cx));
        assert!(behind.contains(&"rebase".to_string()));
        assert!(behind.contains(&"discard".to_string()));
    }

    /// Controller ruling 2026-09-14: an edit on top of a `Behind` draft —
    /// live or restored — can double-count a cell and lets `:rebase` map a
    /// parked value over a newer one, so both surfaces refuse outright
    /// rather than opening an editor or writing another edit.
    #[gpui::test]
    fn edit_and_bump_are_refused_while_behind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));

        h.dispatch(&mut vcx, "edit", None);
        assert!(
            h.editor_value(&vcx).is_none(),
            "no editor opens while behind"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("the draft is behind — :rebase or :discard first".to_string())
        );
        assert_eq!(
            h.command(&mut vcx, "bump 1"),
            Err("the draft is behind — :rebase or :discard first".to_string())
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            1,
            "no second edit was written"
        );
    }

    /// I-1 (final whole-branch review): a stage taken under an as-of
    /// barrier must survive the barrier being REPLACED by a mutation this
    /// panel does not follow. A scope keystroke (or a grouping step, or
    /// `mod+z`) within the 250 ms window replaces the barrier without
    /// bumping `flip`; the panel does not requery for it — so the staged
    /// snapshot is the only answer it will ever get for the new as-of,
    /// and dropping it left the PRE-as-of generation painted under the
    /// window-wide historical stripe with `acted` claiming the panel was
    /// current (no requery until some dataset's next publish).
    #[gpui::test]
    fn a_stage_survives_a_barrier_replaced_by_a_change_the_panel_does_not_follow(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        assert_eq!(h.rows(&vcx), 2, "the two-term document is on screen");

        // B1 over this panel and one blotter that has not answered yet.
        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().expect("an as-of change requeries");
        h.deliver(
            &mut vcx,
            second.tag,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "staged, not painted");

        // A scope keystroke inside the window: B1 is replaced by B2 over
        // the NEW scope. The panel follows neither `scope` nor
        // `grouping`, so it never requeries — it self-arrives on B2.
        h.frame.update(&mut vcx, |f, cx| {
            f.set_text(Some("SPX".into()));
            f.open_flip([QueryKey(TILE), other], Instant::now());
            cx.notify();
        });
        assert!(
            h.document_request().is_none(),
            "a scope bump is not something this panel requeries for"
        );

        // The blotter answers B2, which releases it and bumps `flip`.
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            f.arrived(other, now);
            cx.notify();
        });
        assert!(!h.barrier_open(&vcx));
        assert_eq!(
            h.rows(&vcx),
            5,
            "the staged as-of answer must still promote: nothing this panel \
             follows moved, and no other answer is coming"
        );
    }

    /// The other side of the same gate, and why it is still load-bearing:
    /// a stage whose own `as_of` no longer matches the frame's must NOT
    /// promote. Reachable while HIDDEN — `set_visible(false)` cancels the
    /// request but a stage already taken stays, and a hidden panel does
    /// not requery for the as-of change that follows.
    #[gpui::test]
    fn a_stage_is_dropped_when_a_counter_the_panel_follows_has_moved(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "staged");

        // Hidden, then a SECOND as-of change — a counter this panel does
        // follow — and finally the barrier releases.
        h.visible(&mut vcx, false);
        open_barrier_on_as_of(&h, &mut vcx, &[other], 120);
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            f.arrived(other, now);
            cx.notify();
        });
        assert_eq!(
            h.rows(&vcx),
            2,
            "the stage answers an as-of nobody is looking at any more"
        );
    }

    /// I-2 (final whole-branch review), trace 1: the first delivery is an
    /// EMPTY snapshot — the key has no document in this database, or the
    /// session's persisted as-of predates the document's first publish
    /// (`compile_document`'s `and false` arm). Rebasing a restored draft
    /// against that model dropped every edit silently and left a clean
    /// panel: the one path on the branch that lost unsent work (§8.5).
    #[gpui::test]
    fn a_restored_draft_survives_an_empty_first_delivery(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(document_of(&[], &NODES, BASE)));

        let (edits, chips) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().len(), t.header_texts()));
        assert_eq!(
            edits, 1,
            "an empty document resolves nothing — and drops nothing"
        );
        assert!(
            chips.iter().any(|c| c == "edits await a document"),
            "and the header says the edits are parked: {chips:?}"
        );

        // The real document arrives on a later delivery and resolves them.
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let cell = h
            .tile
            .read_with(&vcx, |t, _| t.model().rows[1].cells[1].clone());
        assert_eq!(cell.text.to_string(), "9.5000");
        assert!(cell.edited, "the restored edit is placed by label at last");
    }

    /// I-2, trace 2: the first delivery is a malformed generation that
    /// `MatrixModel::build` refuses (a repeated pivot pair). The model
    /// stays empty, so the restored draft must stay parked — rebasing
    /// against an unbuildable delivery's empty last-good model destroyed
    /// it just as silently.
    #[gpui::test]
    fn a_restored_draft_survives_a_first_delivery_that_cannot_be_built(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            first,
            // Two identical terms: a repeated pivot pair, refused.
            Arc::new(document_of(&["2026-10-16", "2026-10-16"], &NODES, BASE)),
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            1,
            "a refused build resolves nothing"
        );

        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let cell = h
            .tile
            .read_with(&vcx, |t, _| t.model().rows[1].cells[1].clone());
        assert_eq!(cell.text.to_string(), "9.5000");
        assert!(cell.edited);
    }

    /// I-2, trace 3: a restored edit whose row or column the delivered
    /// document no longer has is DROPPED — and the trader is told, as
    /// `:rebase` tells them on the same situation. It used to be pruned
    /// with nothing said at all.
    #[gpui::test]
    fn a_restored_edit_the_document_lacks_is_named_in_the_notice(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5], ["2099-01-01", "-1", 1.0]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));

        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c.contains("2099-01-01")),
            "the dropped pair is named: {chips:?}"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            1,
            "the one whose labels the document still has is kept"
        );
    }

    /// M-1 (final whole-branch review): once a restored draft has landed
    /// `Behind` with the newest generation painted (§8.7.17 — no base was
    /// ever delivered, so there is nothing to retain), a LATER delivery
    /// must not pin that painted generation as if it were the edits'
    /// base. It is not: the base is `2026-09-12T14:00:00Z`, which this
    /// session has never seen. Pinning it froze the panel on a generation
    /// that was neither the base nor the newest.
    #[gpui::test]
    fn a_restored_behind_draft_keeps_following_the_feed(cx: &mut gpui::TestAppContext) {
        const NEWEST: &str = "2026-09-12T14:15:00Z";
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_behind()),
            "a base this session never saw is Behind on the first delivery"
        );

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t1", "t2"], &NODES, NEWEST)),
        );
        assert_eq!(
            h.rows(&vcx),
            3,
            "the newest generation keeps painting: nothing here is the \
             edits' own base"
        );
    }

    /// M-2 (final whole-branch review): a generation that cannot be laid
    /// out as a grid must change NOTHING but the notice. The last good
    /// model stays on screen (it always did) and so must `self.snapshot`
    /// and the draft's own state — otherwise the panel goes `Behind`
    /// against a generation it never painted and `:rebase` has an
    /// unbuildable document to rebase onto.
    #[gpui::test]
    fn a_delivery_that_cannot_be_built_changes_nothing_but_the_notice(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t0"], &NODES, NEWER)),
        );

        let (state, rows, chips) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().rows.len(),
                t.header_texts(),
            )
        });
        assert_eq!(
            state,
            DraftState::Editing,
            "an unbuildable generation is not a delivery the draft heard about"
        );
        assert_eq!(rows, 2, "the last good model stays on screen");
        assert!(
            chips.iter().any(|c| c.contains("repeats")),
            "and the refusal is reported: {chips:?}"
        );
        // The draft is still against the painted generation, so `:bump`
        // (refused while `Behind`) still works.
        h.command(&mut vcx, "bump 0.1")
            .expect("the draft is not behind");
    }

    /// The other half of M-5's move: the delivery notice is cleared on
    /// the delivery that PAINTS (in `apply`) rather than in `deliver`'s
    /// `Ok` arm, so a select failure's message goes away exactly when the
    /// document that replaces it reaches the screen.
    #[gpui::test]
    fn a_painting_delivery_clears_the_previous_deliverys_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        h.deliver_err(&mut vcx, tag, "the document select failed");
        assert!(h.tile.read_with(&vcx, |t, _| t.notice().is_some()));

        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            None,
            "the document that replaced it is on screen"
        );
    }

    /// M-5 (final whole-branch review): the notice a delivery's own
    /// `apply` sets — the restored-draft dropped-edit report here —
    /// survives that delivery. The `Ok` arm used to clear `notice` before
    /// staging, so the clear now happens on the delivery that PAINTS, in
    /// `apply`, ahead of every notice `apply` itself writes.
    #[gpui::test]
    fn a_notice_set_while_applying_a_delivery_survives_it(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2099-01-01", "-1", 1.0]]
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c.contains("2099-01-01")),
            "the dropped-edit report is on screen after the delivery that \
             produced it: {chips:?}"
        );
    }
}
