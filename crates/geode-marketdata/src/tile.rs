//! One market-data panel tile (market-data spec §8.2): asks for one
//! document by key through `DataHandle`, keeps the prepared
//! [`MatrixModel`] a frame paints from, and owns the cursor, the yank, the
//! `/` find and the `:` vocabulary over it.
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
use geode_core::document::split_key;
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::snapshot::Snapshot;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::fonts;
use geode_shell::frame::{Frame, FrameVersions};
use geode_shell::keymap::KeyContext;
use geode_shell::module::FindEvent;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, Focusable as _, IntoElement, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};
use std::cell::Cell as StdCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The row-label gutter's width, and one cell's. Fixed: a document's
/// columns are a ladder the desk chose (§8.2 — "no horizontal
/// virtualisation, since no sketched document has more than a few dozen
/// columns"), so every row is the same shape and `uniform_list` can lay
/// out only what is on screen.
const LABEL_WIDTH: f32 = 128.0;
const CELL_WIDTH: f32 = 84.0;

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

/// What one prepared header chip is painted as. The tone is resolved to a
/// theme colour at paint (never a stored colour, so a theme switch needs
/// no rebuild), and `Time` is the one tone whose colour depends on the
/// clock — the staleness reading, which is a comparison per frame and not
/// a format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Plain,
    Key,
    Time,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
struct Chip {
    text: SharedString,
    tone: Tone,
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
    /// The document key, in the dataset's declared `key` order. `None`
    /// until `:key` names one — a fresh panel has no document to ask
    /// about, and guessing one would paint a document the trader never
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
    /// `Rc`, not a plain `MatrixModel`: `render` hands this to the
    /// `uniform_list` closure on every paint — every shell repaint, not
    /// just this tile's own rebuilds — and a `MatrixModel` clone there
    /// would reallocate every row and bump every cell's `SharedString`
    /// per frame (the diagnostics tile's own MAJ-4, same shape). Replaced
    /// wholesale by `rebuild_model` and never mutated in place.
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
    /// `Draft` edit is keyed by.
    cursor: (usize, usize),
    scroll: UniformListScrollHandle,
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
    /// The prepared header, rebuilt by [`Self::changed`] — the one door
    /// every mutation on this tile ends at — so `render` clones
    /// refcounts and formats nothing.
    chips: Vec<Chip>,
    /// The painted generation's source time, parsed once beside its chip
    /// so the staleness rule is a comparison per frame rather than an
    /// RFC-3339 parse.
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = restored
            .and_then(|t| t.get("key"))
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
            scroll: UniformListScrollHandle::new(),
            editor: None,
            find: None,
            notice: None,
            stale_after,
            chips: Vec::new(),
            source_at: None,
            staged: None,
            last_flip: 0,
        };
        this.rebuild_chrome();
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
        acted.as_of != now.as_of || acted.data != now.data
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
                self.notice = None;
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
    fn apply(&mut self, snapshot: Arc<Snapshot>, _cx: &mut Context<Self>) {
        let as_of = snapshot
            .provenance()
            .datasets
            .first()
            .and_then(|f| f.as_of.clone());
        if let Some(as_of) = &as_of {
            self.draft.on_delivered(as_of);
        }
        if self.draft.is_behind() {
            // Keep painting the generation the edits were made against;
            // `snapshot` below still records the newer one for `:rebase`
            // (Task 8).
            if self.base_snapshot.is_none() {
                self.base_snapshot = self.snapshot.take();
            }
        } else {
            self.base_snapshot = None;
        }
        self.snapshot = Some(snapshot);
        self.rebuild_model();
        if self.unresolved_restore {
            self.unresolved_restore = false;
            // A restored draft's edits have no grid position until a model
            // resolves them by label (Task 5's own note on
            // `Draft::from_toml`). A draft that landed `Behind` is not
            // rebased here: moving edits onto a generation the trader has
            // not seen is exactly the decision `:rebase` exists to ask
            // for (§8.4).
            if !self.draft.is_behind() {
                self.draft.rebase(&self.model);
                self.rebuild_model();
            }
        }
        self.sync_scroll();
    }

    /// Apply a staged snapshot, if any — from the `flip` bump in the frame
    /// observer, or from this panel's own `deliver` when its arrival was
    /// the one that emptied the barrier. A no-op with nothing staged, so
    /// calling it on every `flip` costs nothing.
    ///
    /// A staged snapshot is only ever valid for the flip identity it was
    /// staged under: a second mutation inside the same window replaces the
    /// barrier before this panel's requery for the NEWER versions lands,
    /// and the `flip` that releases that newer barrier must not promote
    /// the older snapshot. Dropping it keeps what is already on screen
    /// (last-good); the requery already in flight paints the real answer.
    fn promote(&mut self, cx: &mut Context<Self>) {
        let Some((snapshot, versions)) = self.staged.take() else {
            return;
        };
        if versions.same_flip_identity(self.frame.read(cx).versions()) {
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
    fn rebuild_model(&mut self) {
        let Some(snapshot) = self.painted_snapshot() else {
            self.model = Rc::new(MatrixModel::empty(
                self.spec,
                self.key.as_deref().unwrap_or(&[]),
            ));
            self.clamp_cursor();
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

    fn sync_scroll(&self) {
        self.scroll
            .scroll_to_item(self.cursor.0, ScrollStrategy::Nearest);
    }

    /// The one door every mutation ends at: re-prepare the header (which
    /// formats, and so must never happen in `render`) and notify.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_chrome();
        cx.notify();
    }

    /// Prepare the header's chips (spec §8.2): the title and key, each
    /// header attribute, the generation's source time, the draft's own
    /// line, and the notice.
    fn rebuild_chrome(&mut self) {
        self.chips.clear();
        match &self.key {
            Some(key) => {
                self.chips.push(Chip {
                    text: format!("{} {}", self.spec.title, display_key(key)).into(),
                    tone: Tone::Key,
                });
            }
            None => {
                self.chips.push(Chip {
                    text: format!("{} — no key — :key <value>", self.spec.title).into(),
                    tone: Tone::Warn,
                });
            }
        }
        for (label, value) in &self.model.header {
            self.chips.push(Chip {
                text: format!("{label}: {value}").into(),
                tone: Tone::Plain,
            });
        }
        self.source_at = self
            .model
            .source_time
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        if let Some(at) = self.source_at {
            // Local, like every other displayed time in this codebase
            // (Phase 4a's ruling).
            self.chips.push(Chip {
                text: at
                    .with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
                    .into(),
                tone: Tone::Time,
            });
        }
        if self.key.is_some() && self.model.rows.is_empty() {
            self.chips.push(Chip {
                text: format!(
                    "no document received for {}",
                    display_key(self.key.as_deref().unwrap_or(&[]))
                )
                .into(),
                tone: Tone::Warn,
            });
        }
        let summary = self.draft.summary();
        if !summary.is_empty() {
            self.chips.push(Chip {
                text: summary.into(),
                tone: Tone::Warn,
            });
        }
        if let Some(notice) = &self.notice {
            self.chips.push(Chip {
                text: notice.clone(),
                tone: Tone::Error,
            });
        }
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
        self.sync_scroll();
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
        self.rebuild_model();
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
    fn revert(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft.is_empty() {
            return Err("no edits to revert".to_string());
        }
        self.draft.revert();
        self.rebuild_model();
        self.changed(cx);
        Ok(())
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
        self.rebuild_model();
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
        let snapshot = self
            .snapshot
            .clone()
            .expect("`Behind` implies a newer generation was delivered");
        let newer_model = MatrixModel::build(&snapshot, self.spec, &Draft::default())?;
        let (_, dropped) = self.draft.rebase(&newer_model);
        self.base_snapshot = None;
        self.notice = if dropped.is_empty() {
            None
        } else {
            Some(dropped_notice(&dropped).into())
        };
        self.rebuild_model();
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
        self.base_snapshot = None;
        self.notice = None;
        self.rebuild_model();
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
        self.sync_scroll();
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
        // the door a `key` line's own catalog request rides — the next
        // completion list is then the fresh one (spec §8.3's "completions
        // from the catalog's keys").
        //
        // UNCONDITIONALLY, not `request_catalog_if_needed` (review fix
        // round 1, MIN-4, controller ruling): a held catalog listing this
        // dataset is not a FRESH one, and documents arrive while the panel
        // is open — a subscribed feed publishes a new key every few
        // seconds. Gated on staleness, the panel asked once and then
        // offered a completion list that could never grow. `set_visible`'s
        // own request stays gated: there, one catalog is as good as
        // another and the point is only to have one at all.
        if line.split_whitespace().next() == Some("key") {
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
            let n = self.draft.len();
            let plural = if n == 1 { "" } else { "s" };
            return Err(format!("{n} edit{plural} pending — :revert first"));
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
        self.rebuild_model();
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
                "key".into(),
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
    /// behind it.
    #[cfg(test)]
    pub(crate) fn header_chips(&self) -> Vec<String> {
        self.chips.iter().map(|c| c.text.to_string()).collect()
    }

    /// The row `scroll_to_item` was last asked to show —
    /// `logical_scroll_top_index` answers from the still-pending deferred
    /// scroll when one is queued, which is exactly the state right after
    /// a cursor move and before the next paint consumes it.
    #[cfg(test)]
    pub(crate) fn scroll_target(&self) -> usize {
        self.scroll.logical_scroll_top_index()
    }
}

/// A document key in the panel's own typeable spelling (`SPX.Z`,
/// `SPX.Z/EOD`) — the storage separator is unprintable, so it is never
/// what a trader sees or types.
fn display_key(key: &[String]) -> String {
    key.join(&KEY_DISPLAY_SEPARATOR.to_string())
}

/// `:rebase`'s notice about the edits it could not carry over — a row or
/// column label the newer document no longer has. Each pair is spelled
/// `row/col` (the same display separator a document key uses), since a
/// bare pair of labels with nothing between them reads as one run-on word.
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
        // Copied out of the theme before anything else borrows `cx`, and
        // `Hsla` is `Copy`: the per-cell decision below is then a compare
        // and a copy, never a lookup or an allocation.
        let (foreground, muted_foreground, warning, warning_foreground, danger) = (
            theme.foreground,
            theme.muted_foreground,
            theme.warning,
            theme.warning_foreground,
            theme.danger,
        );
        let (accent, muted, border) = (theme.accent, theme.muted, theme.border);

        let stale = self.is_stale(chrono::Utc::now());
        let mut header = h_flex()
            .w_full()
            .h(px(22.))
            .items_center()
            .gap_3()
            .px_2()
            .text_sm()
            .text_color(muted_foreground)
            .border_b_1()
            .border_color(border)
            .debug_selector(|| format!("marketdata-header-{}", self.id.0));
        for chip in &self.chips {
            let colour = match chip.tone {
                Tone::Plain => muted_foreground,
                Tone::Key => foreground,
                Tone::Time if stale => warning,
                Tone::Time => muted_foreground,
                Tone::Warn => warning_foreground,
                Tone::Error => danger,
            };
            header = header.child(div().text_color(colour).child(chip.text.clone()));
        }
        if stale {
            header = header.child(div().text_color(warning).child("stale"));
        }

        let label_w = px(LABEL_WIDTH);
        let cell_w = px(CELL_WIDTH);
        // The column strip: a fixed header row above the list, in the
        // data face so its labels line up with the cells beneath them.
        let mut strip = h_flex()
            .w_full()
            .h(px(20.))
            .items_center()
            .px_2()
            .text_xs()
            .font_family(fonts::MONO)
            .text_color(muted_foreground)
            .border_b_1()
            .border_color(border)
            .child(div().w(label_w).child(SharedString::from(self.spec.rows)));
        for column in &self.model.columns {
            strip = strip.child(
                div()
                    .w(cell_w)
                    .text_right()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(column.clone()),
            );
        }

        // One `Rc` clone, one `Option<Entity>` clone and two `usize`s into
        // the closure — never the model itself (a `MatrixModel` clone
        // here would reallocate every row on every paint).
        let model = self.model.clone();
        // The cell the editor is ON travels with it, rather than the paint
        // reading the cursor: the two are the same under every binding
        // (insert mode binds nothing that moves a cursor), but a palette
        // dispatch can move one under an open editor, and a commit writes
        // to the cell it OPENED on — so painting at the cursor would show
        // the input over a cell it is not about to write.
        let editor = self.editor.as_ref().map(|e| (e.cell, e.state.clone()));
        let cursor = self.cursor;
        let list = uniform_list(
            "marketdata-rows",
            self.model.rows.len(),
            move |range, _window, _cx| {
                range
                    .map(|i| {
                        let row = &model.rows[i];
                        let mut el = h_flex().w_full().px_2().text_xs().child(
                            div()
                                .w(label_w)
                                .font_family(fonts::MONO)
                                .text_color(foreground)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(row.label.clone()),
                        );
                        for (c, cell) in row.cells.iter().enumerate() {
                            let at_cursor = (i, c) == cursor;
                            let mut d = div()
                                .w(cell_w)
                                .font_family(fonts::MONO)
                                .text_right()
                                .whitespace_nowrap()
                                .overflow_hidden();
                            // `sent` is checked first: a sent cell is
                            // also an edited one (the draft keeps its
                            // edits until the echo clears them, §9.4),
                            // and what it needs to say is that it is out
                            // the door.
                            d = if at_cursor {
                                d.bg(accent).text_color(foreground)
                            } else if cell.sent {
                                d.bg(muted).text_color(muted_foreground)
                            } else if cell.edited {
                                d.bg(warning.opacity(0.25)).text_color(warning_foreground)
                            } else {
                                d.text_color(foreground)
                            };
                            // The editor is painted IN the cell it edits
                            // (spec §8.3), and painted is what makes it
                            // typeable at all (`Editing::state`); every
                            // other cell paints its text.
                            el = el.child(match &editor {
                                Some((at, state)) if *at == (i, c) => d.child(Input::new(state)),
                                _ => d.child(cell.text.clone()),
                            });
                        }
                        el.into_any_element()
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .debug_selector(|| format!("marketdata-rows-{}", self.id.0));

        v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0))
            .child(header)
            .child(strip)
            .child(list)
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
                t.header_chips(),
            )
        });
        assert_eq!(rows, 2, "two terms down the side");
        assert_eq!(columns, 3, "three nodes across the top");
        assert!(
            chips.iter().any(|c| c == "CVI SPX.Z"),
            "the title and the key: {chips:?}"
        );
        assert!(
            chips.iter().any(|c| c == "spot_ref: 5000"),
            "each header attribute: {chips:?}"
        );
        let local = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string();
        assert!(
            chips.contains(&local),
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

        let chips = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.header_chips());
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
            h.tile.read_with(&vcx, |t, _| t
                .header_chips()
                .iter()
                .any(|c| c == "CVI NDX.Z")),
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
            Err("1 edit pending — :revert first".to_string()),
            "the count is spelled as the header spells it, and the verb is named"
        );
        let (key, edits) = h
            .tile
            .read_with(&vcx, |t, _| (t.serialize(), t.draft().len()));
        assert_eq!(edits, 1, "the edit is still there");
        assert_eq!(
            key.get("key").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            key["key"][0].as_str(),
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

        // Read the queued scroll inside the same update as the dispatch:
        // a real draw consumes the deferred `scroll_to_item`, after which
        // the handle answers the (unsized) test window's own offset.
        let target = vcx.update(|window, cx| {
            h.content
                .dispatch(&ActionId("marketdata::bottom".into()), None, window, cx);
            h.tile.read(cx).scroll_target()
        });
        assert_eq!(target, terms.len() - 1, "G scrolls the last row into view");
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
key = ["SPX.Z"]
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
            "the key and the draft survive a restart, labels and all"
        );
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
            .read_with(&vcx, |t, _| (t.draft().state.clone(), t.header_chips()));
        assert!(
            matches!(state, DraftState::Behind { .. }),
            "a newer generation under a restored draft is Behind, got {state:?}"
        );
        assert!(
            chips
                .iter()
                .any(|c| c.starts_with("newer document received")),
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
            h.tile
                .read_with(&vcx, |t, _| t.header_chips())
                .iter()
                .any(|c| c.contains("1 edit")),
            "the header counts it: {:?}",
            h.tile.read_with(&vcx, |t, _| t.header_chips())
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
                .read_with(&vcx, |t, _| t.header_chips())
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
            !h.tile
                .read_with(&vcx, |t, _| t.header_chips())
                .iter()
                .any(|c| c.contains("edit")),
            "and the header says nothing about a draft: {:?}",
            h.tile.read_with(&vcx, |t, _| t.header_chips())
        );
        assert_eq!(
            h.command(&mut vcx, "revert"),
            Err("no edits to revert".to_string()),
            "a second revert has nothing to do and says so"
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
        let chips = h.tile.read_with(&vcx, |t, _| t.header_chips());
        assert!(
            chips
                .iter()
                .any(|c| c == &format!("newer document received {local}")),
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
        let chips = h.tile.read_with(&vcx, |t, _| t.header_chips());
        assert!(
            chips
                .iter()
                .any(|c| c == &format!("newer document received {local}")),
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

        let chips = h.tile.read_with(&vcx, |t, _| t.header_chips());
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
        let chips = h.tile.read_with(&vcx, |t, _| t.header_chips());
        assert!(
            !chips.iter().any(|c| c.contains("edit")),
            "no draft chip left: {chips:?}"
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
}
