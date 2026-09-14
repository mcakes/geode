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
//! Cell EDITING is Task 7 and the draft states (`Behind`, `:rebase`,
//! `:discard`) are Task 8. What is here for them is the plumbing they
//! need and nothing that pretends to be them: [`MarketDataTile::editor`]
//! is the insert-mode handle `key_context` already reports on, the
//! `:` grammar already parses their verbs, and the draft is already
//! restored, rebased, painted and serialised.

use crate::commands::{self, Command, KEY_DISPLAY_SEPARATOR};
use crate::core::{Draft, MatrixModel, PanelSpec};
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
    App, ClipboardItem, Context, Entity, IntoElement, ScrollStrategy, SharedString,
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
    /// Task 7 fills it; Part 3 only reports and paints it, so that the
    /// insert-mode fragment and the shell's own branch are wired and
    /// provable before the editing that depends on them lands.
    editor: Option<Entity<InputState>>,
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

    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
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
            // Task 7 owns the cell editor; the actions, the fragment's
            // insert context and the `editor` handle are here so that
            // what it has to build is the editing and not the wiring.
            "edit" | "commit" | "cancel" => {
                self.notice = Some("editing lands in Task 7".into());
                true
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
            // Parsed, not executed: the grammar a trader types is the one
            // Tasks 7 and 8 wire up, so a typo is still a typo here and
            // only a real verb answers this.
            Command::Revert => Err("revert lands in Task 7".into()),
            Command::Bump { .. } => Err("bump lands in Task 7".into()),
            Command::Rebase => Err("rebase lands in Task 8".into()),
            Command::Discard => Err("discard lands in Task 8".into()),
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
        let editor = self.editor.clone();
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
                            // The editor is painted IN the cursor cell
                            // (spec §8.3) — Task 7 is what puts one
                            // there; a `None` editor paints the text, as
                            // every other cell does.
                            el = el.child(match (at_cursor, &editor) {
                                (true, Some(state)) => d.child(Input::new(state)),
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

    /// The window root: renders the tile and holds what the harness reads
    /// back out of the window closure.
    struct Host {
        tile: Entity<MarketDataTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
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
        // The occupant's `content` is the harness's only handle on the
        // trait; the window closure can return just one value, so it is
        // parked here on the way out.
        let slot: Rc<RefCell<Option<Box<dyn TileContent>>>> = Rc::new(RefCell::new(None));
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
                    *slot.borrow_mut() = Some(occupant.content);
                    cx.new(|_| Host {
                        tile,
                        frame,
                        diagnostics,
                    })
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let (tile, frame, diagnostics) = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| {
            (h.tile.clone(), h.frame.clone(), h.diagnostics.clone())
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        (
            Harness {
                tile,
                content: slot.borrow_mut().take().expect("the factory built one"),
                frame,
                diagnostics,
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
    #[gpui::test]
    fn a_notice_reaches_the_header_and_escape_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        let chips = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.header_chips());
        let before = chips(&vcx);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(
            chips(&vcx),
            before,
            "a motion changes nothing in the header"
        );

        h.dispatch(&mut vcx, "edit", None);
        assert!(
            chips(&vcx).iter().any(|c| c == "editing lands in Task 7"),
            "a notice must reach the chips on the keystroke that set it: {:?}",
            chips(&vcx)
        );
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
    fn a_restored_draft_against_a_newer_generation_lands_behind(cx: &mut gpui::TestAppContext) {
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
}
