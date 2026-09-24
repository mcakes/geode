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
//! `data` — narrowed by a publication watch to this dataset and document key. `flip` is
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
//! silently lost) and starts painting it; `:revert` drops the edits and
//! shows the newer document, clean. Both are refused outside `Behind`
//! (there is no "newer" to move onto), and so are `edit`/`:bump`
//! (controller ruling 2026-09-14) — an edit made now would be keyed
//! against a grid `:rebase` is about to move away from underneath it.

use crate::commands::{self, BumpAxis, Command, KEY_DISPLAY_SEPARATOR};
use crate::core::cursor::{self, Cursor, Grid, Motion};
use crate::core::draft::{RowDelete, RowEdit, bumped, local_hhmm};
use crate::core::matrix::RowState;
use crate::core::menu::{self, MenuInputs, MenuRow};
use crate::core::spec::RowIdentity;
use crate::core::{
    Cell, CellKind, Columns, DateTimeField, Draft, DraftBadge, FieldKey, MatrixModel, PanelSpec,
    Precision, Segment, SegmentText, UpdatePolicy, attr_text, parse_attr, parse_cell, route,
};
use crate::delegate::{DelegateChoice, DelegateEditor, DelegateEditorPaint, MatrixDelegate};
use crate::header::{self, HeaderInputs, HeaderModel};
use crate::popup::{
    ChoicePopup, MenuState, PickerRows, PickerState, Popup, render_menu, render_picker,
};
use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use geode_core::document::{Value, split_key};
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::schema::ColumnType;
use geode_core::snapshot::Snapshot;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{Frame, FrameVersions, PublicationWatch};
use geode_shell::keymap::KeyContext;
use geode_shell::module::{FindEvent, StackHandle};
use geode_shell::shell::colours::{to_hsla, to_rgb};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use geode_shell::vimnav::NavCommand;
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable as _, Hsla, IntoElement,
    KeyDownEvent, SharedString, Window, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::table::{DataTable, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme, v_flex};
use std::cell::Cell as StdCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many rows `ctrl+d`/`ctrl+u` step — `vimnav`'s own ±5, the same
/// fixed offset every list in this codebase uses, multiplied by the count
/// prefix rather than being viewport-relative.
const HALF_PAGE: isize = 5;

/// How many rows `ctrl+f`/`ctrl+b`/`pagedown`/`pageup` step — `vimnav`'s
/// own ±10, the blotter's `page_down_full`.
const FULL_PAGE: isize = 10;

/// `/` over the row labels (spec §8.3). The vim jump model only: a
/// document's rows ARE its axis, in the desk's own order, so narrowing
/// them (`FindStyle::Fzf`) would hide rows a cell reference is counted
/// against — the panel moves its cursor instead, which is what
/// `find_match` is for.
pub struct FindState {
    /// Where the cursor was when `/` opened; `escape` returns here. The
    /// whole `Cursor`, not a row (final review, T4): a find started from
    /// the attribute strip must cancel back INTO the strip, and a row
    /// alone would land it on grid row 0.
    origin: Cursor,
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
/// `primary_text` (2026-09-19) is the date field's active-segment text:
/// `primary_foreground` floored against `primary` itself, the solid fill
/// it sits on — seven bundled themes (Gruvbox Light at 2.19:1, Ayu Light,
/// Everforest Light, Catppuccin Latte, Flexoki Light, Asciinema,
/// Spaceduck) ship the pair under 3:1. The floor moves lightness toward
/// pure black or pure white, whichever contrasts more with `primary` —
/// not toward the theme's own `foreground`, which on three light themes
/// (Everforest Light's grey on its mid green) is itself under 3:1 against
/// `primary`, leaving `readable_on` no `t` that clears. Every colour
/// clears 3:1 against one of black and white, so this floor always lands.
///
/// Memoised, not derived per frame: `readable_on` is a 16-step bisection
/// through OKLab, and PHILOSOPHY §6 forbids that per chip per frame.
/// `key` is EVERY colour `derive` reads and nothing else — the blotter's
/// theme-signature rule at the scale of six inputs — so a theme switch
/// recomputes on its first frame and every other frame is one compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FlooredTones {
    key: [Hsla; 6],
    pub(crate) warn: Hsla,
    pub(crate) error: Hsla,
    pub(crate) primary_text: Hsla,
}

impl FlooredTones {
    pub(crate) fn derive(theme: &Theme) -> Self {
        let (bg, fg) = (to_rgb(theme.background), to_rgb(theme.foreground));
        let floor = |c: Hsla| to_hsla(readable_on(to_rgb(c), bg, fg));
        let primary = to_rgb(theme.primary);
        let black = Rgb {
            r: 0.0,
            g: 0.0,
            b: 0.0,
        };
        let white = Rgb {
            r: 1.0,
            g: 1.0,
            b: 1.0,
        };
        let toward = if contrast_ratio(black, primary) >= contrast_ratio(white, primary) {
            black
        } else {
            white
        };
        Self {
            key: Self::key(theme),
            warn: floor(theme.warning),
            error: floor(theme.danger),
            primary_text: to_hsla(readable_on(
                to_rgb(theme.primary_foreground),
                primary,
                toward,
            )),
        }
    }

    fn key(theme: &Theme) -> [Hsla; 6] {
        [
            theme.background,
            theme.foreground,
            theme.warning,
            theme.danger,
            theme.primary,
            theme.primary_foreground,
        ]
    }

    /// Re-derive only when one of the six inputs moved.
    pub(crate) fn refresh(&mut self, theme: &Theme) {
        if self.key != Self::key(theme) {
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
/// and not how it got there. `:rebase`/`:revert` are the only doors
/// forward, and the notice names both.
const BEHIND_REFUSED: &str = "the draft is behind — :rebase or :revert first";

/// What every edit door answers on a `Deleted` row (dividend spec §5.2):
/// the row is still painted, struck through, so the cursor can land on
/// it, but a value written into a row the draft is about to remove would
/// be an edit with nowhere to go. `:revert` is the door back, and the
/// notice names it.
pub(crate) const DELETED_REFUSED: &str = "row is deleted — :revert restores it";

/// What `:rebase`/`:revert` answer outside `Behind` — there is no
/// "newer" document to move onto or fall back to.
const NOT_BEHIND: &str = "nothing to rebase — the draft is on the live document";

/// What the row verbs (`o`, `shift+o`, `d d`; dividend spec §5.3) answer
/// with the cursor in the attribute strip: an attribute is not a row, so
/// there is nothing to insert beside or delete.
const NOT_A_ROW: &str = "not a row";

/// What `d d` answers on a row already marked `Deleted` (spec §5.3): the
/// mark is idempotent, so the notice names the door back rather than
/// pretending a second mark did something.
const ALREADY_DELETED: &str = "row is already deleted — :revert restores it";

/// One cell a `:bump` writes: where it is, the labels that make the edit
/// portable across generations, and the value being added to — the shape
/// [`Draft::bump`] consumes.
type BumpCell = ((usize, usize), (String, String), f64, ColumnType);

/// The open cell or attribute editor (spec §8.6/§5.2): the input the
/// trader is typing into, and what it was opened on.
struct Editing {
    state: EditorState,
    target: EditTarget,
}

/// The two forms an editor takes (header spec §5.2, 2026-09-19; a cell
/// opens either by its column's [`CellKind`] since spec §4.4).
enum EditorState {
    /// A text `Input` — every `Number`/`Text` cell, and every attribute
    /// but a `Date` (a `Choice` cell opens `Popup::Choice` instead).
    /// Tile-owned, and PAINTED in the cell or the strip (see `render` and
    /// `MatrixDelegate::render_td`): gpui installs a text-input handler
    /// only for a focused `Input` that has been drawn, so an editor kept
    /// off the element tree would take no characters at all.
    Text(Entity<InputState>),
    /// The segmented date field a `Date` attribute or a `Date` cell opens
    /// instead: a pure [`DateTimeField`] the tile routes keys into
    /// ([`MarketDataTile::date_field_key`]), its own focus handle (what
    /// makes the shell's insert branch see a non-shell focus and what
    /// `holds_focus` answers off), and the three segment strings prepared
    /// on every key so `render` formats nothing.
    Date {
        field: DateTimeField,
        focus: FocusHandle,
        paint: DateFieldPaint,
    },
}

impl EditorState {
    /// Whether this editor's own focusable — the `Input`'s handle or the
    /// date field's — holds window focus.
    fn is_focused(&self, window: &Window, cx: &App) -> bool {
        match self {
            EditorState::Text(state) => state.read(cx).focus_handle(cx).is_focused(window),
            EditorState::Date { focus, .. } => focus.is_focused(window),
        }
    }
}

/// The date field as painted: the segments exactly as
/// [`DateTimeField::segments`] answers them (`text` a `SharedString`,
/// already carrying which one is active and mid-typing) plus the
/// per-tile selector the painter needs — all prepared by
/// [`DateFieldPaint::of`] whenever the field changes, never in `render`,
/// so the painter (`geode_widgets::datefield::paint`) and the delegate
/// mirror both take them straight through with NO allocation of their
/// own: `segments` is an `Rc<[SegmentText]>` (one clone is a refcount
/// bump, not a `Vec` copy) and `selector` a `SharedString` (an inline or
/// refcounted copy, never a `format!` per render). `Clone` — three
/// refcounts at most — because the delegate mirrors it into the cell it
/// paints (`crate::delegate::DelegateEditorPaint`).
#[derive(Clone)]
pub(crate) struct DateFieldPaint {
    pub segments: Rc<[SegmentText]>,
    pub selector: SharedString,
}

impl DateFieldPaint {
    fn of(field: &DateTimeField, tile_id: u64) -> Self {
        Self {
            segments: field.segments().into(),
            selector: format!("marketdata-date-seg-{tile_id}").into(),
        }
    }
}

/// What `header::render` paints in the editor slot of the attribute being
/// edited.
pub(crate) enum EditorPaint<'a> {
    Text(&'a Entity<InputState>),
    Date {
        paint: &'a DateFieldPaint,
        focus: &'a FocusHandle,
    },
}

/// What an open [`Editing`] was opened on.
#[derive(Clone)]
enum EditTarget {
    Cell {
        /// The cell this editor was opened on, captured rather than read
        /// back off the cursor at commit time.
        cell: (usize, usize),
        /// That cell's labels when the editor opened.
        ///
        /// The grid can move underneath an open editor: a delivery lands
        /// while a trader is typing, a shorter generation clamps the
        /// cursor (`clamp_cursor`), and a commit that wrote to whatever
        /// the cursor now points at would file a typed number against a
        /// different term. `commit` compares these against the model's
        /// CURRENT pair for the same cell and refuses when they differ —
        /// one comparison, at the one moment the answer matters, rather
        /// than a cancel-on-delivery path (which `promote` could not
        /// take: it runs from the frame observer, where there is no
        /// `Window` to blur).
        labels: (SharedString, SharedString),
    },
    Attr {
        /// The attribute's index into `model.header` when the editor
        /// opened — a paint-time position, not an identity.
        index: usize,
        /// The attribute's column name — its real identity, checked
        /// against the model at commit time exactly as a cell's labels
        /// are (a header attribute's own "did the grid move" rule).
        column: SharedString,
    },
    /// The row-label editor a `Typed` axis opens on `o`/`shift+o`
    /// (dividend spec §5.3): a provisional row the draft holds under its
    /// minted `label`, painted in the row-label column, whose commit
    /// RENAMES it to what the trader typed.
    RowLabel {
        /// The row's model index when the editor opened — the paint-time
        /// position, checked against `label` at commit as a cell's
        /// labels are.
        row: usize,
        /// The minted label the row is filed under in the draft — its
        /// identity until the commit renames it, and what
        /// [`MarketDataTile::close_editor`] drops if the editor closes
        /// with the row still provisional.
        label: SharedString,
    },
}

/// What `commit_attr_edit` is handed: text still to be parsed (the text
/// editor, `:set`) or a value already known valid (the date field).
enum AttrInput<'a> {
    Text(&'a str),
    Value(Value),
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
    query_in_flight: bool,
    publication: Option<PublicationWatch>,
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
    /// until `:rebase`/`:revert` (Task 8).
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
    /// What a different generation does to a draft with edits (spec
    /// §8.4, 2026-09-19): `Hold` is today's `Behind`; `Rebase` and
    /// `Replace` are applied by [`Self::apply`] at the moment `Behind`
    /// would otherwise be entered, so a switch never acts retroactively
    /// on a draft already `Behind`. Set by `:auto`, the menu's `On new
    /// document` rows and the three `marketdata::auto_*` actions; carried
    /// in the session as `auto` when not the default.
    policy: UpdatePolicy,
    /// A restored draft's edits are parked out of every grid's range
    /// (`Draft::from_toml`, which stores label pairs and not indices), so
    /// they paint nowhere until a model resolves them. Set at
    /// construction and cleared by the first delivery, which is the one
    /// that has a model to rebase against.
    ///
    /// `set_key` SETS it, to whether the draft it installs is non-empty
    /// (2026-09-19, per-underlying drafts): a switch back to an
    /// underlying with a parked draft is a restore in every respect —
    /// the same label-pair form, the same resolution against the first
    /// non-empty built model, the same "first delivery is `hold`" rule —
    /// and a switch to one without is an empty draft with nothing to
    /// resolve. Either way it can never be left set against a document
    /// its labels did not come from.
    unresolved_restore: bool,
    /// Every OTHER underlying's unsent draft, keyed by document key
    /// (user ruling 2026-09-19, "keep them per underlying"): a switch
    /// parks the current draft here as `Draft::to_toml`'s label-pair
    /// table — the session's own portable form, which is what makes
    /// grid indices irrelevant across documents — and a switch back
    /// removes the entry and installs it through the restore path. The
    /// table IS the parked form: nothing here is parsed until it is
    /// needed (a picker's row marks, a session write), so a parked draft
    /// costs no model, no snapshot and no cursor. Every draft verb
    /// (`:revert`, `:bump`, `:set`, a cell edit) acts on `draft` — the
    /// CURRENT underlying's — alone; the header's dirty dot likewise.
    parked: BTreeMap<Vec<String>, toml::Table>,
    /// A grid cell — the same index a `Draft` edit is keyed by, and the
    /// truth the table's own selection mirrors (never the other way
    /// round) — or an attribute in the header strip (spec 2026-09-14
    /// §5.1).
    cursor: Cursor,
    /// The grid column the cursor left from on entering the strip —
    /// `cursor::step`'s own memory, kept here because the tile is what
    /// owns the cursor across motions (`j` reads it back to return to the
    /// same column; `0` before the strip has ever been entered).
    last_grid_col: usize,
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
    /// The tile-owned popup — `.`/`⋯` open `Menu` (spec §6.1); Task 7
    /// adds `Picker`. `None` most of the time, so most frames pay nothing
    /// for it beyond the tag check.
    popup: Option<Popup>,
    /// The `⋯` button's tooltip selector (`"tip-marketdata-menu-button-
    /// {id}"`), built once here — it depends only on the tile id, never
    /// per render — and passed into [`header::render`].
    menu_tip_selector: SharedString,
    /// The `Behind` state run's tooltip selector (`"tip-marketdata-
    /// state-{id}"`), built once alongside `menu_tip_selector`.
    state_tip_selector: SharedString,
    /// This tile's place in its stack (tile-stacks spec §5.1), painted in
    /// the header (Task 9); `None` while not a stack member.
    stack: Option<StackHandle>,
    /// [`Self::title`]'s cache (whole-branch review, Minor 5), same
    /// shape as the blotter's own `title` field — a pure function of
    /// `spec.title` and `key`, replaced only in `set_key`, never
    /// `format!`-ed in `title()` itself.
    title: SharedString,
    /// The `AppClock` global (as-of dialog spec §6.1), read once at
    /// construction and refreshed by an `observe_global::<AppClock>`
    /// handler — carried here, not read fresh from every formatting
    /// site, so `HeaderModel::prepare` stays a pure function of its
    /// `HeaderInputs` and `menu::rows` of its own arguments. `pub(crate)`
    /// so tests read it directly (`t.clock`) rather than reaching for the
    /// global themselves.
    pub(crate) clock: geode_core::clock::Clock,
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
        // Every underlying's unsent draft rides the session as
        // `[drafts.<display key>]` (spec §8.5, 2026-09-19). The restored
        // underlying's own entry is the CURRENT draft, installed exactly
        // as the legacy single `draft` key was; every other entry stays
        // parked, as a table, until its underlying is loaded. `draft`
        // itself is still read — a session written before this change —
        // and means the current underlying's draft, but only when no
        // `drafts` entry already speaks for it.
        let mut parked: BTreeMap<Vec<String>, toml::Table> = restored
            .and_then(|t| t.get("drafts"))
            .and_then(|v| v.as_table())
            .map(|drafts| {
                drafts
                    .iter()
                    .filter_map(|(k, v)| {
                        let parts = parse_display_key(k);
                        if parts.is_empty() {
                            return None;
                        }
                        Some((parts, v.as_table()?.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let draft = key
            .as_ref()
            .and_then(|k| parked.remove(k))
            .or_else(|| {
                restored
                    .and_then(|t| t.get("draft"))
                    .and_then(|v| v.as_table())
                    .cloned()
            })
            .map(|t| Draft::from_toml(&t))
            .unwrap_or_default();
        // An unknown or missing `auto` is the default, never a refusal:
        // a session file is the trader's own layout and a tile that
        // fails to open over one word in it is worse than one that
        // holds.
        let policy = restored
            .and_then(|t| t.get("auto"))
            .and_then(|v| v.as_str())
            .and_then(UpdatePolicy::parse)
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
        // The delegate holds the tile WEAKLY (the table is the tile's
        // own field, so a strong handle would be a cycle) for the date
        // field's key and click routing, and its own copy of the floored
        // tones for that field's active segment.
        let tones = FlooredTones::derive(cx.theme());
        let weak_tile = cx.weak_entity();
        let table = cx.new(|cx| {
            TableState::new(
                MatrixDelegate::new(spec, weak_tile, id.0, tones),
                window,
                cx,
            )
            .row_selectable(true)
            .col_selectable(false)
            .cell_selectable(true)
            .row_header(false)
            .loop_selection(false)
            .col_resizable(false)
            .col_movable(false)
            .sortable(false)
        });
        // The mouse's part in this panel: a click selects a cell — the
        // cursor moves to it, a click on the row-label column moves the
        // row and leaves the column alone — and a DOUBLE-click opens the
        // editor on that cell (user ruling 2026-09-17, reversing the
        // 2026-09-14 "editing is keyboard-only" ruling), exactly as `i`
        // would: the same refusals (`Behind`, no document), and none at
        // all on the row-label column, where there is no cell to edit.
        //
        // What made the mapping honourable is a SHELL rule, not anything
        // here: every tile mouse-down re-arms the shell's
        // `pending_focus_restore` (CLAUDE.md's focus rule), and
        // `ShellView::render` now withholds the restore while a tile's
        // occupant holds the keyboard in insert mode
        // (`occupant_holds_insert_focus`) — so the editor `begin_edit`
        // focuses on the click keeps it past the next frame. The table
        // emits `SelectCell` and then `DoubleClickedCell` for the second
        // click of a pair, so the first arm below has already cancelled
        // any open editor by the time the second opens one.
        //
        // `SelectRow`/`SelectColumn` are deliberately not matched:
        // `sync_cursor` emits both, so matching them would re-enter this
        // handler on every cursor move.
        //
        // `subscribe_in` (and so a `Window`) for the cancel and the open.
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            match event {
                TableEvent::SelectCell(row, col) => {
                    // A click while the cell editor is open CANCELS it
                    // (controller ruling 2026-09-14, review Minor 5) —
                    // through `close_editor`, so blur then drop, and never
                    // a commit: a click is not `enter`, and silently
                    // writing a half-typed number because the trader
                    // clicked elsewhere is the one outcome nobody asked
                    // for. Cancelling is not optional either: left open,
                    // the editor would sit painted on the cell the cursor
                    // just left while `mode == insert` is still claimed.
                    if this.editor.is_some() {
                        this.close_editor(window, cx);
                        // `cancel`'s own chrome step in `dispatch`: the
                        // header is re-prepared once, off the render
                        // thread.
                        this.changed(cx);
                    }
                    let col = this.table.read(cx).delegate().model_col(*col);
                    this.cursor_to(*row, col, cx)
                }
                TableEvent::DoubleClickedCell(row, col) => {
                    if let Some(col) = this.table.read(cx).delegate().model_col(*col) {
                        this.cursor_to(*row, Some(col), cx);
                        this.begin_edit(window, cx);
                        // `dispatch`'s own tail: the delegate paints the
                        // editor only once `sync_cursor` has handed it over.
                        this.sync_cursor(cx);
                        // `edit`'s own chrome step: the header paints the
                        // notice a refusal leaves.
                        this.changed(cx);
                    }
                }
                _ => {}
            }
        })
        .detach();
        cx.observe(&frame, |this, _frame, cx| {
            // A flip released (Phase 4 §3.10): promote whatever is staged,
            // and do it REGARDLESS of visibility — a panel hidden between
            // staging and the flip must not come back showing the old
            // generation. `flip` is checked here and never in
            // `follows_changed`: it means "you may promote", never
            // "requery" (CLAUDE.md).
            let now = this.versions(cx);
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
        cx.observe(&diagnostics, |this, _diagnostics, cx| {
            // A fresh catalog only matters LIVE while the picker is open
            // (spec §7): `completions()` already reads the catalog
            // pull-style at call time, and a closed panel gets a fresh
            // one the next time it opens (`open_picker`'s own
            // re-request) — so most notifications from this entity
            // (a source's health, say) cost this one `matches!` and
            // nothing else.
            if !matches!(this.popup, Some(Popup::Picker(_))) {
                return;
            }
            let all = this.catalog_keys(cx);
            let Some(Popup::Picker(p)) = &mut this.popup else {
                return;
            };
            // Review fix round 1, IMPORTANT-2: this observer fires on
            // EVERY notification this entity emits, not only a catalog
            // change (a source's health ticks about twice a second with
            // a diagnostics tile open) — comparing first is what keeps an
            // unrelated notification from resetting the highlight and
            // from re-cloning the catalog into `labels` for nothing.
            if p.rows.all() == all.as_slice() {
                return;
            }
            // Review fix round 2: `replace_all` captures the highlighted
            // KEY STRING before `all` is overwritten and re-places by it
            // (`PickerRows::replace_all`'s own doc comment has the full
            // story).
            p.rows.replace_all(all);
            cx.notify();
        })
        .detach();
        // `AppClock` (as-of dialog spec §6.1): refresh the tile's own
        // reading and re-prepare the header — the field the header's
        // freshness/`Behind` text and the `⋯` menu's "Rebase onto …" row
        // all read — then notify, the same shape every other mutation
        // that reaches `rebuild_chrome` follows (`changed`, just above).
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = cx
                .try_global::<geode_shell::clock::AppClock>()
                .map(|c| c.0)
                .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
            this.rebuild_chrome();
            cx.notify();
        })
        .detach();

        let mut this = MarketDataTile {
            id,
            spec,
            frame,
            diagnostics,
            data,
            model: Rc::new(MatrixModel::empty(spec, key.as_deref().unwrap_or(&[]))),
            title: Self::compute_title(spec, key.as_deref()),
            unresolved_restore: !draft.is_empty(),
            parked,
            key,
            tag: 0,
            acted: None,
            query_in_flight: false,
            publication: None,
            visible: false,
            snapshot: None,
            base_snapshot: None,
            draft,
            policy,
            cursor: Cursor::Cell { row: 0, col: 0 },
            last_grid_col: 0,
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
                badge: DraftBadge::Clean,
                state: None,
                incomplete: None,
                notice: None,
                time: None,
                stale: false,
            },
            source_at: None,
            staged: None,
            last_flip: 0,
            tones,
            popup: None,
            menu_tip_selector: format!("tip-marketdata-menu-button-{}", id.0).into(),
            state_tip_selector: format!("tip-marketdata-state-{}", id.0).into(),
            stack: None,
            clock: cx
                .try_global::<geode_shell::clock::AppClock>()
                .map(|c| c.0)
                .unwrap_or_else(|| geode_core::clock::Clock::machine().0),
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
    /// `insert` while the cell editor holds the keyboard OR the
    /// underlying picker or a choice cell's typeahead is open (spec
    /// §7/§8.6, dividend spec §4.4 — a `Popup::Picker`'s or
    /// `Popup::Choice`'s field holds the keyboard exactly as the cell
    /// editor does, which is the shell's insert branch's own one pair to
    /// key on), `menu` exactly
    /// while the action list is open (spec §6.1; `editor` wins over
    /// `popup` since the two are exclusive by construction, see
    /// `toggle_menu`/`open_picker`), else `normal`. `counts()` stays on in
    /// every mode deliberately: stopping a typed `3` from becoming a
    /// count prefix is the shell's job there, not this context's.
    ///
    /// **No context distinguishes the picker from the plain cell editor
    /// (controller ruling, superseding this crate's own earlier attempt
    /// at one).** The picker's highlight moves on bare `up`/`down` alone
    /// (the neutral `insert_up`/`insert_down` pair, which nudges the
    /// editor's number instead when the editor is what is open —
    /// `dispatch` tells the two apart, not the context), deliberately
    /// not a chord: CLAUDE.md's standing rule for a module's
    /// insert-mode field is that a shipped chord (`ctrl+k` is the
    /// palette) still fires from inside it, because a chord resolves
    /// against the WHOLE context stack and a module must never take a
    /// shell chord away — narrowing that shadow to "only while the picker
    /// is open" still takes the palette away exactly when a trader is
    /// typing in the picker, which is no better than taking it from the
    /// cell editor. Spec §7 named `ctrl+j`/`ctrl+k` in error; Task 8
    /// records the amendment.
    pub fn key_context(&self) -> KeyContext {
        let mode = if self.editor.is_some()
            || matches!(self.popup, Some(Popup::Picker(_) | Popup::Choice(_)))
        {
            "insert"
        } else if matches!(self.popup, Some(Popup::Menu(_))) {
            "menu"
        } else {
            "normal"
        };
        KeyContext::new("marketdata").pair("mode", mode).counts()
    }

    /// `TileContent::holds_focus` (review C-1, 2026-09-17): does THIS
    /// panel's open editor, its picker's field or a choice popup's field
    /// hold window focus? The
    /// mode above says an editor is OPEN; this says whose field the
    /// keyboard is actually in — two different facts once an editor has
    /// been left open by a tile-focus move (I-3), and the shell's
    /// insert-focus predicate needs the second.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        let editor = self
            .editor
            .as_ref()
            .is_some_and(|e| e.state.is_focused(window, cx));
        let popup = match &self.popup {
            Some(Popup::Picker(p)) => p.input.read(cx).focus_handle(cx).is_focused(window),
            Some(Popup::Choice(c)) => c.input.read(cx).focus_handle(cx).is_focused(window),
            Some(Popup::Menu(_)) | None => false,
        };
        editor || popup
    }

    // ---- the request -------------------------------------------------

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame.read(cx).versions_for(&self.publication)
    }

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
        // An unrelated notification is not an answer to the query this
        // barrier is already waiting for.
        if self.query_in_flight
            && self
                .acted
                .is_some_and(|acted| acted.same_flip_identity(now))
        {
            return;
        }
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
        let batch = geode_core::document::join_key(&document_key);
        if self
            .publication
            .as_ref()
            .is_none_or(|watch| !watch.matches(self.spec.dataset, Some(&batch)))
        {
            self.publication = Some(self.frame.update(cx, |frame, _| {
                frame.watch_publications(self.spec.dataset, Some(&batch))
            }));
        }
        // A fresh question always supersedes whatever was staged for the
        // old one, whether or not `promote`'s own version check would
        // have caught it.
        self.staged = None;
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions_for(&self.publication))
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
        self.query_in_flight = queued;
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
            self.query_in_flight = false;
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
        self.query_in_flight = false;
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
        let moved = as_of.as_ref().is_some_and(|t| draft.on_delivered(t));
        // The update policy (spec §8.4, 2026-09-19) is applied HERE and
        // only here: `on_delivered` is the `hold` decision, and `Behind`
        // after it means "today's code would hold" — a draft with edits
        // met a different generation (a further one under an
        // already-`Behind` draft included, so a `rebase`/`replace` panel
        // that was left `Behind` under `hold` moves onto the NEWEST on
        // the next delivery, never retroactively on the switch). A clean
        // draft and the base's own round trip never reach this branch.
        // The notice is decided now and written at the commit point
        // below, ahead of the restore block's own, so it is never wiped
        // by the "clear on the delivery that paints" rule.
        //
        // **Gated on `moved` — a real TRANSITION, never a redelivery**
        // (review I-1): this panel requeries on every `data` bump (any
        // dataset's publish, every few seconds on the demo bus), and a
        // draft already `Behind` reads `Behind` again on every one of
        // those same-generation redeliveries. Gated on the state alone,
        // `:auto replace` was a `:revert` executed by an unrelated
        // publish seconds after the switch, and the restore rule below
        // protected a draft for exactly one data bump. `on_delivered`
        // answers `true` for `Editing → Behind` and `Behind{a} →
        // Behind{b}` alone, so a redelivery of the generation the draft
        // is already behind leaves it there, `:rebase`/`:revert` its
        // doors, until the next NEW generation.
        //
        // **The first delivery after a restore is always `hold`**
        // (ruling 2026-09-19): the policy governs LIVE deliveries while
        // the trader is working, and a draft restored from the session
        // has not been seen this session at all — `replace` dropping it
        // on a delivery nobody was watching breaks §8.5's "unsent work
        // survives a restart" with only a notice for company, and
        // `rebase` would move edits onto a generation the trader never
        // chose. So while `unresolved_restore` is set the delivery takes
        // the `hold` path (a differing base lands `Behind`, a matching
        // one resolves through the restore block below as today), and
        // the policy resumes from the next delivery.
        let mut notice: Option<SharedString> = None;
        if moved
            && draft.is_behind()
            && self.policy != UpdatePolicy::Hold
            && !self.unresolved_restore
        {
            match self.policy {
                UpdatePolicy::Hold => unreachable!("guarded above"),
                UpdatePolicy::Rebase => {
                    // The rebase guard (spec §2, amendment 4) needs the
                    // OUTGOING document's own group sizes before this
                    // rebase moves `draft` onto the incoming one —
                    // `painted_snapshot` is exactly what is on screen
                    // right now (still `self.base_snapshot`/
                    // `self.snapshot`, neither overwritten yet), and
                    // `capture_groups_if_base` is what confirms it is
                    // really this draft's base and not a fallback (its own
                    // doc comment has the M-1 story). Skipped in silence
                    // otherwise, the same "leave groups as they were" the
                    // restore path below takes.
                    self.capture_groups_if_base(&mut draft);
                    // Two builds, on purpose: `Draft::rebase` re-places
                    // the edits by the NEW document's row and column
                    // labels, which only a model of that document
                    // carries — so the first build is against a clean
                    // draft for its labels alone (what `:rebase` does),
                    // and the second, below, paints the re-placed edits.
                    // A refusal of either changes nothing but the notice.
                    let clean = match MatrixModel::build(&snapshot, self.spec, &Draft::default()) {
                        Ok(model) => model,
                        Err(e) => {
                            self.notice = Some(e.into());
                            return;
                        }
                    };
                    // An EMPTY new document (no rows for this key at
                    // this as-of — `compile_document`'s `and false` arm)
                    // is not a document to move edits onto: rebasing
                    // against an empty label map drops every edit in
                    // silence (review I-2, the restore block's own
                    // guard below). The draft stays `Behind` — the
                    // `hold` path, no extra notice — and `:rebase`
                    // remains the trader's explicit door.
                    if !clean.rows.is_empty() {
                        let (_, dropped) = draft.rebase(&clean);
                        if !dropped.is_empty() {
                            notice = Some(dropped_notice(&dropped).into());
                        }
                    }
                }
                UpdatePolicy::Replace => {
                    // Counted BEFORE the revert — the notice is the whole
                    // disclosure of unsent work gone by the trader's own
                    // standing choice, and it must say what went.
                    let phrase = draft.count_phrase();
                    draft.revert();
                    // `Behind` implies `on_delivered` ran with `Some`.
                    let when = as_of
                        .as_deref()
                        .map(|t| local_hhmm(t, self.clock))
                        .unwrap_or_default();
                    notice = Some(format!("update {when} replaced {phrase}").into());
                }
            }
        }
        // While `Behind`, keep painting the generation the edits were made
        // against; `snapshot` below still records the delivered one for
        // `:rebase` (Task 8). Retained only when the OUTGOING snapshot
        // really IS that base (M-1): a restored draft lands `Behind` with
        // its base never delivered at all, and pinning whatever happened
        // to be painted froze the panel on a generation that was neither
        // the base nor the newest, with the header naming a third. Under
        // `rebase`/`replace` the draft is no longer `Behind` by here, so
        // nothing is retained and the new document is painted.
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
        // ahead of every notice this method itself writes below; the
        // policy's own notice (`None` under `hold`) is what it is cleared
        // TO, so a `replace` disclosure is never lost to the clear.
        self.notice = notice;
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
                // Against the DOCUMENT's own grid, never `self.model`:
                // that one was built WITH this draft, so it already
                // carries the draft's inserted rows spliced in — `rebase`
                // would read each as a row the document now carries and
                // drop it as a conflict (§5.1), and would key every cell
                // edit by its post-splice position rather than the
                // document position `Draft::edits` holds. The same rule
                // `:rebase` and the update policy's `rebase` arm follow.
                // One extra build, once per restore, never per delivery.
                // Not `Behind`, so the painted snapshot is the delivered
                // one; it built a moment ago, so this cannot fail, and a
                // failure would leave the draft parked (the honest state)
                // rather than rebased against nothing.
                let clean = self
                    .painted_snapshot()
                    .and_then(|s| MatrixModel::build(&s, self.spec, &Draft::default()).ok());
                let Some(clean) = clean else {
                    self.unresolved_restore = true;
                    self.install_model(cx);
                    return;
                };
                let (_, dropped) = self.draft.rebase(&clean);
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
        if !Self::differs_on_followed(versions, self.versions(cx)) {
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
            let now = self.versions(cx);
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
            self.query_in_flight = false;
        }
        self.changed(cx);
    }

    pub fn set_stack(&mut self, stack: Option<StackHandle>, cx: &mut Context<Self>) {
        self.stack = stack;
        cx.notify();
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn compute_title(spec: &PanelSpec, key: Option<&[String]>) -> SharedString {
        match key {
            Some(k) => format!("{} · {}", spec.title, display_key(k)).into(),
            None => spec.title.into(),
        }
    }

    // ---- the model ---------------------------------------------------

    /// The snapshot on screen: the draft's own base generation while one
    /// is retained, else the newest delivered.
    fn painted_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.base_snapshot.clone().or_else(|| self.snapshot.clone())
    }

    /// Capture `draft`'s same-day group sizes (spec §2's rebase guard,
    /// amendment 4) against the snapshot on screen right now — but ONLY
    /// when that snapshot really IS `draft`'s own base generation, never
    /// merely `painted_snapshot()`'s best guess (review finding, controller
    /// ruling: a capture site captures only when the model it is about to
    /// count was built from a snapshot whose source time equals
    /// `draft.base`).
    ///
    /// `painted_snapshot()` falls back to the NEWEST delivered snapshot
    /// when no base is retained — the M-1 path in `apply`: a restored or
    /// parked draft whose base generation was never delivered this
    /// session at all. Trusting that fallback here would silently replace
    /// a previously correct `groups` (captured against the true base, by
    /// an earlier call or carried in from the session) with the NEWER
    /// document's own sizes, disarming the guard it exists to run — a
    /// same-day group that in truth changed size between the draft's real
    /// base and the newer document would then read as unchanged. Skipped
    /// in silence otherwise: leaving `groups` untouched is always the safe
    /// choice, since a stale-but-correct-for-its-generation count only
    /// ever makes the guard MORE willing to refuse, never less.
    ///
    /// Every capture site in this file calls this rather than
    /// `Draft::capture_groups` directly, so the rule lives in one place.
    fn capture_groups_if_base(&self, draft: &mut Draft) {
        let Some(base) = self.painted_snapshot() else {
            return;
        };
        if source_time_of(&base) != draft.base {
            return;
        }
        if let Ok(base_model) = MatrixModel::build(&base, self.spec, &Draft::default()) {
            draft.capture_groups(&base_model);
        }
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
    /// **Every model swap ends here**, and the `refresh` is the reason:
    /// the pinned release (`gpui-component-0.6.2/src/table/state.rs`)
    /// caches each `column()`'s answer in `col_groups` at prepare time
    /// and paints its HEADER from that cache alone, so a document whose
    /// node ladder changed would keep the previous one's headers (and
    /// lay its cells out at the previous widths) until something else
    /// happened to refresh. The same trap CLAUDE.md records for the
    /// blotter's gutter.
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

    /// The grid and strip shape `cursor::step`/`clamp` reason about —
    /// this panel's whole cursor vocabulary, in one small `Copy` value.
    fn grid(&self) -> Grid {
        Grid {
            rows: self.model.rows.len(),
            cols: self.model.columns.len(),
            attrs: self.model.header.len(),
        }
    }

    /// Keep the cursor inside the grid or the strip — a new generation can
    /// be shorter than the one it replaces (or lose an attribute), and a
    /// cursor left past its end would yank, edit and paint nothing.
    fn clamp_cursor(&mut self) {
        self.cursor = cursor::clamp(self.cursor, self.grid());
    }

    /// Mirror the cursor and the open editor into the delegate, and move
    /// the table's own selection to match — which is also what keeps the
    /// cursor row and column in view (`set_selected_row` and
    /// `set_selected_col` each scroll, non-strictly, so a cell already on
    /// screen never jumps).
    ///
    /// While the cursor is in the strip (`Cursor::Attr`) the delegate
    /// mirrors NO selection at all (`clear_selection`) — the pinned
    /// component's own clear door, confirmed at implementation time — so
    /// the grid paints no highlighted row behind an attribute edit.
    ///
    /// In the grid, the column is shifted by one: the table's column 0 is
    /// the row-label column, which the cursor never enters. The column is
    /// set BEFORE the row deliberately — each setter switches the
    /// component's selection mode, and the row highlight is painted only
    /// in row mode, so ending on the row is what makes the panel read
    /// like the blotter (a highlighted row plus a bordered cursor cell)
    /// rather than painting nothing at all.
    fn sync_cursor(&self, cx: &mut Context<Self>) {
        let editor = self.delegate_editor();
        let choice = self.delegate_choice();
        match self.cursor {
            Cursor::Cell { row, col } => self.table.update(cx, |t, cx| {
                let d = t.delegate_mut();
                d.cursor = Some((row, col));
                d.editor = editor;
                d.choice = choice;
                let table_col = d.table_col(col);
                t.set_selected_col(table_col, cx);
                t.set_selected_row(row, cx);
                t.scroll_to_row(row, cx);
            }),
            Cursor::Attr(_) => self.table.update(cx, |t, cx| {
                let d = t.delegate_mut();
                d.cursor = None;
                d.editor = editor;
                d.choice = choice;
                t.clear_selection(cx);
            }),
        }
    }

    /// The open editor as the delegate paints it: a CELL editor in either
    /// form (the text `Input`, or the date field's prepared segments and
    /// focus handle), `None` for an attribute editor, which the header
    /// paints itself (`render`), and `None` with nothing open.
    fn delegate_editor(&self) -> Option<DelegateEditor> {
        let e = self.editor.as_ref()?;
        // A row-label editor sits in the row-label column (`col: None`),
        // which `render_td`'s label arm paints through the same
        // `render_editor` a cell's uses.
        let (row, col) = match &e.target {
            EditTarget::Cell { cell, .. } => (cell.0, Some(cell.1)),
            EditTarget::RowLabel { row, .. } => (*row, None),
            EditTarget::Attr { .. } => return None,
        };
        let paint = match &e.state {
            EditorState::Text(state) => DelegateEditorPaint::Text(state.clone()),
            EditorState::Date { paint, focus, .. } => DelegateEditorPaint::Date {
                paint: paint.clone(),
                focus: focus.clone(),
            },
        };
        Some(DelegateEditor { row, col, paint })
    }

    /// The open choice popup as the delegate paints it (dividend spec
    /// §4.4): the cell it hangs under and the tile's own prepared paint —
    /// an `Rc` bump, never a re-preparation. `None` with no popup, or a
    /// popup of another kind.
    fn delegate_choice(&self) -> Option<DelegateChoice> {
        match &self.popup {
            Some(Popup::Choice(c)) => {
                let (cell, _) = c.target();
                Some(DelegateChoice {
                    row: cell.0,
                    col: cell.1,
                    paint: c.paint(),
                })
            }
            Some(Popup::Menu(_) | Popup::Picker(_)) | None => None,
        }
    }

    /// Re-mirror the open editor and choice popup alone — what a date
    /// CELL's field needs after every keystroke and segment click, since
    /// the delegate paints its own COPY of the segments
    /// (`DelegateEditorPaint::Date`) and the field's own key path ends in
    /// neither `dispatch` nor [`Self::sync_cursor`]; and what the choice
    /// popup needs after a keystroke in its field, a hover or a close
    /// from its own mouse door, for the same reason. Deliberately not
    /// `sync_cursor` itself: that door also re-sets the table's
    /// selection, and the pinned `set_selected_row` stops propagation of
    /// the event in flight, which a key the field did NOT consume must
    /// still reach the shell.
    fn sync_editor(&self, cx: &mut Context<Self>) {
        let editor = self.delegate_editor();
        let choice = self.delegate_choice();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.editor = editor;
            d.choice = choice;
            cx.notify();
        });
    }

    /// Move the cursor to a clicked cell — the mouse's form of §8.3's
    /// motions, clamped into the grid. `col: None` is a click on the
    /// row-label column: the row moves and the column stays where it was
    /// (`last_grid_col` when the click arrives from the strip, since
    /// there is no grid column of the cursor's own yet).
    fn cursor_to(&mut self, row: usize, col: Option<usize>, cx: &mut Context<Self>) {
        if self.model.rows.is_empty() {
            return;
        }
        let row = row.min(self.model.rows.len().saturating_sub(1));
        let current_col = match self.cursor {
            Cursor::Cell { col, .. } => col,
            Cursor::Attr(_) => self.last_grid_col,
        };
        let col = col
            .unwrap_or(current_col)
            .min(self.model.columns.len().saturating_sub(1));
        self.cursor = Cursor::Cell { row, col };
        self.sync_cursor(cx);
        cx.notify();
    }

    /// Move the cursor to an attribute in the header strip — the mouse's
    /// form of `k` (spec §5.1: "a click on an attribute value moves the
    /// cursor to `Attr(i)` and opens nothing"). `header::render` attaches
    /// this to each attribute value's own mouse-down.
    ///
    /// An open cell editor is CANCELLED first (final review, B4) — the
    /// `SelectCell` handler's own rule, and for the same reason: this
    /// mouse-down has already re-armed the shell's focus restore, so an
    /// editor left open would be painted on a cell the cursor just left,
    /// deaf, with `mode == insert` still claimed. Never a commit: a click
    /// is not `enter`. That is the only reason this takes a `Window`.
    pub(crate) fn cursor_to_attr(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let attrs = self.model.header.len();
        if attrs == 0 {
            return;
        }
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        self.cursor = Cursor::Attr(i.min(attrs - 1));
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// An attribute value's mouse-down with its click count
    /// (`header::render` attaches this): every press is
    /// [`Self::cursor_to_attr`], and the second press of a pair ALSO
    /// opens the editor on that attribute (user ruling 2026-09-17,
    /// reversing 2026-09-14's "editing is keyboard-only") — `i`'s exact
    /// path, refusals included. The strip focuses nothing of its own, so
    /// the editor `begin_edit` focuses here holds the keyboard when the
    /// shell's tile-level listener arms its focus restore on this same
    /// press, and `ShellView::render`'s insert-mode rule withholds it.
    pub(crate) fn attr_clicked(
        &mut self,
        i: usize,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor_to_attr(i, window, cx);
        if click_count == 2 && matches!(self.cursor, Cursor::Attr(_)) {
            self.begin_edit(window, cx);
            self.sync_cursor(cx);
            self.changed(cx);
        }
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
            incomplete: self.draft.incomplete_rows(self.spec, &self.model.columns),
            clock: self.clock,
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
        // "Any other dispatched action closes the popup first, then
        // runs" (spec §6.1) — the one rule that keeps the popup from
        // needing the shell's modal machinery. The five menu verbs are
        // the popup's own grammar and must not close it out from under
        // themselves; `commit`/`cancel` join them (Task 7) because with
        // a picker open they route to IT rather than closing it —
        // `key_context` reports `insert` while one is open, which is
        // exactly what puts `commit`/`cancel` in a trader's hand for it.
        // The four `insert_*` verbs join them too (2026-09-17): with a
        // picker open they ARE its highlight step, so closing it first
        // would leave them nothing to move.
        //
        // `close_popup_with_window`, never plain `close_popup`: this is
        // reachable with a `Popup::Picker` open (any OTHER action, an
        // ordinary cursor motion say), and its field holds the keyboard —
        // dropping it unblurred would leave `Window::focused` pointing at
        // a dead input for the rest of the session (`close_editor`'s own
        // rule).
        if !matches!(
            verb,
            "menu"
                | "menu_down"
                | "menu_up"
                | "menu_pick"
                | "menu_close"
                | "commit"
                | "cancel"
                | "insert_up"
                | "insert_down"
                | "insert_up_big"
                | "insert_down_big"
        ) && self.popup.is_some()
        {
            self.close_popup_with_window(window, cx);
        }
        let n = count.unwrap_or(1).max(1) as isize;
        // Whether this action touched something the HEADER paints (review
        // fix round 1, MIN-5, extended by Task 5 to the strip): a motion
        // that STAYS in the grid, a yank and an `n`/`N` step must not
        // re-prepare the chips — `changed` formats, and a held `j` would
        // then format the whole header per keystroke for a row number
        // nothing shows. A motion that crosses into or out of the strip
        // is the one exception: the strip's own cursor border is header
        // paint (spec §5.1).
        let chrome = match verb {
            "down" | "up" | "left" | "right" | "page_down" | "page_up" | "page_down_full"
            | "page_up_full" | "top" | "bottom" | "first_col" | "last_col" => {
                let motion = match verb {
                    "down" => Motion::Rows(n),
                    "up" => Motion::Rows(-n),
                    "left" => Motion::Cols(-n),
                    "right" => Motion::Cols(n),
                    "page_down" => Motion::Rows(HALF_PAGE * n),
                    "page_up" => Motion::Rows(-HALF_PAGE * n),
                    // `vimnav`'s ±10, the blotter's `page_down_full`.
                    "page_down_full" => Motion::Rows(FULL_PAGE * n),
                    "page_up_full" => Motion::Rows(-FULL_PAGE * n),
                    "top" => Motion::Top,
                    "bottom" => Motion::Bottom,
                    "first_col" => Motion::FirstCol,
                    _ => Motion::LastCol,
                };
                let was_attr = matches!(self.cursor, Cursor::Attr(_));
                let grid = self.grid();
                self.cursor = cursor::step(self.cursor, &mut self.last_grid_col, motion, grid);
                was_attr != matches!(self.cursor, Cursor::Attr(_))
            }
            "yank" | "yank_row" | "yank_col" => {
                let what = match verb {
                    "yank" => Yank::Cell,
                    "yank_row" => Yank::Row,
                    _ => Yank::Col,
                };
                // `yc` in the strip has no column to yank (spec §5.1): a
                // notice, not a silent no-op — the one yank that touches
                // the header at all.
                if what == Yank::Col && matches!(self.cursor, Cursor::Attr(_)) {
                    self.notice = Some("nothing to yank in a column here".into());
                    true
                } else {
                    if let Some(text) = self.yank_text(what) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    false
                }
            }
            "edit" => {
                self.begin_edit(window, cx);
                true
            }
            "commit" => match self.popup {
                Some(Popup::Picker(_)) => {
                    self.commit_picker(window, cx);
                    false
                }
                Some(Popup::Choice(_)) => self.commit_choice(window, cx),
                Some(Popup::Menu(_)) | None => self.commit_edit(window, cx),
            },
            "cancel" => {
                // A picker or choice popup takes priority over the
                // (otherwise absent) editor: the three are exclusive by
                // construction, so this is really "whichever of them is
                // open, if any".
                if matches!(self.popup, Some(Popup::Picker(_) | Popup::Choice(_))) {
                    self.close_popup_with_window(window, cx);
                    false
                } else {
                    // Only when there WAS an editor: `marketdata::cancel`
                    // is bound in insert mode alone, so a normal-mode
                    // arrival is the palette's, and it has nothing to say.
                    match self.editor.is_some() {
                        true => {
                            self.close_editor(window, cx);
                            true
                        }
                        false => false,
                    }
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
            "menu" => {
                self.toggle_menu(window, cx);
                false
            }
            "menu_close" => {
                // Only when there WAS one open — the same "only when
                // there was one" rule `escape` above keeps: reachable
                // from the palette with nothing open, and that has
                // nothing to report.
                if self.popup.is_none() {
                    return false;
                }
                // A bare action id, so nothing upstream promises it can
                // only ever reach a `Menu` — the one door blurs first if
                // it finds a Picker.
                self.close_popup_with_window(window, cx);
                false
            }
            "menu_down" | "menu_up" => {
                let delta = if verb == "menu_down" { n } else { -n };
                match &mut self.popup {
                    Some(Popup::Menu(m)) => {
                        m.highlighted = menu::step(&m.rows, m.highlighted, delta);
                    }
                    Some(Popup::Picker(p)) => p.rows.step_highlighted(delta),
                    // Reachable from the palette alone (`mode == menu` is
                    // never reported with a choice popup open), and it
                    // has nothing to move there.
                    Some(Popup::Choice(_)) | None => {}
                }
                false
            }
            // The insert-mode arrow pair (2026-09-17), whose meaning
            // follows which input is open: the picker's highlight step
            // while the picker holds the keyboard (`_big` is the same one
            // step — a list has no "big"), the choice popup's highlight
            // step while IT does (a bare step wraps, §20.5's one motion
            // rule — the picker's clamp is header spec §7's own), a nudge
            // of the editor's number while the cell or attribute editor
            // does, and nothing at all with none (the palette can reach
            // these; a `false` there is "not handled", not a silent
            // success).
            "insert_up" | "insert_down" | "insert_up_big" | "insert_down_big" => {
                let up = verb.starts_with("insert_up");
                match &mut self.popup {
                    Some(Popup::Picker(p)) => {
                        p.rows.step_highlighted(if up { -n } else { n });
                        false
                    }
                    Some(Popup::Choice(c)) => {
                        c.list
                            .nav(NavCommand::Move((if up { -n } else { n }) as i64));
                        c.prepare();
                        false
                    }
                    Some(Popup::Menu(_)) => return false,
                    None if self.editor.is_some() => {
                        let magnitude = if verb.ends_with("_big") { 10 } else { 1 };
                        let steps = (if up { magnitude } else { -magnitude }) * n as i64;
                        self.nudge(steps, window, cx)
                    }
                    None => return false,
                }
            }
            "menu_pick" => {
                if let Some(Popup::Menu(m)) = &self.popup {
                    let index = m.highlighted;
                    self.menu_pick(index, window, cx);
                }
                false
            }
            "revert" => {
                if let Err(e) = self.revert(cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            "rebase" => {
                if let Err(e) = self.rebase(cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            "load_underlying" => {
                self.open_picker(window, cx);
                false
            }
            // `space`/`shift+space` on a choice cell (dividend spec §4.4):
            // the next/previous option, in place, no popup.
            "step" | "step_back" => {
                let delta = if verb == "step" { n } else { -n };
                if let Err(e) = self.step_choice(delta, window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            // The row verbs (dividend spec §5.3): `o`/`shift+o` insert a
            // row below/above the cursor row, `d d` deletes it. Every
            // one touches what the header paints — the dirty dot, the
            // incomplete chip, or a refusal.
            "insert_below" | "insert_above" => {
                if let Err(e) = self.insert_row(verb == "insert_below", window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            "delete_row" => {
                if let Err(e) = self.delete_row(window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            // The menu's `On new document` rows and the palette's
            // `Auto: …` actions. Nothing the header paints reads the
            // policy, so no chrome rebuild.
            "auto_hold" => {
                self.set_policy(UpdatePolicy::Hold, cx);
                false
            }
            "auto_rebase" => {
                self.set_policy(UpdatePolicy::Rebase, cx);
                false
            }
            "auto_replace" => {
                self.set_policy(UpdatePolicy::Replace, cx);
                false
            }
            // `upload` is Part 4 — parsed and registered today so the
            // palette and a keymap already reach it (spec §6.2), so
            // dispatching it answers honestly rather than pretending it
            // does nothing.
            "upload" => {
                self.notice = Some("not built yet".into());
                true
            }
            _ if self
                .spec
                .actions
                .iter()
                .any(|a| a.id.strip_prefix("marketdata::") == Some(verb)) =>
            {
                // Every `KindAction` in this slice is `built: false`
                // (spec §6.3): when a future one is built it becomes an
                // egress REQUEST designed in its own slice, never
                // computation this crate performs.
                self.notice = Some("not built yet".into());
                true
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

    /// The attribute strip's own [`Self::edit_base`]: an attribute needs
    /// no row or column, only a header to belong to, which the model
    /// carries exactly when it carries any rows at all (see
    /// `MatrixModel::build`'s early return for an empty document).
    fn attr_edit_base(&self) -> Result<String, String> {
        if self.model.header.is_empty() {
            return Err(NO_DOCUMENT.to_string());
        }
        Ok(self.model.source_time.clone().unwrap_or_default())
    }

    /// `marketdata::edit` (`i`/`enter`): open an editor in the cursor cell
    /// or, on `Cursor::Attr`, in the strip — seeded with what is already
    /// painted there (the draft's own value where one has been made,
    /// since that is what `MatrixModel::build`/`header_of` painted) — and
    /// give it the keyboard.
    ///
    /// WHICH editor is the column's [`CellKind`] (spec §4.4): a `Date`
    /// cell opens the segmented date field exactly as a `Date` attribute
    /// does, a `Number` or `Text` cell the text `Input`, and a `Choice`
    /// cell the typeahead popup ([`Self::open_choice`]) — after the same
    /// refusals as the other two, since a popup over a `Behind` draft or
    /// an empty grid would be exactly as dishonest as an editor. The two
    /// text-or-date decisions — a cell's kind, an attribute's declared
    /// type — resolve to the one `wants_date` below, so the field opens
    /// the same way from either.
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
        let (text, target, wants_date) = match self.cursor {
            Cursor::Cell { row, col } => {
                if let Err(e) = self.edit_base() {
                    self.notice = Some(e.into());
                    return;
                }
                // A deleted row's cells refuse edits (dividend spec
                // §5.2) — here, ahead of the editor and the choice popup
                // alike, so nothing opens over a row that is going.
                if self.model.rows[row].state == RowState::Deleted {
                    self.notice = Some(DELETED_REFUSED.into());
                    return;
                }
                let cell = (row, col);
                let text = self.model.rows[row].cells[col].text.clone();
                let labels = self.model.label_of(cell);
                if let Some(CellKind::Choice(options)) = self.model.kind_of(col) {
                    self.open_choice(cell, labels, &text, options, window, cx);
                    return;
                }
                let wants_date = matches!(self.model.kind_of(col), Some(CellKind::Date));
                (text, EditTarget::Cell { cell, labels }, wants_date)
            }
            Cursor::Attr(i) => {
                if let Err(e) = self.attr_edit_base() {
                    self.notice = Some(e.into());
                    return;
                }
                let Some(attr) = self.model.header.get(i) else {
                    self.notice = Some(NO_DOCUMENT.into());
                    return;
                };
                let wants_date = self
                    .spec
                    .header
                    .iter()
                    .find(|a| a.column == attr.column.as_ref())
                    .is_some_and(|a| a.ty == ColumnType::Date);
                (
                    attr.text.clone(),
                    EditTarget::Attr {
                        index: i,
                        column: attr.column.clone(),
                    },
                    wants_date,
                )
            }
        };
        let state = if wants_date {
            // A `Date` attribute or cell opens the segmented field (header
            // spec §5.2, 2026-09-19; spec §4.4), seeded with the painted
            // date — or, when that does not parse (a NULL painted empty,
            // say), today's date on the trader's clock, so the field
            // always opens on something a step or a digit can act on.
            let date = chrono::NaiveDate::parse_from_str(text.as_ref(), "%Y-%m-%d")
                .unwrap_or_else(|_| self.clock.today(chrono::Utc::now()));
            let field = DateTimeField::open(
                date.and_hms_opt(0, 0, 0).expect("midnight exists"),
                Precision::Date,
                Segment::Day,
            );
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            let paint = DateFieldPaint::of(&field, self.id.0);
            EditorState::Date {
                field,
                focus,
                paint,
            }
        } else {
            let state = cx.new(|cx| InputState::new(window, cx));
            state.update(cx, |s, cx| s.set_value(text, window, cx));
            state.read(cx).focus_handle(cx).focus(window, cx);
            EditorState::Text(state)
        };
        self.editor = Some(Editing { state, target });
        self.notice = None;
    }

    /// The date field's own keys (header spec §5.2, 2026-09-19), run from
    /// its `on_key_down` in `header::render` — which sits on the focused
    /// element and so runs BEFORE the shell root's listener. Answers
    /// whether the key was consumed; the listener stops propagation on
    /// `true`, so the shell never also resolves it. A chord (ctrl, alt or
    /// cmd) is never consumed: it reaches the shell exactly as it does
    /// from any editor (`ctrl+k` still opens the palette). Shift alone is
    /// the arrows' "ten steps", the number nudge's own rule.
    /// `geode_widgets::datefield::route` is the ONE key table this
    /// consults — the panel just computes `chord` and matches the two
    /// arms (`Commit`/`Cancel`) it alone owns, `field.apply` the rest.
    ///
    /// `enter` and `escape` are ALSO bound by the fragment
    /// (`marketdata::commit`/`cancel`) for the shell's dispatch: both
    /// doors end in the same `commit_edit`/`close_editor`, so which one a
    /// keystroke reaches cannot change what it does.
    pub(crate) fn date_field_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        let chord = modifiers.control || modifiers.alt || modifiers.platform;
        let Some(key) = route(event.keystroke.key.as_str(), modifiers.shift, chord) else {
            return false;
        };
        let Some(Editing {
            state: EditorState::Date { field, paint, .. },
            ..
        }) = self.editor.as_mut()
        else {
            return false;
        };
        match key {
            FieldKey::Commit => {
                self.commit_edit(window, cx);
                self.sync_cursor(cx);
                self.changed(cx);
                return true;
            }
            FieldKey::Cancel => {
                self.close_editor(window, cx);
                self.sync_cursor(cx);
                self.changed(cx);
                return true;
            }
            other => {
                field.apply(other);
            }
        }
        *paint = DateFieldPaint::of(field, self.id.0);
        // A date CELL's segments are painted from the delegate's copy.
        self.sync_editor(cx);
        // A keystroke that changed the field retires a standing refusal
        // (`finish the day or backspace`, say) — the notice is header
        // chrome, so that one case re-prepares it.
        if self.notice.take().is_some() {
            self.changed(cx);
        } else {
            cx.notify();
        }
        true
    }

    /// A click on one of the field's segments (`header::render` attaches
    /// this): the mouse form of `left`/`right`. It also takes the keyboard
    /// back for the field when the field has lost it (review M4) — an
    /// editor orphaned by a tile-focus move (I-3) is still open, and a
    /// click on its segment is the trader asking to type into it again. A
    /// module focusing its OWN handle, never the shell's (CLAUDE.md's
    /// focus rule); the shell's insert-focus rule then withholds its
    /// restore exactly as it does after `begin_edit`.
    pub(crate) fn date_segment_clicked(
        &mut self,
        segment: Segment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Editing {
            state:
                EditorState::Date {
                    field,
                    paint,
                    focus,
                },
            ..
        }) = self.editor.as_mut()
        {
            field.select(segment);
            *paint = DateFieldPaint::of(field, self.id.0);
            if !focus.is_focused(window) {
                focus.focus(window, cx);
            }
            self.sync_editor(cx);
            cx.notify();
        }
    }

    /// `marketdata::commit` (`enter` in insert mode). Answers whether the
    /// header needs re-preparing.
    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editor.as_mut() else {
            return false;
        };
        let target = editing.target.clone();
        match (&mut editing.state, target) {
            (EditorState::Text(state), EditTarget::Cell { cell, labels }) => {
                let text = state.read(cx).value().to_string();
                self.commit_cell_edit(cell, labels, &text, window, cx)
            }
            (EditorState::Text(state), EditTarget::Attr { index, column }) => {
                let text = state.read(cx).value().to_string();
                self.commit_attr_edit(index, column, AttrInput::Text(&text), window, cx)
            }
            (EditorState::Date { field, paint, .. }, EditTarget::Attr { index, column }) => {
                // A digit still waiting in a segment is finished FIRST
                // (user ruling 2026-09-19, review I-1): `1` in the day
                // then `enter` means the 1st, not the day that was there
                // before with the edit marked as landed. A pending entry
                // that cannot stand — `0`, a short year — refuses the
                // commit and names the segment, the editor staying open
                // with the digits as typed (the cell rule). Both `enter`
                // doors (the field's own listener, the fragment's
                // `commit`) come through here, so they cannot disagree.
                if let Err(segment) = field.complete_pending() {
                    self.notice =
                        Some(format!("finish the {} or backspace", segment.name()).into());
                    return true;
                }
                *paint = DateFieldPaint::of(field, self.id.0);
                // Always a valid date from here (the field's own
                // invariant): nothing to parse, nothing to refuse.
                let value = Value::Date(field.date());
                self.commit_attr_edit(index, column, AttrInput::Value(value), window, cx)
            }
            (EditorState::Date { field, paint, .. }, EditTarget::Cell { cell, labels }) => {
                // A `Date` cell (spec §4.4): the attribute arm's own two
                // steps — finish a waiting digit or refuse naming the
                // segment, then hand the field's (always valid) date to
                // the value door. Both `enter`s land here.
                if let Err(segment) = field.complete_pending() {
                    self.notice =
                        Some(format!("finish the {} or backspace", segment.name()).into());
                    return true;
                }
                *paint = DateFieldPaint::of(field, self.id.0);
                let value = Value::Date(field.date());
                self.commit_cell_value(cell, labels, value, window, cx)
            }
            (EditorState::Text(state), EditTarget::RowLabel { row, label }) => {
                // A typed row label (dividend spec §5.3) is parsed by the
                // axis's own type — `parse_attr`, the attribute rule —
                // and its canonical spelling (`attr_text`) is the label,
                // so `2027-1-5` on a `Date` axis and `2027-01-05` name
                // the same row. A `Minted` axis never opens this editor;
                // refused as a moved grid rather than declared
                // impossible.
                let RowIdentity::Typed(ty) = self.spec.rows.identity else {
                    self.close_editor(window, cx);
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                };
                let text = state.read(cx).value().to_string();
                let new = match parse_attr(&text, ty) {
                    Ok(value) => attr_text(&value),
                    Err(e) => {
                        // Refused inline, the editor open with the text
                        // as typed (the cell rule).
                        self.notice = Some(e.into());
                        return true;
                    }
                };
                self.commit_row_label(row, label, new, window, cx)
            }
            (EditorState::Date { field, paint, .. }, EditTarget::RowLabel { row, label }) => {
                // A `Date` axis's label editor: the strip's own two steps
                // — finish a waiting digit or refuse naming the segment
                // — then the field's date in the ISO spelling a document
                // row label paints.
                if let Err(segment) = field.complete_pending() {
                    self.notice =
                        Some(format!("finish the {} or backspace", segment.name()).into());
                    return true;
                }
                *paint = DateFieldPaint::of(field, self.id.0);
                let new = field.date().format("%Y-%m-%d").to_string();
                self.commit_row_label(row, label, new, window, cx)
            }
        }
    }

    /// The row-label editor's commit (dividend spec §5.3): `new` is
    /// already the canonical label. The row must still be where the
    /// editor opened (the grid-moved rule), the label must not already
    /// name a row on screen — a document row, a deleted one or another
    /// insert alike — and the draft must still hold the provisional row;
    /// then the row is renamed, the editor closed (the rename is what
    /// keeps `close_editor` from dropping the row as provisional), the
    /// grid rebuilt with the cursor on the renamed row, and the FIRST
    /// cell's editor opened, since a row with a name and no values is
    /// the next thing to fill.
    fn commit_row_label(
        &mut self,
        row: usize,
        label: SharedString,
        new: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model.rows.get(row).map(|r| &r.label) != Some(&label) {
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        if self.model.rows.iter().any(|r| r.label.as_ref() == new) {
            // Refused with the editor open: the trader retypes, or
            // escapes to drop the provisional row.
            self.notice = Some(format!("'{new}' is already a row").into());
            return true;
        }
        if !self.draft.rename_row(label.as_ref(), &new) {
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        self.close_editor(window, cx);
        self.notice = None;
        self.rebuild_model(cx);
        if let Some(at) = self.model.rows.iter().position(|r| r.label.as_ref() == new) {
            self.cursor = Cursor::Cell { row: at, col: 0 };
            self.begin_edit(window, cx);
        }
        true
    }

    /// `marketdata::insert_up`/`insert_down` (and `_big`) with the editor
    /// open (2026-09-17): step the number the editor currently spells by
    /// `steps` units and write the result back into the SAME editor —
    /// nothing is committed, `enter` commits and `escape` cancels exactly
    /// as before. The unit is the target's own painted precision: a cell
    /// steps at its column's format (a slice column's own `precision`,
    /// else the panel's), an `F64`/`I64` attribute at the places its text
    /// paints (attributes paint with `{}`, so `5000` steps by one), and a
    /// `Date` attribute by whole days — [`crate::core::nudge_text`] is
    /// the arithmetic, this only decides the type and precision. The two
    /// sources differ on purpose: a cell's precision comes from its
    /// COLUMN's format, because that is what the cell paints with, while
    /// an attribute's comes from its own TEXT, because an attribute has no
    /// format and paints exactly the places the document sent.
    /// Text that does not parse leaves the editor untouched and says so
    /// in the notice. Answers whether the header needs re-preparing.
    fn nudge(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editor.as_mut() else {
            return false;
        };
        let state = match &mut editing.state {
            EditorState::Text(state) => state.clone(),
            EditorState::Date { field, paint, .. } => {
                // The field owns dates (2026-09-19): the shell's
                // `insert_up`/`insert_down` reach here only when the
                // field's own listener did not consume the key, and step
                // the same way it would have.
                field.step(steps);
                *paint = DateFieldPaint::of(field, self.id.0);
                return self.notice.take().is_some();
            }
        };
        let text = state.read(cx).value().to_string();
        let (ty, precision) = match &editing.target {
            EditTarget::Cell { cell: (_, col), .. } => {
                // `declared_type` answers `None` for the other three
                // `CellKind`s (Task 4's), reachable while their text
                // editor is still open — a `Text`/`Choice`/`Date` cell
                // COMMITS through `commit_cell_edit`'s own arms, but a
                // nudge is number arithmetic and has nothing to step
                // there, so it is refused here with a notice instead.
                // This is the ONE refusal: the precision lookup below
                // does not repeat it, because `declared_type` already
                // proved `kind_of` is `Number` here — a second refusal
                // over the same fact would be a defence the first one
                // hides from the mutation harness.
                let Some(ty) = declared_type(self.spec, &self.model, *col) else {
                    self.notice = Some("not a numeric cell".into());
                    return true;
                };
                // The precision to step by is the column's own
                // `CellKind::Number`'s `ColumnFormat` — a slice column's
                // own, distinct from the ladder's.
                let precision = match self.model.kind_of(*col) {
                    Some(CellKind::Number(format)) => Some(usize::from(format.precision)),
                    _ => None,
                };
                (ty, precision)
            }
            EditTarget::Attr { column, .. } => {
                let Some(attr) = self
                    .spec
                    .header
                    .iter()
                    .find(|a| a.column == column.as_ref())
                else {
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                };
                (attr.ty, None)
            }
            // A typed row label steps by its axis type at the places its
            // text paints — an attribute's own rule; a `Minted` axis
            // never opens this editor (unreachable, refused rather than
            // declared impossible).
            EditTarget::RowLabel { .. } => match self.spec.rows.identity {
                RowIdentity::Typed(ty) => (ty, None),
                RowIdentity::Minted => {
                    self.notice = Some("not a numeric cell".into());
                    return true;
                }
            },
        };
        match crate::core::nudge_text(&text, ty, precision, steps) {
            Ok(next) => {
                state.update(cx, |s, cx| s.set_value(next, window, cx));
                // A cleared notice is header paint; an unchanged `None`
                // is not.
                self.notice.take().is_some()
            }
            Err(e) => {
                self.notice = Some(e.into());
                true
            }
        }
    }

    /// `commit_edit`'s text-editor cell arm: the text is PARSED before
    /// anything is written, by the column's [`CellKind`] (spec §4.4) — a
    /// `Number` through [`parse_cell`] and the column's declared type
    /// (`Value::F64`/`I64` by that type, never by what the text happens
    /// to parse as), a `Text` trimmed and taken verbatim, empty refused
    /// only where the column is `required` (an optional note may
    /// honestly be cleared), a `Date` through the ISO spelling its cell
    /// paints (unreachable from `begin_edit`, which opens the field on a
    /// `Date` cell, but parsed rather than declared impossible), and a
    /// `Choice` as a `Text` — unreachable the same way since Task 5's
    /// popup, and kept the same way. A refusal is inline, with the editor
    /// left open and focused, because retyping a value is one keystroke
    /// away where dropping the editor would throw the whole line back at
    /// the trader. What is written, and how, is
    /// [`Self::commit_cell_value`]'s.
    fn commit_cell_edit(
        &mut self,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let value = match self.model.kind_of(cell.1) {
            Some(CellKind::Number(_)) => {
                // `declared_type` answers `Some` for every `Number` column
                // (its match is on the same `kind_of` as this one), so
                // this `else` cannot run — spelled as the moved-grid
                // refusal rather than an `unwrap`, because a panic on the
                // render thread is never the answer.
                let Some(ty) = declared_type(self.spec, &self.model, cell.1) else {
                    self.close_editor(window, cx);
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                };
                match parse_cell(text, ty) {
                    Ok(parsed) => match ty {
                        ColumnType::I64 => Value::I64(parsed as i64),
                        _ => Value::F64(parsed),
                    },
                    Err(e) => {
                        // Stay in insert mode, with the text as typed.
                        self.notice = Some(e.into());
                        return true;
                    }
                }
            }
            Some(CellKind::Text | CellKind::Choice(_)) => {
                let trimmed = text.trim();
                if trimmed.is_empty() && self.column_required(cell.1) {
                    self.notice = Some("a value is required".into());
                    return true;
                }
                Value::Utf8(trimmed.to_string())
            }
            Some(CellKind::Date) => match parse_attr(text, ColumnType::Date) {
                Ok(value) => value,
                Err(e) => {
                    self.notice = Some(e.into());
                    return true;
                }
            },
            None => {
                // The column is gone: the grid moved under the editor.
                self.close_editor(window, cx);
                self.notice = Some(CELL_MOVED.into());
                return true;
            }
        };
        self.commit_cell_value(cell, labels, value, window, cx)
    }

    /// Whether a flat column must hold a value ([`ValueColumn::required`]);
    /// a pivot's cells are never required — a NULL there is the desk's own
    /// "no value here" (§6.3), and every cell of it is a number anyway.
    fn column_required(&self, col: usize) -> bool {
        self.spec
            .flat_columns()
            .get(col)
            .is_some_and(|vc| vc.required)
    }

    /// Write one already-valid cell value into the draft and paint it —
    /// the door both editor forms end at (the text editor after
    /// [`Self::commit_cell_edit`] parsed, the date field with its own
    /// date), so the identity check, the base stamp and the repaint are
    /// spelled once.
    ///
    /// The repaint PATCHES the one cell in the model the tile already
    /// holds (spec §4.5, [`MatrixModel::patch_cell`]) rather than
    /// rebuilding: the flat build is the per-commit cost `docs/perf.md`
    /// records at the edge of the 8 ms pure-UI budget for a 10,000-row
    /// schedule, and a commit changes one cell. The delegate's `Rc` clone
    /// is taken back FIRST so `Rc::make_mut` finds the model uniquely
    /// held and patches in place — with the delegate's clone still alive
    /// it would copy every row to patch one, the cost this exists to
    /// avoid — and `install_model` then hands the same `Rc` back and
    /// refreshes the table. Two cases still rebuild, honestly: a patch
    /// that answers `false` (the cell is out of the model's range, which
    /// the identity check above makes unreachable, kept as the belt), and
    /// a draft that was `Sent` — `Draft::set` moves it back to `Editing`,
    /// which changes EVERY cell's `sent` flag, not this one's.
    ///
    /// WHERE the value is written is the row's state (dividend spec
    /// §5.1/§5.2). A document row's edit goes into `Draft::edits` under
    /// the cell's own `cell_ref` — the row's DOCUMENT position, which is
    /// `cell` itself only while no row is inserted above it — never under
    /// the cursor's model index. An inserted row's value goes into its
    /// `RowEdit.cells` by column label through `Draft::set_row_cell`,
    /// since such a row has no document position for an index-keyed edit
    /// to name. A deleted row refuses: `begin_edit` already declines to
    /// open an editor on one, and this is the belt for the doors that
    /// open nothing (a choice step).
    fn commit_cell_value(
        &mut self,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model.label_of(cell) != labels {
            // The grid moved under the editor (see `EditTarget::Cell`).
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        let base = match self.edit_base() {
            Ok(base) => base,
            Err(e) => {
                self.close_editor(window, cx);
                self.notice = Some(e.into());
                return true;
            }
        };
        let was_sent = self.draft.is_sent();
        // Both `Copy`, read out ahead of the match so the arms can take
        // `&mut self` (the editor's close) with no borrow of the model.
        let (state, cell_ref) = {
            let row = &self.model.rows[cell.0];
            (row.state, row.cells[cell.1].cell_ref)
        };
        match state {
            RowState::Deleted => {
                self.close_editor(window, cx);
                self.notice = Some(DELETED_REFUSED.into());
                return true;
            }
            RowState::Inserted => {
                // The identity check above proved `labels.0` is this
                // row's label; a draft that no longer holds it as an
                // inserted row is a grid that moved (the row was dropped
                // between the editor opening and this commit), refused
                // the same way.
                if !self
                    .draft
                    .set_row_cell(labels.0.as_ref(), labels.1.as_ref(), value)
                {
                    self.close_editor(window, cx);
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                }
            }
            RowState::Document => {
                self.draft.set(
                    cell_ref,
                    (labels.0.to_string(), labels.1.to_string()),
                    value,
                    &base,
                );
            }
        }
        self.close_editor(window, cx);
        self.notice = None;
        let snapshot = match self.painted_snapshot() {
            Some(snapshot) if !was_sent => snapshot,
            // No painted snapshot is unreachable past `edit_base` (a model
            // with rows came from one); the `Sent` case is the honest
            // rebuild described above.
            _ => {
                self.rebuild_model(cx);
                return true;
            }
        };
        // Take the delegate's clone back before `make_mut` looks at the
        // count — see the doc comment.
        self.table.update(cx, |t, _| {
            t.delegate_mut().model = Rc::new(MatrixModel::default());
        });
        let patched = Rc::make_mut(&mut self.model).patch_cell(
            cell.0,
            cell.1,
            &snapshot,
            self.spec,
            &self.draft,
        );
        if patched {
            self.install_model(cx);
        } else {
            self.rebuild_model(cx);
        }
        true
    }

    /// `commit_edit`'s attribute arm (spec §5.2): the same parse-first
    /// discipline as [`Self::commit_cell_edit`], through
    /// [`crate::core::draft::parse_attr`] and the attribute's own declared
    /// type rather than the panel's cell type.
    fn commit_attr_edit(
        &mut self,
        index: usize,
        column: SharedString,
        input: AttrInput<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model.header.get(index).map(|h| &h.column) != Some(&column) {
            // The header moved under the editor — unreachable today (a
            // panel's header is fixed by its spec), refused rather than
            // guessed at, the same rule `commit_cell_edit` applies to a
            // moved cell.
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        let Some(attr) = self
            .spec
            .header
            .iter()
            .find(|a| a.column == column.as_ref())
        else {
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        };
        let value = match input {
            AttrInput::Value(value) => value,
            AttrInput::Text(text) => match parse_attr(text, attr.ty) {
                Ok(value) => value,
                Err(e) => {
                    // Refused, staying in insert mode with the typed text
                    // (the cell rule, spec §5.2) — retyping is one
                    // keystroke away where dropping the editor would
                    // throw the whole line back at the trader.
                    self.notice = Some(e.into());
                    return true;
                }
            },
        };
        let base = match self.attr_edit_base() {
            Ok(base) => base,
            Err(e) => {
                self.close_editor(window, cx);
                self.notice = Some(e.into());
                return true;
            }
        };
        self.draft.set_attr(attr.column, value, &base);
        self.close_editor(window, cx);
        self.notice = None;
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
    /// rule), and the shell decides where focus lands next. The blur is
    /// conditional on the editor's own field holding focus, for
    /// `close_popup_with_window`'s reason: an editor orphaned by `mod+l`
    /// and closed from a `:` line must not blur the command line.
    ///
    /// A ROW-LABEL editor closing on a row still provisional — the draft
    /// holds its minted label as an `Inserted` row with no cell filled —
    /// drops that row (dividend spec §5.3): a row nobody named and
    /// nothing was typed into is not unsent work, and leaving `new-1`
    /// behind on a `Typed` axis would paint a row the axis's own type
    /// cannot name. A commit renames the row BEFORE closing, so the
    /// minted label is gone from the draft by the time this looks.
    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = &self.editor
            && e.state.is_focused(window, cx)
        {
            window.blur(cx);
        }
        let Some(Editing {
            target: EditTarget::RowLabel { label, .. },
            ..
        }) = self.editor.take()
        else {
            return;
        };
        let provisional = matches!(
            self.draft.row_state(label.as_ref()),
            Some(RowEdit::Inserted { cells, .. }) if cells.is_empty()
        );
        if provisional {
            let base = self.model.source_time.clone().unwrap_or_default();
            self.draft.delete_row(label.as_ref(), &base);
            self.rebuild_model(cx);
        }
    }

    // ---- the row verbs (dividend spec §5.3) ---------------------------

    /// The cursor row a row verb acts on, with every refusal the three
    /// share: an open editor is cancelled first (a row verb is
    /// navigation, never a commit — `set_key`'s own rule), a `Behind`
    /// draft refuses (`BEHIND_REFUSED`), the strip refuses (`NOT_A_ROW`),
    /// and an empty grid refuses (`NO_DOCUMENT`, through `edit_base`,
    /// which also answers the generation the edit is stamped against).
    fn row_verb_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(usize, String), String> {
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        if self.draft.is_behind() {
            return Err(BEHIND_REFUSED.to_string());
        }
        let Cursor::Cell { row, .. } = self.cursor else {
            return Err(NOT_A_ROW.to_string());
        };
        let base = self.edit_base()?;
        Ok((row, base))
    }

    /// `o`/`shift+o` (`marketdata::insert_below`/`insert_above`): insert a
    /// row beside the cursor row and start filling it.
    ///
    /// WHERE it lands is the anchor (controller ruling 2026-09-19). `o`
    /// anchors on the cursor row itself, document or inserted alike —
    /// and a follower that row already had is re-hung onto the new row
    /// (`Draft::rehang_followers`), so the new row sits IMMEDIATELY below
    /// the cursor row rather than beside its earlier sibling in label
    /// order, where a later rename would re-sort the pair on commit.
    /// `shift+o` on a DOCUMENT row anchors on the row painted above it
    /// (`None` at the top) — which may itself be an inserted row; on an
    /// INSERTED row the new row takes that row's own anchor and the row
    /// is re-anchored onto the new one (`Draft::reanchor_row`), so the
    /// chain paints new-above-old and survives a rebase as a chain.
    ///
    /// WHO names it is the axis (`RowIdentity`): a `Minted` axis mints
    /// `new-<n>` against the rows on screen and opens the first CELL's
    /// editor at once; a `Typed` axis opens the row-label editor on the
    /// minted row instead — the segmented date field for a `Date` axis
    /// (opening on today's date on the trader's clock, the strip's own landing), the text
    /// `Input` for any other type — whose commit renames the row and then
    /// opens the first cell (`commit_row_label`), and whose cancel drops
    /// the row (`close_editor`).
    fn insert_row(
        &mut self,
        below: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (row, base) = self.row_verb_target(window, cx)?;
        let cursor_label = self.model.rows[row].label.to_string();
        let label = self
            .draft
            .mint_label(|l| self.model.rows.iter().any(|r| r.label.as_ref() == l));
        // The anchor, and — on `shift+o` over an inserted row — the row
        // to hang off the new one afterwards. On `o`, whatever already
        // hung off the cursor row moves onto the new row FIRST, before
        // the new row itself is anchored there.
        let (anchor, rehang) = if below {
            self.draft
                .rehang_followers(Some(&cursor_label), Some(label.clone()));
            (Some(cursor_label), None)
        } else if self.model.rows[row].state == RowState::Inserted {
            let inherited = match self.draft.row_state(&cursor_label) {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            };
            (inherited, Some(cursor_label))
        } else {
            (
                row.checked_sub(1)
                    .map(|above| self.model.rows[above].label.to_string()),
                None,
            )
        };
        self.draft.insert_row(label.clone(), anchor, &base);
        if let Some(old) = rehang {
            self.draft.reanchor_row(&old, Some(label.clone()));
        }
        self.rebuild_model(cx);
        let Some(at) = self
            .model
            .rows
            .iter()
            .position(|r| r.label.as_ref() == label)
        else {
            // The rebuild refused the grid (its notice says why); the
            // draft still carries the row for the next build to place.
            return Ok(());
        };
        self.cursor = Cursor::Cell { row: at, col: 0 };
        self.notice = None;
        match self.spec.rows.identity {
            RowIdentity::Minted => self.begin_edit(window, cx),
            RowIdentity::Typed(ty) => self.begin_label_edit(at, label.into(), ty, window, cx),
        }
        Ok(())
    }

    /// Open the row-label editor on the provisional row at `row` (spec
    /// §5.3): `begin_edit`'s two forms, seeded EMPTY — a `Date` axis's
    /// field on today's date on the trader's clock, since there is no painted text to
    /// open on, and any other type's text `Input` blank — with the
    /// keyboard, so the shell's insert branch hands it every bare key.
    fn begin_label_edit(
        &mut self,
        row: usize,
        label: SharedString,
        ty: ColumnType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = if ty == ColumnType::Date {
            let field = DateTimeField::open(
                self.clock
                    .today(chrono::Utc::now())
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight exists"),
                Precision::Date,
                Segment::Day,
            );
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            let paint = DateFieldPaint::of(&field, self.id.0);
            EditorState::Date {
                field,
                focus,
                paint,
            }
        } else {
            let state = cx.new(|cx| InputState::new(window, cx));
            state.read(cx).focus_handle(cx).focus(window, cx);
            EditorState::Text(state)
        };
        self.editor = Some(Editing {
            state,
            target: EditTarget::RowLabel { row, label },
        });
    }

    /// `d d` (`marketdata::delete_row`): delete the cursor row through
    /// `Draft::delete_row` — an inserted row is dropped outright and the
    /// grid rebuilt (the cursor clamps onto whatever now sits there), a
    /// document row is marked `Deleted` and stays painted struck through,
    /// and a row already marked answers `ALREADY_DELETED` rather than
    /// marking it twice.
    fn delete_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let (row, base) = self.row_verb_target(window, cx)?;
        let label = self.model.rows[row].label.to_string();
        match self.draft.delete_row(&label, &base) {
            RowDelete::Dropped | RowDelete::Marked => {
                self.notice = None;
                self.rebuild_model(cx);
                Ok(())
            }
            RowDelete::Already => Err(ALREADY_DELETED.to_string()),
        }
    }

    // ---- the action list ---------------------------------------------

    /// `.`/`⋯` (spec §6.1): open the action list, or close it if it is
    /// already open. Insert mode and the popup are exclusive — opening it
    /// with an editor still open cancels the editor first (never commits
    /// it, exactly as a click elsewhere does), which is why this takes
    /// `window`.
    ///
    /// **The popup match is exhaustive on purpose** (review fix round 1,
    /// IMPORTANT-1): a Picker open when this runs — reachable via
    /// `ctrl+k` → palette → "Actions menu", which is not bound by mode at
    /// all — must be closed through [`Self::close_popup_with_window`]
    /// (blur, then drop) before the Menu overwrites `self.popup`, or the
    /// picker's still-focused `InputState` would be dropped with no
    /// blur, leaving `Window::focused` pointing at a dead input for the
    /// rest of the session (`close_editor`'s own rule). Unlike the editor
    /// case just below, this one does NOT return: opening the action
    /// list is the whole point of the row that reached here, so closing
    /// the picker falls through into building the menu rather than
    /// merely toggling it off.
    pub(crate) fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.popup {
            Some(Popup::Menu(_)) => {
                self.close_popup_with_window(window, cx);
                return;
            }
            Some(Popup::Picker(_) | Popup::Choice(_)) => self.close_popup_with_window(window, cx),
            None => {}
        }
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        let rows = menu::rows(
            &MenuInputs {
                badge: self.draft.badge(),
                // Upload is Part 4; every build before it greys the row with
                // `not built yet` (spec §6.2's table) rather than pretending
                // the panel can send anything anywhere.
                upload_built: false,
                policy: self.policy,
                kind_title: self.spec.title,
                kind_actions: self.spec.actions,
            },
            self.clock,
        );
        let highlighted = menu::first_enabled(&rows);
        self.popup = Some(Popup::Menu(MenuState { rows, highlighted }));
        cx.notify();
    }

    /// Close whatever popup is open — THE one door (final review, B1).
    /// Harmless when none is (`dispatch`'s own "any other action closes
    /// it first" rule calls this unconditionally).
    ///
    /// The keyboard is given up first when the popup is a
    /// [`Popup::Picker`] — `close_editor`'s own blur-then-drop order
    /// (blur, THEN drop, both halves): its field holds the keyboard, and
    /// dropping it without blurring would leave `Window::focused`
    /// pointing at a dead input for the rest of the session. There used
    /// to be a second, `Window`-less `close_popup(cx)` for the sites
    /// believed unreachable with a Picker open (`find`, `command`, the
    /// menu's `on_mouse_down_out`); every one of them WAS reachable —
    /// `u` then `ctrl+k` then the palette's "Find", or `u` then `mod+l`
    /// (the shell moves focus to its root and the picker stays `Some`)
    /// then `/` or `:` — and every one of them already had a `Window` in
    /// hand, so the door without one is gone rather than guarded.
    ///
    /// The blur is conditional on the picker's OWN field holding focus
    /// (re-review of that wave): a picker can be orphaned with the
    /// keyboard elsewhere — `u`, `ctrl+k` (the palette takes focus), the
    /// palette's "Find" (the shell's command line takes it, the picker
    /// still `Some`) — and the first find keystroke reaches here. An
    /// unconditional blur then blurred the FIND FIELD, and the shell's
    /// focus backstop cancelled the command line on the next render: the
    /// trader's find died after one character. Blurring is a module
    /// giving up focus it holds, never focus something else holds.
    ///
    /// A [`Popup::Choice`] (dividend spec §4.4) is the picker's twin here
    /// in both halves: its field holds the keyboard the same way, and it
    /// is orphaned the same ways. It is also MIRRORED into the delegate
    /// (painted under its cell), so the close re-mirrors — this door is
    /// reachable from the popup's own `on_mouse_down_out`, which ends in
    /// no `dispatch` tail.
    pub(crate) fn close_popup_with_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let own_field_focused = match &self.popup {
            Some(Popup::Picker(p)) => p.input.read(cx).focus_handle(cx).is_focused(window),
            Some(Popup::Choice(c)) => c.input.read(cx).focus_handle(cx).is_focused(window),
            Some(Popup::Menu(_)) | None => false,
        };
        if own_field_focused {
            window.blur(cx);
        }
        let was_choice = matches!(self.popup, Some(Popup::Choice(_)));
        self.popup = None;
        if was_choice {
            self.sync_editor(cx);
        }
        cx.notify();
    }

    /// A hover over menu row `index` — the mouse form of `j`/`k`. Cheap
    /// on purpose: gpui fires `on_mouse_move` on every pointer move over
    /// the row, so only a CHANGE notifies; a pointer resting on the
    /// highlighted row costs a compare.
    pub(crate) fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &mut self.popup else {
            return;
        };
        if m.highlighted == index || index >= m.rows.len() {
            return;
        }
        m.highlighted = index;
        cx.notify();
    }

    /// A hover over painted picker row `row` — the mouse form of
    /// `up`/`down`; the same change-only rule as [`Self::menu_hover`].
    pub(crate) fn picker_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return;
        };
        if p.rows.highlighted() == row || !p.rows.set_highlighted(row) {
            return;
        }
        cx.notify();
    }

    /// `enter` on the highlighted row, or a click on any row (spec
    /// §6.2): a disabled row's reason becomes the notice and the popup
    /// stays open; an enabled one closes the popup and re-enters
    /// [`Self::dispatch`] on its own id, so a menu row and a keybinding
    /// (or a `:` line) take exactly one path from here on.
    pub(crate) fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        let Some(row) = m.rows.get(index) else {
            return;
        };
        let MenuRow::Action { id, enabled, .. } = row else {
            return;
        };
        match enabled {
            Err(reason) => {
                self.notice = Some((*reason).into());
                self.rebuild_chrome();
                cx.notify();
            }
            Ok(()) => {
                let id = id.clone();
                self.close_popup_with_window(window, cx);
                self.dispatch(&id, None, window, cx);
            }
        }
    }

    // ---- the underlying picker -----------------------------------

    /// `u` (normal mode) and the menu row (spec §7): open the underlying
    /// picker over the dataset's catalog keys, ranked by `listfilter` as
    /// the trader types. Never refused for a dirty draft (2026-09-19):
    /// a pick PARKS the current draft under its underlying (`set_key`),
    /// and a row whose underlying already holds a parked draft says so in
    /// its label (`NKY.Z · 1 cell, spot_ref`, prepared here from
    /// [`Self::parked_marks`], never in `render`). Re-requests the catalog
    /// on the way in (the existing rule: `request_catalog()` +
    /// `cx.notify()` in the same update) so the list is fresh even if
    /// this panel has never asked before; a catalog that arrives later,
    /// while the picker is still open, is folded in by the diagnostics
    /// observer in `new`.
    pub(crate) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Defensive: unreachable through the shipped keymap (`edit` is a
        // normal-mode binding, and `load_underlying`'s own `dispatch`
        // guard above already closes any OTHER open popup before this
        // runs), but a direct action dispatch (the palette) is not bound
        // by mode at all.
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        self.request_catalog(cx);
        let rows = PickerRows::with_marks(self.catalog_keys(cx), self.parked_marks());
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("underlying"));
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            // `InputState::set_value` emits no `Change` at all (a test
            // harness writes the field that way, never through real
            // keystrokes), which is why `commit_picker` re-ranks from the
            // field's own current text as well rather than trusting this
            // subscription alone — this is the LIVE path, for a trader
            // actually typing.
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                if let Some(Popup::Picker(p)) = &mut this.popup {
                    p.rows.refilter(&query);
                }
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.popup = Some(Popup::Picker(PickerState { input, rows }));
        self.notice = None;
        self.changed(cx);
    }

    /// `commit` (`enter`) with the picker open (spec §7): re-rank from the
    /// field's CURRENT text before resolving the highlighted row. A real
    /// keystroke already kept the ranking current through the `Change`
    /// subscription in `open_picker`, but `InputState::set_value` emits
    /// none at all (CLAUDE.md's own trap, exercised by a test harness that
    /// writes the field that way) — trusting whatever was last ranked
    /// would let the choice depend on a rank that was never actually run.
    /// `refilter` is a no-op when the text has not actually changed
    /// (`PickerRows`'s own doc comment, review fix round 1, CRITICAL),
    /// so this defensive call never resets the highlight the trader
    /// already moved to — it only re-ranks, preserving the highlighted
    /// KEY, when there is a real query to catch up on. Nothing painted
    /// (no catalog, or nothing matches) is inert (spec §7): nothing to
    /// load, and the picker stays open.
    fn commit_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return;
        };
        let query = p.input.read(cx).value().to_string();
        p.rows.refilter(&query);
        if p.rows.painted_len() == 0 {
            return;
        }
        let index = p.rows.highlighted();
        self.picker_pick(index, window, cx);
    }

    /// A row click, or `enter` after [`Self::commit_picker`]'s own
    /// re-rank (spec §7): load the key at painted row `index` (a
    /// WINDOW-relative index, matching [`PickerRows::highlighted`] and
    /// the row a click carries) through the same door `:underlying`/`:key`
    /// use, closing the picker first — exactly as [`Self::menu_pick`]
    /// closes the menu before dispatching its own row, and for the same
    /// reason: `set_key` needs no keyboard, and the picker's own field is
    /// done being useful the moment a row is chosen.
    pub(crate) fn picker_pick(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Popup::Picker(p)) = &self.popup else {
            return;
        };
        let Some(i) = p.rows.painted().nth(index) else {
            return;
        };
        let key = p.rows.all()[i].clone();
        self.close_popup_with_window(window, cx);
        self.set_key(parse_display_key(&key), window, cx);
    }

    // ---- the choice cell (dividend spec §4.4) --------------------------

    /// `begin_edit`'s `Choice` arm: open the typeahead popup over the
    /// column's own vocabulary, placed on the cell's current text, and
    /// give its field the keyboard — the picker's contract, spec §7.
    /// The popup is exclusive with the editor and every other popup
    /// (`begin_edit` returned early on an open editor; `dispatch` closed
    /// any other popup before `edit` ran; a double-click's mouse-down
    /// closed one through `on_mouse_down_out`), and the one path that
    /// could still find one — the palette's `Edit cell` with a picker
    /// open — is closed through the one door, blur first.
    fn open_choice(
        &mut self,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
        current: &str,
        options: &'static [&'static str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup.is_some() {
            self.close_popup_with_window(window, cx);
        }
        // The column's own label is the placeholder — what the field is
        // choosing a value FOR.
        let placeholder: SharedString = self
            .model
            .columns
            .get(cell.1)
            .cloned()
            .unwrap_or_else(|| SharedString::new_static("option"));
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe_in(&input, window, |this, input, event, _window, cx| {
            // The LIVE path, for a trader typing (`open_picker`'s own
            // subscription, for the same reason): `set_value` emits no
            // `Change`, so `commit_choice` re-reads the field as well.
            if let InputEvent::Change = event {
                let query = input.read(cx).value().to_string();
                if let Some(Popup::Choice(c)) = &mut this.popup
                    && c.list.set_query(&query)
                {
                    c.prepare();
                    this.sync_editor(cx);
                }
                cx.notify();
            }
        })
        .detach();
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.popup = Some(Popup::Choice(ChoicePopup::new(
            input, options, current, cell, labels,
        )));
        self.notice = None;
    }

    /// `commit` (`enter`) with the choice popup open: re-rank from the
    /// field's CURRENT text (`commit_picker`'s own defensive rule —
    /// `InputState::set_value` emits no `Change`, and a pick must be
    /// decided by a ranking that was actually run), then write the lit
    /// option through the cell door. Nothing lit — the typed text matches
    /// no option — is refused with the popup left open, since retyping is
    /// one keystroke away. Answers whether the header needs re-preparing.
    fn commit_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Popup::Choice(c)) = &mut self.popup else {
            return false;
        };
        let query = c.input.read(cx).value().to_string();
        if c.list.set_query(&query) {
            c.prepare();
        }
        let Some(i) = c.list.pick() else {
            self.notice = Some("no option matches".into());
            return true;
        };
        let option = c.list.options()[i].clone();
        self.pick_option(option, window, cx)
    }

    /// A row click, or `enter` after [`Self::commit_choice`]'s re-rank:
    /// light painted row `row` (window-relative, the row a click carries)
    /// and write it. Refused silently past the painted range, which a
    /// click cannot reach anyway.
    pub(crate) fn choice_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Choice(c)) = &mut self.popup else {
            return;
        };
        if !c.list.set_highlighted(row) {
            return;
        }
        let Some(i) = c.list.pick() else {
            return;
        };
        let option = c.list.options()[i].clone();
        if self.pick_option(option, window, cx) {
            self.rebuild_chrome();
        }
        cx.notify();
    }

    /// The pick itself: close the popup FIRST (blur, then drop — its
    /// field is done being useful the moment an option is chosen, as
    /// `picker_pick` closes before `set_key`), then write through
    /// [`Self::commit_cell_value`], the one door every cell value lands
    /// through, so the identity check and the patch are spelled once.
    fn pick_option(&mut self, option: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Popup::Choice(c)) = &self.popup else {
            return false;
        };
        let (cell, labels) = c.target();
        self.close_popup_with_window(window, cx);
        self.commit_cell_value(cell, labels, Value::Utf8(option), window, cx)
    }

    /// A hover over painted choice row `row` — the mouse form of
    /// `up`/`down`; [`Self::picker_hover`]'s change-only rule, plus the
    /// re-mirror the delegate's copy needs.
    pub(crate) fn choice_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Popup::Choice(c)) = &mut self.popup else {
            return;
        };
        if c.list.highlighted() == row || !c.list.set_highlighted(row) {
            return;
        }
        c.prepare();
        self.sync_editor(cx);
        cx.notify();
    }

    /// `space`/`shift+space` (`step`/`step_back`): move the cursor cell
    /// to the next/previous option of its column's vocabulary, in place,
    /// wrapping at both ends — the settings and object dialogs' own
    /// stepping verbs, brought to a grid cell. The CURRENT option is what
    /// the cell paints (the draft's own value where one exists), so two
    /// steps compose; a NULL cell, or one holding a value the vocabulary
    /// no longer lists, has no current option and lands on the first
    /// (forward) or last (back) rather than treating the hole as option
    /// 0 and stepping past it. The gates are `:bump`'s: `Behind`, no
    /// document, the strip; and a cell of any other kind says so.
    fn step_choice(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.draft.is_behind() {
            return Err(BEHIND_REFUSED.to_string());
        }
        self.edit_base()?;
        let Cursor::Cell { row, col } = self.cursor else {
            return Err("step needs a grid cell — the cursor is in the header".to_string());
        };
        if self.model.rows[row].state == RowState::Deleted {
            return Err(DELETED_REFUSED.to_string());
        }
        let Some(CellKind::Choice(options)) = self.model.kind_of(col) else {
            return Err("not a choice cell".to_string());
        };
        let options: &'static [&'static str] = options;
        let len = options.len() as isize;
        if len == 0 {
            // A spec declaring `choices: Some(&[])` — nothing to step
            // through, and `rem_euclid(0)` below would panic.
            return Err("the column declares no options".to_string());
        }
        let current = self.model.rows[row].cells[col].text.as_ref();
        let next = match options.iter().position(|o| *o == current) {
            Some(i) => (i as isize + delta).rem_euclid(len),
            None if delta > 0 => 0,
            None => len - 1,
        };
        let cell = (row, col);
        let labels = self.model.label_of(cell);
        self.commit_cell_value(
            cell,
            labels,
            Value::Utf8(options[next as usize].to_string()),
            window,
            cx,
        );
        Ok(())
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
    /// the header no longer said anything about, and `:rebase`/`:revert`
    /// were both refused (there is no draft to move or drop) — the only
    /// way out was a `:key` retype or waiting for the next delivery.
    fn revert(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft.is_empty() {
            return Err("no edits to revert".to_string());
        }
        self.draft.revert();
        self.leave_behind();
        self.rebuild_model(cx);
        // `rebuild_model` only ever WRITES `notice` on a build failure, so
        // one already wins by being left in place. On success, clear only
        // the BEHIND-refusal notice — the one thing this verb is itself
        // the escape from — and leave anything else (a delivery error,
        // say) alone: it has nothing to do with reverting a draft.
        if self.notice.as_deref() == Some(BEHIND_REFUSED) {
            self.notice = None;
        }
        self.changed(cx);
        Ok(())
    }

    /// Drop whatever generation was retained under `Behind` so the next
    /// [`Self::rebuild_model`] paints the newest delivered one instead.
    ///
    /// The one door both `:revert` and `:rebase` leave through — harmless
    /// to call when the draft was never `Behind` (`base_snapshot` is
    /// already `None`), which is why `revert` above calls it
    /// unconditionally rather than guarding on `is_behind()` first.
    fn leave_behind(&mut self) {
        self.base_snapshot = None;
    }

    /// `:bump <delta> [row|col]` — add `delta` to every NUMBER cell along
    /// the cursor's ROW by default (a term's whole node ladder is the
    /// shape a trader nudges) or down its column on request.
    ///
    /// Each cell's CURRENT painted value is what is added to, which is the
    /// draft's own value wherever one exists, so two bumps compose instead
    /// of the second reading through to the document underneath
    /// (`Draft::bump`'s own contract). A NULL cell is skipped: there is no
    /// number to add to, and inventing one would put a value on screen the
    /// document never carried. A cell whose column is not `CellKind::Number`
    /// (a flat panel's date or status column) is skipped the same way —
    /// `:bump` is arithmetic, and a schedule's non-numeric columns have
    /// nothing to add to either.
    fn bump(&mut self, delta: f64, axis: BumpAxis, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft.is_behind() {
            return Err(BEHIND_REFUSED.to_string());
        }
        let base = self.edit_base()?;
        let Cursor::Cell { row, col } = self.cursor else {
            // `:bump` walks a grid row or column (spec §8.3); the strip
            // has neither, and there is no cell here to name.
            return Err("bump needs a grid cell — the cursor is in the header".to_string());
        };
        // A row bump on a deleted row is refused outright (dividend spec
        // §5.2: a deleted row's cells refuse edits); a column bump skips
        // one, below, rather than refusing the whole column for it.
        if matches!(axis, BumpAxis::Row) && self.model.rows[row].state == RowState::Deleted {
            return Err(DELETED_REFUSED.to_string());
        }
        // The draft's own edit for this cell, through `Draft::numeric_edit`
        // — `:bump`'s own door onto it — falling back to the model's own
        // painted value (the document's, or NULL) when there is no edit
        // yet to read. An INSERTED row's cell_ref is a model position, not
        // a document one, so `Draft::edits` is never asked about it: its
        // painted value IS the draft's own (`RowEdit.cells`, which is all
        // the model ever paints there).
        let numeric_value = |state: RowState, cell: &Cell| {
            let edit = match state {
                RowState::Inserted => None,
                RowState::Document | RowState::Deleted => self.draft.numeric_edit(cell.cell_ref),
            };
            edit.or(match &cell.value {
                Some(Value::F64(v)) => Some(*v),
                Some(Value::I64(v)) => Some(*v as f64),
                Some(Value::Utf8(_) | Value::Date(_)) | None => None,
            })
        };
        // Each cell's own declared type — `Draft::bumped`'s one typing
        // rule, read off the spec rather than guessed from the painted
        // value: `Columns::Values` names each column's own type by its
        // grid position (`column_required`'s own convention — a flat
        // model's columns are built from `flat_columns()` in that exact
        // order); `Columns::Axis` gives every ladder cell the spec's
        // `value_type`, except a leading slice-value cell, which is
        // always `F64` regardless (`SliceValue`'s own doc comment: a
        // slice value is f64 only).
        let ty_of = |ci: usize| -> ColumnType {
            match self.spec.columns {
                Columns::Values(_) => self
                    .spec
                    .flat_columns()
                    .get(ci)
                    .map(|vc| vc.ty)
                    .unwrap_or(self.spec.value_type),
                Columns::Axis(_) if ci < self.model.slice_columns => ColumnType::F64,
                Columns::Axis(_) => self.spec.value_type,
            }
        };
        let mut skipped = 0usize;
        // Each cell to bump as (model cell, its current value, its declared type).
        let values: Vec<((usize, usize), f64, ColumnType)> = match axis {
            // A row bump walks the LADDER: the leading `slice_columns`
            // cells are the term's own forward/atm/skew, and bumping a
            // term's vols must not move its forward with them. A column
            // bump on a slice column still bumps that column down every
            // term, which is what a bump on `fwd` means.
            BumpAxis::Row => {
                let r = &self.model.rows[row];
                r.cells
                    .iter()
                    .enumerate()
                    .skip(self.model.slice_columns)
                    .filter_map(|(ci, cell)| {
                        if !matches!(self.model.kind_of(ci), Some(CellKind::Number(_))) {
                            skipped += 1;
                            return None;
                        }
                        numeric_value(r.state, cell).map(|v| ((row, ci), v, ty_of(ci)))
                    })
                    .collect()
            }
            BumpAxis::Col => {
                if !matches!(self.model.kind_of(col), Some(CellKind::Number(_))) {
                    return Err("not a numeric column".to_string());
                }
                let ty = ty_of(col);
                self.model
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.state != RowState::Deleted)
                    .filter_map(|(ri, r)| {
                        r.cells
                            .get(col)
                            .and_then(|cell| numeric_value(r.state, cell))
                            .map(|v| ((ri, col), v, ty))
                    })
                    .collect()
            }
        };
        if values.is_empty() {
            return Err(if skipped > 0 {
                format!("no numeric cells to bump ({skipped} skipped)")
            } else {
                "no values to bump".to_string()
            });
        }
        // Collected rather than handed to `Draft::bump` as a lazy iterator:
        // the labels come off `self.model` while the draft is borrowed
        // mutably, which the borrow checker refuses — and one `Vec` per
        // `:bump` line is a keystroke's worth of work, not a per-frame one.
        // A document row's cell is keyed for `Draft::edits` by its own
        // `cell_ref` (the document position — `commit_cell_value`'s rule);
        // an inserted row's goes to its `RowEdit.cells` by label instead,
        // with the same arithmetic.
        let mut document: Vec<BumpCell> = Vec::with_capacity(values.len());
        let mut inserted: Vec<((String, String), f64, ColumnType)> = Vec::new();
        for (cell, value, ty) in values {
            let labels = self.model.label_of(cell);
            let labels = (labels.0.to_string(), labels.1.to_string());
            let r = &self.model.rows[cell.0];
            match r.state {
                RowState::Inserted => inserted.push((labels, value, ty)),
                RowState::Document | RowState::Deleted => {
                    document.push((r.cells[cell.1].cell_ref, labels, value, ty));
                }
            }
        }
        // Every cell's `bumped()` result is checked — document rows AND
        // inserted rows together — before either write door opens
        // (controller ruling, fix round 1): a mixed row with an I64 node
        // behind an F64 one must not land the F64 cells through
        // `Draft::bump` and only then hit the I64 refusal in the
        // `set_row_cell` loop below, since a partial bump on a row an
        // inserted-row cell shares with document cells is exactly the
        // half-applied edit `Draft::bump`'s own check-then-write exists to
        // rule out. `bumped` is a pure function of its four arguments, so
        // validating it here and letting `Draft::bump`/`set_row_cell`
        // recompute the identical result when they actually write is a
        // second pass over a keystroke's worth of cells, not a risk of
        // disagreement.
        for (_, labels, value, ty) in &document {
            bumped(*value, delta, *ty, &labels.1)?;
        }
        for (labels, value, ty) in &inserted {
            bumped(*value, delta, *ty, &labels.1)?;
        }
        self.draft.bump(document.into_iter(), delta, &base)?;
        for ((row_label, col_label), value, ty) in inserted {
            let value = bumped(value, delta, ty, &col_label)?;
            self.draft.set_row_cell(&row_label, &col_label, value);
        }
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
        // The rebase guard (spec §2, amendment 4) needs the group sizes
        // of the document currently on screen — `painted_snapshot` while
        // `Behind` — before `rebase` below moves the draft onto the newer
        // one. Skipped, not refused, on a build failure or when the
        // painted snapshot is not really this draft's base (the M-1 path
        // — `capture_groups_if_base`'s own doc comment): the guard is a
        // refinement of `rebase`'s own report, never a gate on running it
        // at all. `self.draft` is taken out and put back rather than
        // borrowed in place, since `capture_groups_if_base` also reads
        // `self` (`painted_snapshot`, `spec`) and a method call cannot
        // hold both an immutable borrow of `self` and a mutable one of
        // `self.draft` at once.
        let mut draft = std::mem::take(&mut self.draft);
        self.capture_groups_if_base(&mut draft);
        self.draft = draft;
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

    /// The cursor's row when it is on a grid cell, `0` when it is in the
    /// strip — `find` and `repeat_find` never touch the strip (spec
    /// §5.1: "`/` matches row and column labels as today and never the
    /// strip"), so a search that somehow starts from `Attr` has nowhere
    /// better to begin than the top.
    fn cursor_row(&self) -> usize {
        match self.cursor {
            Cursor::Cell { row, .. } => row,
            Cursor::Attr(_) => 0,
        }
    }

    /// Move the cursor to a grid row, keeping its column — the column the
    /// cursor already had, or `last_grid_col` if it was in the strip.
    fn set_cursor_row(&mut self, row: usize) {
        let col = match self.cursor {
            Cursor::Cell { col, .. } => col,
            Cursor::Attr(_) => self.last_grid_col,
        };
        self.cursor = Cursor::Cell { row, col };
    }

    /// The blotter's own tab-separated spelling (§8.3): a cell is its
    /// text, a row is its label then its cells, a column is its cells one
    /// per line. Always the PREPARED text, so what is yanked is exactly
    /// what is on screen — a NULL yanks as nothing, never as `0.0000`.
    ///
    /// In the strip (spec §5.1), `y` yanks the attribute's own value and
    /// `yy` its label and value the same way; `yc` has no column to yank
    /// and answers `None` (the dispatcher turns that case into a notice
    /// before it ever reaches here).
    fn yank_text(&self, what: Yank) -> Option<String> {
        match self.cursor {
            Cursor::Cell { row: r, col: c } => {
                let row = self.model.rows.get(r)?;
                Some(match what {
                    Yank::Cell => row.cells.get(c)?.text.to_string(),
                    Yank::Row => {
                        // The copied line is what the trader SEES: the
                        // label leads it only where the label column is
                        // painted (`RowLabel::Shown`).
                        let label = self.spec.rows.shown().then(|| row.label.as_ref());
                        label
                            .into_iter()
                            .chain(row.cells.iter().map(|cell| cell.text.as_ref()))
                            .collect::<Vec<_>>()
                            .join("\t")
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
            Cursor::Attr(i) => {
                let attr = self.model.header.get(i)?;
                match what {
                    Yank::Cell => Some(attr.text.to_string()),
                    Yank::Row => Some(format!("{}\t{}", attr.label, attr.text)),
                    Yank::Col => None,
                }
            }
        }
    }

    /// What `/` searches, one string per row: the row label where it is
    /// painted (`RowLabel::Shown`), the row's painted cell texts joined
    /// where it is not — a trader can only look for what they can see,
    /// and a hidden `dividend_id` is not that.
    fn row_labels(&self) -> Vec<String> {
        if self.spec.rows.shown() {
            self.model
                .rows
                .iter()
                .map(|r| r.label.to_string())
                .collect()
        } else {
            self.model
                .rows
                .iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .map(|c| c.text.as_ref())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        }
    }

    /// `/` (spec §6.1's own example of a shell-owned door the popup must
    /// not survive): `/`/`:` are `tile`-context bindings the shell
    /// resolves before ever reaching this module's own dispatch, so the
    /// popup's own "any other dispatched action closes it first" rule
    /// (`dispatch`'s own guard) never sees them. Closing here,
    /// unconditionally and on every variant, is what keeps a find
    /// session that starts with the menu open from painting `mode ==
    /// menu` under the find field for even one keystroke. The `window`
    /// is for that close alone: a Picker can be open here too (`u`,
    /// `ctrl+k`, the palette's "Find"), and closing one blurs first.
    pub fn find(&mut self, event: FindEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popup_with_window(window, cx);
        match event {
            FindEvent::Changed(query) => {
                let origin = match &self.find {
                    Some(find) => find.origin,
                    None => {
                        let origin = self.cursor;
                        self.find = Some(FindState {
                            origin,
                            committed: None,
                        });
                        origin
                    }
                };
                // A search from the strip begins at the top (`cursor_row`'s
                // own rule for `Attr`).
                let origin = match origin {
                    Cursor::Cell { row, .. } => row,
                    Cursor::Attr(_) => 0,
                };
                let labels = self.row_labels();
                // Every keystroke searches from the ORIGIN, not from
                // wherever the previous one landed: that is what makes a
                // lengthening query walk forward and a shortened one walk
                // back (vim's incsearch).
                if let Some(row) = find_match(&labels, origin, FindDirection::Forward, &query) {
                    self.set_cursor_row(row);
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
                    self.cursor = find.origin;
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
        let mut at = self.cursor_row();
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
        self.set_cursor_row(at);
        self.clamp_cursor();
    }

    // ---- the `:` line ------------------------------------------------

    pub fn command(
        &mut self,
        line: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
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
        let command = commands::parse(line)?;
        // The other half of `find`'s own door (spec §6.1): `:` is a
        // shell-owned `tile`-context binding too, so the popup's own
        // "any other dispatched action closes it first" guard in
        // `dispatch` never sees a `:` line either. Every parsed command
        // but `Menu` itself (which TOGGLES the popup, and so must decide
        // for itself rather than have this close it out from under that
        // decision) closes it here, once parsing has succeeded — a
        // failed parse leaves the popup exactly as `dispatch`'s own
        // guard would, since nothing here ran at all.
        if !matches!(command, Command::Menu) {
            self.close_popup_with_window(window, cx);
        }
        match command {
            Command::Key(key) => {
                self.set_key(key, window, cx);
                Ok(())
            }
            Command::Revert => self.revert(cx),
            Command::Bump { delta, axis } => self.bump(delta, axis, cx),
            Command::Rebase => self.rebase(cx),
            // Parsed, not executed: the grammar a trader types is the one
            // Part 4 wires up.
            Command::Upload => Err("upload is not built yet".into()),
            Command::Set { attr, value } => self.set_attr_command(&attr, value, cx),
            // A bare `auto` answers with the current policy through the
            // command line's own inline slot, `:set <attr>`'s contract:
            // `Err` because nothing was written.
            Command::Auto(None) => Err(format!(
                "auto is {} (hold, rebase, replace)",
                self.policy.as_str()
            )),
            Command::Auto(Some(policy)) => {
                self.set_policy(policy, cx);
                Ok(())
            }
            Command::Menu => {
                self.toggle_menu(window, cx);
                Ok(())
            }
        }
    }

    /// The one setter for the update policy (spec §8.4, 2026-09-19) —
    /// `:auto`, the menu rows and the palette actions all land here. It
    /// changes what the NEXT delivery does and nothing on screen now: a
    /// draft already `Behind` stays `Behind` (`:rebase`/`:revert` are
    /// still its doors), so no model or chrome is rebuilt. The menu's
    /// tick cannot be stale either — every door here closes an open
    /// popup first — and the session flush compares `serialize()` on its
    /// own tick, so the notify is the ordinary "state moved" one.
    fn set_policy(&mut self, policy: UpdatePolicy, cx: &mut Context<Self>) {
        self.policy = policy;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn policy(&self) -> UpdatePolicy {
        self.policy
    }

    /// `:set <attr> [value]` (spec §5.2): the typed door onto the same
    /// attribute vocabulary `i`/`enter` on `Cursor::Attr` writes through —
    /// same parse, same refusals, attribute names as completions
    /// ([`Self::completions`]).
    ///
    /// `value: None` answers with the current value AS A NOTICE
    /// (`Err`, the command line's own inline-error slot — the same
    /// contract every other module's `command` keeps for a one-line
    /// answer that changed nothing), never `Ok`, since nothing was
    /// written.
    fn set_attr_command(
        &mut self,
        attr: &str,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let unknown = || {
            let names = self
                .spec
                .header
                .iter()
                .map(|a| a.column)
                .collect::<Vec<_>>()
                .join(", ");
            format!("no attribute '{attr}' ({names})")
        };
        match value {
            None => {
                // With no document there is no value to report for ANY
                // attribute — say so, rather than "no attribute 'spot_ref'"
                // about a name the spec does declare (final review, T1).
                if self.model.header.is_empty() {
                    return Err(NO_DOCUMENT.to_string());
                }
                let cell = self
                    .model
                    .header
                    .iter()
                    .find(|h| h.column.as_ref() == attr)
                    .ok_or_else(unknown)?;
                Err(format!("{attr} = {}", cell.text))
            }
            Some(value) => {
                if self.draft.is_behind() {
                    return Err(BEHIND_REFUSED.to_string());
                }
                let header_attr = self
                    .spec
                    .header
                    .iter()
                    .find(|a| a.column == attr)
                    .ok_or_else(unknown)?;
                let parsed = parse_attr(&value, header_attr.ty)?;
                let base = self.attr_edit_base()?;
                self.draft.set_attr(header_attr.column, parsed, &base);
                self.rebuild_model(cx);
                self.changed(cx);
                Ok(())
            }
        }
    }

    /// Point the panel at another document.
    ///
    /// **A switch is a restore** (user ruling 2026-09-19, "keep them per
    /// underlying", superseding the 2026-09-14 refusal): a draft's cells
    /// are grid indices into the document they were made on, so they
    /// cannot travel to another document's ladder — instead the current
    /// draft is PARKED under the outgoing key as `Draft::to_toml`'s
    /// label pairs (the session's own form, where indices do not exist),
    /// and a parked draft for the INCOMING key is installed through the
    /// restore path: `Draft::from_toml` parks every edit at
    /// `UNRESOLVED_COLUMN` and `unresolved_restore` hands it to the first
    /// non-empty built model, which re-places it by label — or lands it
    /// `Behind` when the document moved while the trader was away, with
    /// the `:auto` policy's "first delivery after a restore is `hold`"
    /// rule covering it exactly as a session restore is covered. Nothing
    /// is ever refused and nothing is ever dropped: unsent work on every
    /// underlying survives, each under its own key.
    ///
    /// An open cell editor is CANCELLED, never committed, before the
    /// document is swapped (final review, B2) — a key change is
    /// navigation, and an editor left open across it would pass its own
    /// label-identity check on a same-ladder underlying and file the typed
    /// number into the NEW document's draft. Reachable: `i`, then `mod+l`
    /// (focus to the shell root, editor still open), then `:underlying`.
    /// That close is the only reason this takes a `Window`. It happens
    /// BEFORE the park, so the parked table is the draft as committed,
    /// never the draft plus a half-typed cell.
    fn set_key(&mut self, key: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.key.as_deref() == Some(key.as_slice()) {
            return;
        }
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        // Park the outgoing draft under its own underlying. A non-empty
        // draft with NO key (a hand-edited session's `draft` with no
        // `underlying`) has nothing to park under and stays put — the
        // first underlying named claims it, exactly as the constructor
        // already leaves it waiting for one.
        //
        // The rebase guard's group sizes are captured here too (controller
        // ruling), still under `capture_groups_if_base`'s same source-time
        // rule: `self.painted_snapshot()`/`self.key` still name the
        // OUTGOING underlying at this point (the reset a few lines below
        // has not run yet), so a park is exactly one more place the
        // painted model can be the draft's own base. `self.draft` is taken
        // out and put back rather than borrowed in place, the same
        // borrow-shape reason `fn rebase` does — see that call site.
        if let Some(outgoing) = self.key.take()
            && !self.draft.is_empty()
        {
            let mut draft = std::mem::take(&mut self.draft);
            self.capture_groups_if_base(&mut draft);
            self.parked.insert(outgoing, draft.to_toml());
        }
        if let Some(table) = self.parked.remove(&key) {
            self.draft = Draft::from_toml(&table);
        }
        self.unresolved_restore = !self.draft.is_empty();
        self.key = Some(key);
        self.title = Self::compute_title(self.spec, self.key.as_deref());
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
        self.query_in_flight = false;
        self.publication = None;
        // The tag moves on EVERY switch, visible or not (final review of
        // the per-underlying drafts branch, Minor 6): `requery` bumps it
        // on the visible path, but a hidden panel only `changed` — and an
        // outcome for the OLD key that slipped past `set_visible(false)`'s
        // cancel would then pass `deliver`'s tag check and be rebased onto
        // the new key's restored draft. Bumping here makes the invariant
        // hold by construction rather than by reachability.
        self.tag += 1;
        self.cursor = Cursor::Cell { row: 0, col: 0 };
        self.last_grid_col = 0;
        self.rebuild_model(cx);
        if self.visible {
            self.requery(cx);
        } else {
            self.changed(cx);
        }
    }

    /// The picker's row marks (spec 2026-09-14 §7, amended 2026-09-19):
    /// every parked underlying's display key to its draft's own
    /// `count_phrase`. Parsed from the parked tables HERE, once per
    /// picker open, never in `render`; the current underlying's own draft
    /// is not among them — the header's dirty dot already says so.
    fn parked_marks(&self) -> BTreeMap<String, String> {
        self.parked
            .iter()
            .map(|(key, table)| (display_key(key), Draft::from_toml(table).count_phrase()))
            .collect()
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let attrs: Vec<String> = self
            .spec
            .header
            .iter()
            .map(|a| a.column.to_string())
            .collect();
        commands::completions(
            line,
            cursor,
            &self.catalog_keys(cx),
            self.draft.is_behind(),
            &attrs,
        )
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
        // One `[drafts.<display key>]` per underlying that carries any
        // (2026-09-19): the current one's beside every parked one, the
        // parked tables written verbatim since they already ARE this
        // form. `toml::Table` insertion quotes a dotted key (`"SPX.Z"`)
        // on the way out, so the spelling round-trips through the
        // session file untouched. The legacy bare `draft` is written only
        // for a non-empty draft with no underlying at all — the one
        // shape that has no key to file it under.
        let mut drafts = toml::Table::new();
        if !self.draft.is_empty() {
            // The rebase guard (spec §2, amendment 4) needs group sizes
            // captured against the document currently painted — but only
            // when `painted_snapshot` really NAMES the base rather than
            // falling back to a newer arrival nobody's base ever was (the
            // M-1 path — `capture_groups_if_base`'s own doc comment).
            // Computed on a CLONE, not `self.draft` itself: `serialize`
            // takes `&self`, and this is the one capture site with no
            // `&mut` to write it back onto the live draft, so a same-day
            // group that changed size while nobody ran `:rebase` this
            // session is still caught on the NEXT restart rather than
            // only on the next explicit rebase.
            let mut draft = self.draft.clone();
            self.capture_groups_if_base(&mut draft);
            match &self.key {
                Some(key) => {
                    drafts.insert(display_key(key), toml::Value::Table(draft.to_toml()));
                }
                None => {
                    t.insert("draft".into(), toml::Value::Table(draft.to_toml()));
                }
            }
        }
        for (key, table) in &self.parked {
            drafts.insert(display_key(key), toml::Value::Table(table.clone()));
        }
        if !drafts.is_empty() {
            t.insert("drafts".into(), toml::Value::Table(drafts));
        }
        // Only a non-default policy is worth a key: a `hold` tile reads
        // exactly as one written before the key existed.
        if self.policy != UpdatePolicy::Hold {
            t.insert(
                "auto".into(),
                toml::Value::String(self.policy.as_str().to_string()),
            );
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

    /// The state of model row `row` (dividend spec §5.2), `None` past the
    /// grid's end.
    #[cfg(test)]
    pub(crate) fn row_state_at(&self, row: usize) -> Option<RowState> {
        self.model.rows.get(row).map(|r| r.state)
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Cursor {
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
        self.editor.as_ref().and_then(|e| match &e.state {
            EditorState::Text(state) => Some(state.clone()),
            EditorState::Date { .. } => None,
        })
    }

    /// What the open editor holds, `None` when none is open: the text
    /// editor's text, or the date field's committed `YYYY-MM-DD`.
    #[cfg(test)]
    pub(crate) fn editor_value(&self, cx: &App) -> Option<String> {
        self.editor.as_ref().map(|e| match &e.state {
            EditorState::Text(state) => state.read(cx).value().to_string(),
            EditorState::Date { field, .. } => field.text(),
        })
    }

    /// The open date field, `None` when the open editor is not one.
    #[cfg(test)]
    pub(crate) fn date_field(&self) -> Option<DateTimeField> {
        self.editor.as_ref().and_then(|e| match &e.state {
            EditorState::Date { field, .. } => Some(field.clone()),
            EditorState::Text(_) => None,
        })
    }

    /// Whether the open editor is the ROW-LABEL editor (dividend spec
    /// §5.3) — in either form.
    #[cfg(test)]
    pub(crate) fn label_editor_open(&self) -> bool {
        matches!(
            self.editor,
            Some(Editing {
                target: EditTarget::RowLabel { .. },
                ..
            })
        )
    }

    /// The open date field's own focus handle.
    #[cfg(test)]
    pub(crate) fn date_field_focus(&self) -> Option<FocusHandle> {
        self.editor.as_ref().and_then(|e| match &e.state {
            EditorState::Date { focus, .. } => Some(focus.clone()),
            EditorState::Text(_) => None,
        })
    }

    /// The open picker's own field entity — a test seeds a value through
    /// it, exactly as [`Self::editor_state`] does for the cell editor.
    #[cfg(test)]
    pub(crate) fn picker_state(&self) -> Option<Entity<InputState>> {
        match &self.popup {
            Some(Popup::Picker(p)) => Some(p.input.clone()),
            _ => None,
        }
    }

    /// Whether the choice cell's typeahead popup is open (dividend spec
    /// §4.4) — a test-only door; production code matches `popup` itself.
    #[cfg(test)]
    pub(crate) fn choice_popup_open(&self) -> bool {
        matches!(self.popup, Some(Popup::Choice(_)))
    }

    /// The open choice popup's own field entity — `picker_state`'s twin,
    /// for seeding a value the way `set_value` does (no `Change` event).
    #[cfg(test)]
    pub(crate) fn choice_state(&self) -> Option<Entity<InputState>> {
        match &self.popup {
            Some(Popup::Choice(c)) => Some(c.input.clone()),
            _ => None,
        }
    }

    /// The option the choice popup's highlight is on, `None` with no
    /// popup open or nothing ranked — read by TEXT, as
    /// [`Self::picker_highlighted_key`] is, so a test can tell a real
    /// re-rank from a coincidence of indices.
    #[cfg(test)]
    pub(crate) fn choice_highlighted(&self) -> Option<String> {
        match &self.popup {
            Some(Popup::Choice(c)) => c.list.highlighted_text().map(str::to_string),
            _ => None,
        }
    }

    /// The catalog key the picker's highlight is currently on, `None`
    /// with no picker open or an empty ranked list — the identity a
    /// re-rank (a keystroke, or a catalog change) must preserve, read by
    /// KEY rather than by index so a test can tell a real preservation
    /// from a coincidence.
    /// The open menu's highlighted row index, `None` with no menu open.
    #[cfg(test)]
    pub(crate) fn menu_highlighted(&self) -> Option<usize> {
        match &self.popup {
            Some(Popup::Menu(m)) => Some(m.highlighted),
            _ => None,
        }
    }

    /// The open menu's action rows as `(title, checked)`, in order — what
    /// a policy test reads to say which row carries the tick.
    #[cfg(test)]
    pub(crate) fn menu_checks(&self) -> Vec<(String, Option<bool>)> {
        match &self.popup {
            Some(Popup::Menu(m)) => m
                .rows
                .iter()
                .filter_map(|r| match r {
                    MenuRow::Action { title, checked, .. } => Some((title.to_string(), *checked)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn picker_highlighted_key(&self) -> Option<String> {
        match &self.popup {
            Some(Popup::Picker(p)) => p.rows.highlighted_key().map(str::to_string),
            _ => None,
        }
    }

    /// The open picker's painted labels, in painted order — what a
    /// trader reads, marks included — `None` with no picker open.
    #[cfg(test)]
    pub(crate) fn picker_labels(&self) -> Option<Vec<String>> {
        match &self.popup {
            Some(Popup::Picker(p)) => Some(
                p.rows
                    .painted()
                    .map(|i| p.rows.labels[i].to_string())
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The parked underlyings, in display spelling, with each draft's
    /// count phrase.
    #[cfg(test)]
    pub(crate) fn parked(&self) -> Vec<(String, String)> {
        self.parked_marks().into_iter().collect()
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
        self.header_texts_at(chrono::Utc::now())
    }

    /// [`Self::header_texts`] at an injected clock — the only way a test
    /// reaches [`Self::is_stale`]'s comparison (final review, B3): the
    /// wall-clock door above cannot say whether a fixed past `BASE` reads
    /// stale without knowing how far `now` has drifted past it.
    #[cfg(test)]
    pub(crate) fn header_texts_at(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<String> {
        let mut h = self.header.clone();
        h.stale = self.is_stale(now);
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

/// [`display_key`]'s inverse — a picker row's key, or a session's
/// `drafts.<key>` table name, back into the dataset's key parts. An
/// empty string is an empty key (no parts), which every reader treats
/// as "no key" rather than a one-part key of nothing.
pub(crate) fn parse_display_key(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split(KEY_DISPLAY_SEPARATOR)
        .map(str::to_string)
        .collect()
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

/// The declared [`ColumnType`] a NUMBER cell is parsed and nudged through
/// — `None` for the other three `CellKind`s (`Date`/`Text`/`Choice`,
/// which have no number to step or parse: `nudge` refuses on it, and
/// `commit_cell_edit` reaches it only from its `Number` arm) — the one
/// place both [`MarketDataTile::nudge`] and
/// [`MarketDataTile::commit_cell_edit`] read it, so a mutation to the
/// lookup itself has one site to anchor on rather than two that could
/// drift apart. The panel's own `value_type`
/// under a pivot (one value column, one declared type); the column's OWN
/// `ValueColumn::ty` under a flat panel, since a schedule's columns need
/// not agree — and need not even be the same NUMBER type: a flat `I64`
/// column commits `"3"` as `Value::I64(3)`, not `Value::F64(3.0)`,
/// because this reads the declared type rather than letting `parse_cell`
/// guess from what the text happens to parse as.
///
/// A free function, not a method: `nudge` calls it while a mutable
/// borrow of `self.editor` is already alive (`self.editor.as_mut()`),
/// which a `&self` method call would conflict with — passing `spec` and
/// `model` as their own arguments borrows only those two fields, exactly
/// as the inlined lookup this replaces already did.
fn declared_type(spec: &PanelSpec, model: &MatrixModel, col: usize) -> Option<ColumnType> {
    match model.kind_of(col)? {
        CellKind::Number(_) => Some(match &spec.columns {
            Columns::Axis(_) => spec.value_type,
            Columns::Values(cols) => cols.get(col).map_or(spec.value_type, |vc| vc.ty),
        }),
        CellKind::Date | CellKind::Text | CellKind::Choice(_) => None,
    }
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
        let cursor_attr = match self.cursor {
            Cursor::Attr(i) => Some(i),
            Cursor::Cell { .. } => None,
        };
        let editor = self.editor.as_ref().and_then(|e| match &e.target {
            EditTarget::Attr { index, .. } => Some((
                *index,
                match &e.state {
                    EditorState::Text(state) => EditorPaint::Text(state),
                    EditorState::Date { paint, focus, .. } => EditorPaint::Date { paint, focus },
                },
            )),
            EditTarget::Cell { .. } | EditTarget::RowLabel { .. } => None,
        });
        let menu_open = matches!(self.popup, Some(Popup::Menu(_)));
        let header = header::render(
            &self.header,
            cursor_attr,
            editor,
            menu_open,
            theme,
            &tones,
            &tile,
            self.id.0,
            self.menu_tip_selector.clone(),
            self.state_tip_selector.clone(),
            self.stack.as_ref(),
        );
        // The popup is anchored off a zero-size, absolutely positioned
        // sibling at the header's own right edge (spec §6.1) — `relative`
        // on the wrapper is what makes that positioning read against the
        // header rather than the window.
        let header =
            div()
                .relative()
                .w_full()
                .child(header)
                .when_some(self.popup.as_ref(), |el, p| {
                    let popup_el = match p {
                        Popup::Menu(m) => render_menu(m, &tile, self.id.0, cx).into_any_element(),
                        Popup::Picker(p) => {
                            render_picker(p, &tile, self.id.0, cx).into_any_element()
                        }
                        // Painted by the delegate, under its own cell
                        // (`MatrixDelegate::render_td`), never off the
                        // header.
                        Popup::Choice(_) => return el,
                    };
                    // Anchored just under the header strip, whose height
                    // this follows (`header::HEADER_HEIGHT`).
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(scale::design(header::HEADER_HEIGHT))
                            .child(popup_el),
                    )
                });

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
    use crate::core::draft::RowEdit;
    use crate::core::spec::{RowAxis, RowIdentity, RowLabel, ValueColumn};
    use crate::core::test_fixtures;
    use crate::core::{CVI, DIVIDEND, DraftState};
    use crate::delegate::LABEL_COL;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::groupings::GroupingSlots;
    use geode_core::log::LogLevels;
    use geode_core::query::{
        CatalogSnapshot, DatasetCatalog, GenerationInfo, PartitionCatalog, QueryKey, QueryOutcome,
    };
    use geode_core::scopes::SavedScopes;
    use geode_core::snapshot::{ColumnMeta, Freshness, Provenance, Snapshot, TestColumn};
    use geode_core::view::ColumnFormat;
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

    /// The slice-value columns every fixture document carries, ahead of
    /// the ladder in the grid: a node cell at ladder index `n` sits at
    /// grid column `SLICE + n`, and a fresh cursor at `(0, 0)` is on the
    /// first term's `fwd` (`4500.00`, two places), not its first node.
    const SLICE: usize = 3;

    /// A CVI document in the shape `query::document::compile_document`
    /// delivers one — `document_columns()` order, the value columns
    /// `DeterminedNonAdditive`, no grouping — `terms` × `nodes` rows with
    /// `param` running 0.1, 0.2, … in axis order, and per term the
    /// slice values `forward` = 4500 + 10 × term index, `atm` = 0.18 +
    /// 0.01 × term index, `skew` = −1.0 − 0.1 × term index, repeated on
    /// every node row of the term.
    fn document_of(terms: &[&str], nodes: &[f64], as_of: &str) -> Snapshot {
        let mut cells: Vec<(String, f64, f64)> = Vec::new();
        let mut slices: Vec<(f64, f64, f64)> = Vec::new();
        for (t, term) in terms.iter().enumerate() {
            for node in nodes {
                let i = cells.len() + 1;
                cells.push(((*term).to_string(), *node, i as f64 / 10.0));
                slices.push((
                    4500.0 + 10.0 * t as f64,
                    0.18 + 0.01 * t as f64,
                    -1.0 - 0.1 * t as f64,
                ));
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
                    meta("forward", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| Some(s.0)).collect()),
                ),
                (
                    meta("atm", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| Some(s.1)).collect()),
                ),
                (
                    meta("skew", Attribution::DeterminedNonAdditive),
                    TestColumn::F64(slices.iter().map(|s| Some(s.2)).collect()),
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
    ///
    /// `clicks` stands in for the shell's own tile-level bubble-phase
    /// mouse-down (`focus_main_tile`/`focus_dock_tile`, drag arming,
    /// `pending_focus_restore` — CLAUDE.md's focus rule): this crate's
    /// harness has one tile and no shell, so a real click-to-focus
    /// listener does not exist to observe directly. A plain bubble-phase
    /// `on_mouse_down` wrapping the tile stands in for it — if this
    /// crate's own capture-phase handlers ever swallowed propagation, a
    /// click on them would leave this counter unmoved exactly as it
    /// would leave the shell's own listeners unmoved.
    struct Host {
        tile: Entity<MarketDataTile>,
        clicks: Rc<StdCell<u32>>,
        /// Bubble-phase key-downs that reached the host — the stand-in
        /// for the shell root's own `handle_key_down`: a key the date
        /// field consumes must not count here, a chord must.
        keys: Rc<StdCell<u32>>,
        /// Bubble-phase, hover-gated mouse moves that reached the host —
        /// the stand-in for the grid rows beneath a popup: a move over an
        /// OCCLUDING popup must not count here, a move over the grid must.
        moves: Rc<StdCell<u32>>,
    }
    impl gpui::Render for Host {
        fn render(
            &mut self,
            _w: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            let clicks = self.clicks.clone();
            let moves = self.moves.clone();
            let keys = self.keys.clone();
            gpui::div()
                .size_full()
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, _cx| {
                    clicks.set(clicks.get() + 1);
                })
                .on_key_down(move |_, _, _cx| {
                    keys.set(keys.get() + 1);
                })
                .on_mouse_move(move |_, _, _cx| {
                    moves.set(moves.get() + 1);
                })
                .child(self.tile.clone())
        }
    }

    /// What the window closure hands back: it can return only one value,
    /// so everything a test drives or reads is parked here on the way out.
    struct Built {
        content: Box<dyn TileContent>,
        tile: Entity<MarketDataTile>,
        frame: Entity<Frame>,
        diagnostics: Entity<Diagnostics>,
        clicks: Rc<StdCell<u32>>,
        moves: Rc<StdCell<u32>>,
        keys: Rc<StdCell<u32>>,
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
        /// [`Host`]'s own bubble-phase click counter — the shell's
        /// tile-level mouse-down stand-in.
        clicks: Rc<StdCell<u32>>,
        /// [`Host`]'s hover-gated mouse-move counter — what a popup must
        /// occlude.
        moves: Rc<StdCell<u32>>,
        /// [`Host`]'s bubble-phase key-down counter — the shell root's
        /// listener stand-in.
        keys: Rc<StdCell<u32>>,
    }

    fn open(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_with(cx, None)
    }

    fn open_with(
        cx: &mut gpui::TestAppContext,
        restored: Option<toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_spec(cx, &CVI, restored)
    }

    /// A flat, dividend-schedule-shaped panel (spec §4.3, Task 3's typed
    /// cells) — `open`/`open_with` build over CVI's pivot, where every
    /// column is `Number`; this is the one both this crate's own
    /// `:bump`/`edit` refusal tests and Task 4's own tests build over
    /// instead, so a flat panel mixing `Date`/`Number`/`Choice` columns is
    /// exercised through the real tile, not just `matrix.rs`'s pure core.
    fn open_flat(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_spec(cx, &test_fixtures::SCHEDULE, None)
    }

    fn open_spec(
        cx: &mut gpui::TestAppContext,
        spec: &'static PanelSpec,
        restored: Option<toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        // The shell's own reclaims ride along, exactly as `main.rs`
        // installs them after the component's init: the panel's editor is
        // a component `Input` inside a component table, and which of the
        // two sees a keystroke first is decided by these bindings.
        cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
        // And this crate's own reclaim (`main.rs` calls it beside the
        // blotter's): gpui dispatches a keystroke's BINDINGS before any
        // `on_key_down` listener, so without it the component table's own
        // `up`/`down` actions would eat an arrow aimed at a date field
        // painted inside a cell before the field's listener ever saw it
        // — in the harness only, since the app always has this installed.
        cx.update(crate::init);
        let (data, rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data.clone(), spec, Duration::from_secs(15 * 60));
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
                    let clicks = Rc::new(StdCell::new(0));
                    let moves = Rc::new(StdCell::new(0));
                    let keys = Rc::new(StdCell::new(0));
                    *slot.borrow_mut() = Some(Built {
                        content: occupant.content,
                        tile: tile.clone(),
                        frame,
                        diagnostics,
                        clicks: clicks.clone(),
                        moves: moves.clone(),
                        keys: keys.clone(),
                    });
                    let host = cx.new(|_| Host {
                        tile,
                        clicks,
                        moves,
                        keys,
                    });
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
                clicks: built.clicks,
                moves: built.moves,
                keys: built.keys,
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
        /// [`Host`]'s own bubble-phase click count — the shell's
        /// tile-level mouse-down stand-in (see `Host`'s own doc comment).
        fn host_clicks(&self) -> u32 {
            self.clicks.get()
        }
        fn host_moves(&self) -> u32 {
            self.moves.get()
        }
        fn host_keys(&self) -> u32 {
            self.keys.get()
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
        /// Seed the open picker's field — `set_editor`'s own trick, and
        /// the same reason: `InputState::set_value` emits no `Change` at
        /// all, which is exactly what `commit_picker`'s own defensive
        /// re-rank exists to cover (see its doc comment).
        fn set_picker_text(&self, vcx: &mut gpui::VisualTestContext, text: &str) {
            let state = self
                .tile
                .read_with(vcx, |t, _| t.picker_state())
                .expect("an open picker");
            vcx.update(|window, cx| {
                state.update(cx, |s, cx| s.set_value(text, window, cx));
            });
        }
        /// Seed the open choice popup's field — `set_picker_text`'s twin,
        /// and the same reason: `set_value` emits no `Change`, which is
        /// what `commit_choice`'s own re-rank exists to cover.
        fn set_choice_text(&self, vcx: &mut gpui::VisualTestContext, text: &str) {
            let state = self
                .tile
                .read_with(vcx, |t, _| t.choice_state())
                .expect("an open choice popup");
            vcx.update(|window, cx| {
                state.update(cx, |s, cx| s.set_value(text, window, cx));
            });
        }
        /// The header as painted, off the tile's own reader.
        fn header_texts(&self, vcx: &gpui::VisualTestContext) -> Vec<String> {
            self.tile.read_with(vcx, |t, _| t.header_texts())
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
            self.with_document_tagged(vcx);
        }
        /// [`Self::with_document`], answering the request's tag so a
        /// test can deliver a further generation for the same question.
        fn with_document_tagged(&self, vcx: &mut gpui::VisualTestContext) -> u64 {
            self.command(vcx, "key SPX.Z").expect("a valid key");
            self.visible(vcx, true);
            let tag = self.document_request().expect("one request").tag;
            self.deliver(vcx, tag, Arc::new(cvi(BASE)));
            tag
        }

        /// [`Self::with_document`]'s flat-panel twin, over [`open_flat`]:
        /// key, shown, requested, delivered — two `SCHEDULE`-shaped rows
        /// (`D1`/`D2`), the same key column (`underlying_ref`, `SPX.Z`) a
        /// pivot's document is asked for by.
        fn with_flat_document(&self, vcx: &mut gpui::VisualTestContext) {
            self.with_flat_document_with(
                vcx,
                test_fixtures::schedule_snapshot(&[
                    ("D1", "2026-12-18", 1.25, "declared"),
                    ("D2", "2027-03-19", 0.5, "estimated"),
                ]),
            );
        }
        /// [`Self::with_flat_document`], over a caller-supplied snapshot —
        /// for a fixture with no place in the general one, such as a NULL
        /// cell.
        fn with_flat_document_with(&self, vcx: &mut gpui::VisualTestContext, snapshot: Snapshot) {
            self.command(vcx, "key SPX.Z").expect("a valid key");
            self.visible(vcx, true);
            let tag = self.document_request().expect("one request").tag;
            self.deliver(vcx, tag, Arc::new(snapshot));
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
        /// The choice popup as the DELEGATE holds it — the painted rows
        /// and the lit one — `None` with no popup mirrored. What
        /// `render_td` paints, as distinct from the tile's own list.
        fn delegate_choice_rows(
            &self,
            vcx: &gpui::VisualTestContext,
        ) -> Option<(Vec<String>, usize)> {
            self.tile.read_with(vcx, |t, cx| {
                t.table().read(cx).delegate().choice.as_ref().map(|c| {
                    (
                        c.paint.rows.iter().map(|r| r.to_string()).collect(),
                        c.paint.highlighted,
                    )
                })
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
    fn centre_of(vcx: &mut gpui::VisualTestContext, selector: &str) -> gpui::Point<gpui::Pixels> {
        draw(vcx);
        // `debug_bounds` wants `&'static str`; a formatted selector (a
        // click test naming one tile's attribute index, say) is not one,
        // so it is leaked here — a test-only cost, once per call.
        let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
        vcx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is painted"))
            .center()
    }

    /// A bare mouse move to `at` — what hovering is made of. Two frames
    /// follow, since gpui resolves hover on the draw after the move.
    fn move_to(vcx: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>) {
        vcx.simulate_event(gpui::MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: gpui::Modifiers::default(),
        });
        draw(vcx);
        draw(vcx);
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

        assert_eq!(h.columns(&vcx), 1 + SLICE + NODES.len());
        assert_eq!(
            h.headers(&vcx),
            vec!["term", "fwd", "atm", "skew", "-20", "-1", "3.5"],
            "the row axis's own name, the slice values, then the node labels in document order"
        );
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-th-6").is_some(),
            "the third node's header is painted"
        );

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&TERMS, &NODES[..2], BASE)),
        );
        draw(&mut vcx);
        assert_eq!(h.columns(&vcx), 1 + SLICE + 2, "a node fewer");
        assert!(
            vcx.debug_bounds("marketdata-th-5").is_some(),
            "two nodes are still painted"
        );
        assert!(
            vcx.debug_bounds("marketdata-th-6").is_none(),
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );

        // Row 1, the third node: grid column 5 (after the three slice
        // values), table column 6.
        let at = centre_of(&mut vcx, "marketdata-cell-1-6");
        click_at(&mut vcx, at, 1);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 5 },
            "the click moved the cursor to that cell"
        );
        assert_eq!(
            h.selection(&vcx),
            (Some(1), Some(6)),
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 2 }
        );
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
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
            Cursor::Cell { row: 1, col: 2 },
            "a click on a row label moves the row and leaves the column where it was"
        );
        assert_eq!(h.selection(&vcx), (Some(1), Some(3)));
    }

    /// A double-click opens the editor on the clicked cell (user ruling
    /// 2026-09-17, reversing 2026-09-14's "editing is keyboard-only"):
    /// the cursor lands on it, the editor is seeded with the cell's own
    /// painted text, `key_context` reports `insert`, and after a draw the
    /// editor's input still holds window focus. What made the mapping
    /// honourable is the shell's insert-mode rule on its focus restore
    /// (`ShellView::render`, tested in `geode-shell`); this crate's part
    /// is to open on the click and focus the input.
    #[gpui::test]
    fn a_double_click_opens_the_editor_on_the_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        let at = centre_of(&mut vcx, "marketdata-cell-1-5");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 4 },
            "the cursor moved to the clicked cell"
        );
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("0.5000"),
            "the editor opened, seeded with the cell's painted text"
        );
        assert_eq!(h.mode(&vcx), "insert");
        draw(&mut vcx);
        let input = h.tile.read_with(&vcx, |t, _| t.editor_state()).unwrap();
        assert!(
            vcx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
            "the editor's input holds the keyboard after the next frame"
        );
        assert!(
            h.tile
                .read_with(&vcx, |t, cx| t.table().read(cx).delegate().editor.is_some()),
            "and it is painted in the cell"
        );
    }

    /// A single click still only moves the cursor: the editor is the
    /// double-click's alone.
    #[gpui::test]
    fn a_single_click_still_only_moves_the_cursor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        let at = centre_of(&mut vcx, "marketdata-cell-1-5");
        click_at(&mut vcx, at, 1);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 4 }
        );
        assert_eq!(h.editor_value(&vcx), None, "one click opens nothing");
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// A double-click on the row-label column is a click: the row moves,
    /// the column stays, and nothing opens — there is no cell to edit.
    #[gpui::test]
    fn a_double_click_on_a_row_label_opens_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));

        let at = centre_of(&mut vcx, "marketdata-cell-1-0");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 2 }
        );
        assert_eq!(h.editor_value(&vcx), None);
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// A double-click meets `i`'s own refusals: while the draft is
    /// `Behind` the notice names `:rebase`/`:revert`, nothing opens, and
    /// the mode stays `normal`.
    #[gpui::test]
    fn a_double_click_while_behind_is_refused_with_the_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));

        let at = centre_of(&mut vcx, "marketdata-cell-1-5");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert_eq!(h.editor_value(&vcx), None, "nothing opened");
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(BEHIND_REFUSED.to_string())
        );
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

        let at = centre_of(&mut vcx, "marketdata-cell-1-6");
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
            Cursor::Cell { row: 1, col: 5 },
            "and the cursor moved to the clicked cell"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a click is not `enter`: nothing was written"
        );
        assert_eq!(
            h.cell(&vcx, 0, 0).0,
            "4500.00",
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
                (d.cursor, d.editor.as_ref().map(|e| (e.row, e.col)))
            })
        };
        assert_eq!(mirror(&vcx), (Some((0, 0)), None));

        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(
            mirror(&vcx),
            (Some((1, 2)), None),
            "every cursor move reaches the delegate — in MODEL coordinates"
        );

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(
            mirror(&vcx),
            (Some((1, 2)), Some((1, Some(2)))),
            "and so does the open editor's own cell"
        );
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(
            mirror(&vcx),
            (Some((1, 2)), None),
            "cancel clears the mirror too"
        );
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
        assert_eq!(
            columns,
            SLICE + 3,
            "three slice values then three nodes across the top"
        );
        assert!(chips.iter().any(|c| c == "CVI"), "the title: {chips:?}");
        assert!(
            chips.iter().any(|c| c == "SPX.Z"),
            "the underlying: {chips:?}"
        );
        assert!(
            chips.iter().any(|c| c == "spot 5000"),
            "each header attribute: {chips:?}"
        );
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let local = clock.hms(chrono::DateTime::parse_from_rfc3339(BASE).unwrap().to_utc());
        // Exact, at an injected clock one second past `BASE`: not stale,
        // so the chip is the time alone (the staleness marker has its own
        // test below).
        let chips = h
            .tile
            .read_with(&vcx, |t, _| t.header_texts_at(base_plus(1)));
        assert!(
            chips.iter().any(|c| c == &local),
            "the source time on the trader's own clock ({local}): {chips:?}"
        );
    }

    /// The stack marker (tile-stacks spec §5.1) paints only while the
    /// tile is a stack member with more than one member, first in the
    /// header strip.
    #[gpui::test]
    fn the_stack_marker_paints_only_while_a_member(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert!(vcx.debug_bounds("stack-marker-3").is_none());

        h.tile.update(&mut vcx, |t, cx| {
            t.set_stack(Some(StackHandle::new(2, 4, |_, _| {})), cx);
        });
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let marker = vcx.debug_bounds("stack-marker-3").expect("painted");
        let header = vcx.debug_bounds("marketdata-header-3").unwrap();
        assert!(
            marker.left() - header.left() < gpui::px(20.0),
            "first in the strip"
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.title()).as_ref(), "CVI");
    }

    /// Whole-branch review, Minor 7: the title's cache (`Self::title`,
    /// `compute_title`) must follow a real key change too, not just read
    /// correctly with no underlying set.
    #[gpui::test]
    fn title_follows_the_underlying(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.title()).as_ref(),
            "CVI · NKY.Z"
        );
    }

    /// `BASE` plus `secs` seconds — the injected clock `header_texts_at`
    /// reads staleness against.
    fn base_plus(secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Utc)
            + chrono::Duration::seconds(secs)
    }

    /// The time chip reads ` stale` once `now` is past the painted
    /// generation's source time by more than `stale_after` (the harness's
    /// factory is built with fifteen minutes) and not a second before —
    /// `is_stale`'s comparison, reachable only through the injected clock
    /// (final review, B3).
    #[gpui::test]
    fn the_time_chip_says_stale_past_stale_after(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let local = clock.hms(chrono::DateTime::parse_from_rfc3339(BASE).unwrap().to_utc());
        let stale_after = 15 * 60;
        let fresh = h
            .tile
            .read_with(&vcx, |t, _| t.header_texts_at(base_plus(1)));
        assert!(
            fresh.iter().any(|c| c == &local),
            "not stale at +1s: {fresh:?}"
        );
        let at_limit = h
            .tile
            .read_with(&vcx, |t, _| t.header_texts_at(base_plus(stale_after)));
        assert!(
            at_limit.iter().any(|c| c == &local),
            "exactly stale_after is not yet stale: {at_limit:?}"
        );
        let stale = h
            .tile
            .read_with(&vcx, |t, _| t.header_texts_at(base_plus(stale_after + 1)));
        assert!(
            stale.iter().any(|c| c == &format!("{local} stale")),
            "stale one second past stale_after: {stale:?}"
        );
    }

    /// Review finding (Task 7): no test installed `AppClock` before this
    /// fix, so a tile that hard-coded `Clock::machine()` — or whose
    /// `observe_global` handler were deleted — would have passed
    /// everything else. `BASE` is `"2026-09-12T14:00:00Z"`: Tokyo is
    /// UTC+9, so the header's source time reads `23:00:00` there and
    /// `14:00:00` once the global switches to UTC — both spelled by
    /// hand, not derived through `Clock` (the thing under test).
    #[gpui::test]
    fn the_header_time_reads_the_installed_app_clock_and_follows_a_later_change(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_global(geode_shell::clock::AppClock(
                geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
            ))
        });
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let before = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        assert_eq!(
            before.as_deref(),
            Some("23:00:00"),
            "Tokyo is UTC+9 on the BASE instant 14:00:00Z: {before:?}"
        );

        vcx.update(|_, cx| {
            cx.set_global(geode_shell::clock::AppClock(geode_core::clock::Clock::utc()))
        });
        vcx.run_until_parked();
        let after = h.tile.read_with(&vcx, |t, _| t.header.time.clone());
        assert_eq!(
            after.as_deref(),
            Some("14:00:00"),
            "the observer refreshed the tile's clock and repainted: {after:?}"
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

    /// User ruling 2026-09-19 ("keep them per underlying"), superseding
    /// the 2026-09-14 refusal: a key change with edits pending PARKS the
    /// current draft under its own underlying and switches. Nothing is
    /// discarded and nothing is refused — the new document is requested,
    /// the header's dot goes off (it reads the CURRENT draft alone) and
    /// the parked draft is on record for the picker and the session.
    #[gpui::test]
    fn a_key_change_parks_the_draft_instead_of_refusing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));

        assert_eq!(h.command(&mut vcx, "underlying NKY.Z"), Ok(()));
        let (dirty, len, parked, key) = h.tile.read_with(&vcx, |t, _| {
            (
                t.header_dirty(),
                t.draft().len(),
                t.parked(),
                t.serialize()["underlying"][0].as_str().map(str::to_string),
            )
        });
        assert!(!dirty, "the dot reads the current draft, which is empty");
        assert_eq!(len, 0);
        assert_eq!(
            parked,
            vec![("SPX.Z".to_string(), "1 cell, spot_ref".to_string())],
            "SPX's draft is parked under SPX, as a count the picker can name"
        );
        assert_eq!(key.as_deref(), Some("NKY.Z"));
        let req = h.document_request().expect("the switch asks for NKY");
        assert_eq!(req.document_key, vec!["NKY.Z".to_string()]);
    }

    /// Coming back is a restore: the parked draft is installed through
    /// the same door a session's draft is, resolved by label against the
    /// first non-empty model, so the edits are back on their cells and
    /// the attribute edit on its chip — `Editing`, not `Behind`, because
    /// the document did not move while the trader was away.
    #[gpui::test]
    fn returning_to_an_underlying_restores_its_parked_draft(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();

        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        assert!(
            !h.cell(&vcx, 0, 0).1,
            "an SPX edit never paints on NKY's grid"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.model().header[1].text.to_string()),
            "5000",
            "nor does its attribute edit"
        );

        h.command(&mut vcx, "underlying SPX.Z").unwrap();
        let (dirty, len, parked) = h
            .tile
            .read_with(&vcx, |t, _| (t.header_dirty(), t.draft().len(), t.parked()));
        assert!(dirty, "the dot is back on the keystroke that switches");
        assert_eq!(len, 2, "the cell and the attribute, awaiting a document");
        assert!(parked.is_empty(), "nothing is parked any more");
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let (text, edited) = h.cell(&vcx, 0, 0);
        assert_eq!(text, "9.90");
        assert!(edited, "the cell edit is back on its cell");
        let (spot, spot_edited, state) = h.tile.read_with(&vcx, |t, _| {
            (
                t.model().header[1].text.to_string(),
                t.model().header[1].edited,
                t.draft().state.clone(),
            )
        });
        assert_eq!(spot, "4520");
        assert!(spot_edited);
        assert_eq!(state, DraftState::Editing, "same generation: not Behind");
    }

    /// The document moved while the draft was parked: the return lands
    /// `Behind` with the edits parked at their labels and the header
    /// reading `update HH:MM` — and, being a restore, the first delivery
    /// is `hold` even under `:auto replace` (ruling 2026-09-19); the
    /// policy acts only on the NEXT new generation.
    #[gpui::test]
    fn a_parked_draft_whose_document_moved_returns_behind_and_holds_once(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.command(&mut vcx, "auto replace").unwrap();

        h.command(&mut vcx, "underlying SPX.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        let (state, len) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().state.clone(), t.draft().len()));
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "held, not replaced, on the first delivery after a restore — got {state:?}"
        );
        assert_eq!(len, 1, "the edit survives, parked at its label");
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let local = clock.hm(chrono::DateTime::parse_from_rfc3339(NEWER)
            .unwrap()
            .to_utc());
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c == &format!("update {local}")),
            "{chips:?}"
        );

        // The next NEW generation is a live delivery, and `replace` acts.
        h.deliver(&mut vcx, tag, Arc::new(cvi("2026-09-12T14:20:00Z")));
        let (state, len) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().state.clone(), t.draft().len()));
        assert_eq!(state, DraftState::Clean, "replaced on the next generation");
        assert_eq!(len, 0);
    }

    /// Two underlyings, two drafts, switched back and forth twice: each
    /// comes back intact under its own key, and neither ever paints on
    /// the other's grid.
    #[gpui::test]
    fn two_underlyings_keep_two_drafts_with_no_cross_talk(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);

        // NKY: a different cell, a different value.
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "7.7");
        h.dispatch(&mut vcx, "commit", None);
        assert!(!h.cell(&vcx, 0, 0).1, "SPX's cell is clean on NKY");
        assert_eq!(h.cell(&vcx, 0, 1), ("7.7000".to_string(), true));

        for _ in 0..2 {
            h.command(&mut vcx, "underlying SPX.Z").unwrap();
            let tag = h.document_request().unwrap().tag;
            h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
            assert_eq!(h.cell(&vcx, 0, 0), ("9.90".to_string(), true));
            assert!(!h.cell(&vcx, 0, 1).1, "NKY's cell is clean on SPX");
            assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 1);

            h.command(&mut vcx, "underlying NKY.Z").unwrap();
            let tag = h.document_request().unwrap().tag;
            h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
            assert_eq!(h.cell(&vcx, 0, 1), ("7.7000".to_string(), true));
            assert!(!h.cell(&vcx, 0, 0).1, "SPX's cell is clean on NKY");
            assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 1);
        }
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.parked()),
            vec![("SPX.Z".to_string(), "1 cell".to_string())]
        );
    }

    /// `:revert` (and every other draft verb) acts on the CURRENT
    /// underlying's draft alone: reverting NKY leaves SPX's parked draft
    /// exactly where it was.
    #[gpui::test]
    fn revert_touches_only_the_current_underlyings_draft(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.command(&mut vcx, "set spot_ref 1").unwrap();

        h.command(&mut vcx, "revert").unwrap();
        let (len, parked) = h.tile.read_with(&vcx, |t, _| (t.draft().len(), t.parked()));
        assert_eq!(len, 0, "NKY's draft is gone");
        assert_eq!(
            parked,
            vec![("SPX.Z".to_string(), "1 cell".to_string())],
            "SPX's is untouched"
        );
        h.command(&mut vcx, "underlying SPX.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        assert_eq!(h.cell(&vcx, 0, 0), ("9.90".to_string(), true));
    }

    /// Parked drafts ride the session (spec §8.5, amended 2026-09-19):
    /// one `[drafts.<underlying>]` per underlying with edits — the
    /// current one's beside every parked one — and a restore installs the
    /// restored underlying's own entry as the current draft, keeping the
    /// rest parked until each is loaded.
    #[gpui::test]
    fn parked_drafts_ride_the_session(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        h.command(&mut vcx, "set spot_ref 4520").unwrap();

        let written = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert!(
            written.get("draft").is_none(),
            "the legacy key is not written"
        );
        let drafts = written["drafts"].as_table().expect("a drafts table");
        assert_eq!(
            drafts.keys().cloned().collect::<Vec<_>>(),
            vec!["NKY.Z".to_string(), "SPX.Z".to_string()],
            "the current draft and the parked one, each under its key"
        );
        assert_eq!(
            drafts["SPX.Z"]["edits"][0][2].as_float(),
            Some(9.9),
            "SPX's cell edit as a label pair"
        );
        assert_eq!(
            drafts["NKY.Z"]["attrs"]["spot_ref"].as_float(),
            Some(4520.0)
        );
        // A dotted key is quoted on the way to disk and comes back whole.
        let text = toml::to_string(&written).unwrap();
        assert!(text.contains("[drafts.\"SPX.Z\"]"), "{text}");
        let reread: toml::Table = text.parse().unwrap();
        assert_eq!(reread, written);

        // A restart onto NKY (the written `underlying`): NKY's edits are
        // back on the first delivery, SPX's are parked.
        let (h, mut vcx) = open_with(cx, Some(reread));
        h.visible(&mut vcx, true);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.parked()),
            vec![("SPX.Z".to_string(), "1 cell".to_string())]
        );
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let (spot, spot_edited) = h.tile.read_with(&vcx, |t, _| {
            (
                t.model().header[1].text.to_string(),
                t.model().header[1].edited,
            )
        });
        assert_eq!(spot, "4520");
        assert!(spot_edited, "NKY's attribute edit is restored");
        h.command(&mut vcx, "underlying SPX.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        assert_eq!(
            h.cell(&vcx, 0, 0),
            ("9.90".to_string(), true),
            "and SPX's cell edit once SPX is loaded"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.parked()),
            vec![("NKY.Z".to_string(), "spot_ref".to_string())],
            "NKY's restored draft is now the parked one"
        );
    }

    /// The picker names an underlying's parked edits in its row label —
    /// `SPX.Z · 1 cell, spot_ref` — while the current underlying's row
    /// stays bare, and the query still ranks over the bare key.
    #[gpui::test]
    fn a_picker_row_names_an_underlyings_parked_edits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["NKY.Z", "SPX.Z"]));
            cx.notify();
        });

        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(
            h.mode(&vcx),
            "insert",
            "the picker opened on a dirty history"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_labels()),
            Some(vec![
                "NKY.Z".to_string(),
                "SPX.Z \u{b7} 1 cell, spot_ref".to_string()
            ])
        );
        h.set_picker_text(&mut vcx, "sp");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.serialize()["underlying"][0]
                .as_str()
                .map(str::to_string)),
            Some("SPX.Z".to_string()),
            "`sp` ranked the bare key and enter loaded it"
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell {
                row: 4,
                col: SLICE + 2
            }
        );

        // Two terms and two nodes now: both axes shrank under the cursor.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&TERMS, &NODES[..2], BASE)),
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell {
                row: 1,
                col: SLICE + 1
            },
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 3, col: 0 }
        );
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 3, col: 2 }
        );
        h.dispatch(&mut vcx, "down", Some(9));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 4, col: 2 },
            "a count past the end clamps"
        );
        h.dispatch(&mut vcx, "top", None);
        h.dispatch(&mut vcx, "first_col", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
        h.dispatch(&mut vcx, "last_col", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell {
                row: 0,
                col: SLICE + 2
            },
            "the last column is the last NODE, past the slice values"
        );

        // The scroll is the table's now: `sync_cursor` sets the selected
        // row (which scrolls it into view) and the selected column, so the
        // selection is what a test reads — the blotter's own tests read
        // `selected_row` exactly this way.
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(
            h.selection(&vcx),
            (Some(terms.len() - 1), Some(LABEL_COL + 1 + SLICE + 2)),
            "G moves the table's selected row, which is what scrolls it into view"
        );
    }

    /// Spec §20.5 on the panel: a bare `j` wraps, a counted one clamps,
    /// the full-page pair moves ten, and `h`/`l` never wrap.
    #[gpui::test]
    fn a_bare_row_step_wraps_and_the_full_page_keys_move_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        let terms: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        h.deliver(&mut vcx, tag, Arc::new(document_of(&terms, &NODES, BASE)));

        // The strip sits ABOVE the wrap cycle (panel-header spec §11's
        // merge ruling): with attributes to enter, `k` on row 0 goes to
        // the strip rather than wrapping, so the wrap is shown through
        // `j` at the bottom; the no-attribute `k` wrap is a pure
        // `core::cursor` test.
        let row = |vcx: &gpui::VisualTestContext| match h.tile.read_with(vcx, |t, _| t.cursor()) {
            Cursor::Cell { row, .. } => row,
            Cursor::Attr(_) => panic!("expected a grid cursor"),
        };
        let col = |vcx: &gpui::VisualTestContext| match h.tile.read_with(vcx, |t, _| t.cursor()) {
            Cursor::Cell { col, .. } => col,
            Cursor::Attr(_) => panic!("expected a grid cursor"),
        };
        h.dispatch(&mut vcx, "bottom", None);
        assert_eq!(row(&vcx), 11);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(row(&vcx), 0, "a bare j at the bottom wraps to row 0");
        h.dispatch(&mut vcx, "up", None);
        assert!(
            matches!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(0)),
            "a bare k at the top enters the strip, never wraps, while attributes exist"
        );
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(row(&vcx), 0, "and j returns to the top row");
        h.dispatch(&mut vcx, "down", Some(20));
        assert_eq!(row(&vcx), 11, "a counted step clamps");
        h.dispatch(&mut vcx, "page_up_full", None);
        assert_eq!(row(&vcx), 1, "ctrl+b moves ten");
        h.dispatch(&mut vcx, "page_down_full", None);
        assert_eq!(row(&vcx), 11);
        h.dispatch(&mut vcx, "page_down_full", None);
        assert_eq!(row(&vcx), 11, "and clamps at the end");
        h.dispatch(&mut vcx, "last_col", None);
        let last = col(&vcx);
        h.dispatch(&mut vcx, "right", None);
        assert_eq!(col(&vcx), last, "columns clamp: l at the last column stays");
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
            commands::completions("", 0, &[], false, &[]),
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
[drafts."SPX.Z"]
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

    /// A session written before 2026-09-19 carries the current draft as
    /// a bare `draft`: it is still read as the restored underlying's own
    /// and written back under `drafts.<underlying>`.
    #[gpui::test]
    fn a_legacy_draft_key_still_restores_as_the_underlyings_draft(cx: &mut gpui::TestAppContext) {
        let legacy: toml::Table = format!(
            r#"
underlying = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#
        )
        .parse()
        .unwrap();
        let (h, vcx) = open_with(cx, Some(legacy.clone()));
        let (len, written) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().len(), t.serialize()));
        assert_eq!(len, 1, "the legacy draft is the current draft");
        assert!(written.get("draft").is_none());
        assert_eq!(written["drafts"]["SPX.Z"], legacy["draft"]);
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
            .read_with(&vcx, |t, _| t.model().rows[1].cells[SLICE + 1].clone());
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
        assert_eq!(clipboard(&mut vcx).as_deref(), Some("4500.00"));
        h.dispatch(&mut vcx, "yank_row", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("2026-10-16\t4500.00\t0.1800\t-1.0000\t0.1000\t0.2000\t0.3000"),
            "the row is label then cells — the slice values included — tab separated"
        );
        h.dispatch(&mut vcx, "yank_col", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("4500.00\n4510.00"),
            "the column is newline separated"
        );
        h.dispatch(&mut vcx, "right", Some(SLICE as u32));
        h.dispatch(&mut vcx, "yank_col", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("0.1000\n0.4000"),
            "a node column, past the slice values"
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
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 },
            "the cursor jumps to the matching term"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 },
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
            Some("4500.00"),
            "seeded with the cell's own text, so a small correction is a small edit"
        );

        h.set_editor(&mut vcx, "4505.5");
        h.dispatch(&mut vcx, "commit", None);

        assert_eq!(
            h.cell(&vcx, 0, 0),
            ("4505.50".to_string(), true),
            "the parsed value, formatted by the slice value's own format, marked as an edit"
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
        assert_eq!(h.cell(&vcx, 0, 0), ("0.50".to_string(), true));
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
            ("4500.00".to_string(), false),
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
            vec!["4500.00", "0.1800", "-1.0000", "0.3500", "0.4500", "0.5500"],
            "the cursor's whole ladder row moved; its slice values did not"
        );
        assert_eq!(
            h.row_texts(&vcx, 1),
            vec!["4510.00", "0.1900", "-1.1000", "0.4000", "0.5000", "0.6000"],
            "and nothing else did"
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 3);

        h.command(&mut vcx, "revert").unwrap();
        h.dispatch(&mut vcx, "right", Some(SLICE as u32));
        h.command(&mut vcx, "bump 1 col").unwrap();
        assert_eq!(
            h.col_texts(&vcx, SLICE),
            vec!["1.1000", "1.4000"],
            "`col` walks the cursor's column instead"
        );
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec!["4500.00", "0.1800", "-1.0000", "1.1000", "0.2000", "0.3000"],
            "and only that column"
        );

        // It composes with an edit already made rather than reading through
        // to the document underneath it.
        h.command(&mut vcx, "bump 1 col").unwrap();
        assert_eq!(h.col_texts(&vcx, SLICE), vec!["2.1000", "2.4000"]);
    }

    /// The slice values (2026-09-17): a ROW bump walks the term's ladder
    /// and leaves its `fwd`/`atm`/`skew` alone — bumping a term's vols
    /// must not move its forward — while a COLUMN bump with the cursor on
    /// `fwd` bumps every term's forward, which is what that column means.
    #[gpui::test]
    fn a_row_bump_skips_the_slice_cells_and_a_column_bump_on_fwd_moves_every_term(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        // The cursor is on `fwd`; the row bump still walks the ladder.
        h.command(&mut vcx, "bump 0.1").unwrap();
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec!["4500.00", "0.1800", "-1.0000", "0.2000", "0.3000", "0.4000"],
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            NODES.len(),
            "one edit per node, none per slice value"
        );

        h.command(&mut vcx, "bump 1 col").unwrap();
        assert_eq!(h.col_texts(&vcx, 0), vec!["4501.00", "4511.00"]);
        assert_eq!(
            h.col_texts(&vcx, 1),
            vec!["0.1800", "0.1900"],
            "the neighbouring slice column is untouched"
        );
    }

    /// A flat panel's row bump (spec §4.3): `SCHEDULE`'s three columns are
    /// `ex` (`Date`), `amount` (`Number`) and `status` (`Choice`) — only
    /// `amount` is `Number`-kind, so a row bump moves it alone and leaves
    /// the other two with no edit at all, never a coerced one.
    #[gpui::test]
    fn a_flat_panels_row_bump_moves_only_the_number_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);

        h.command(&mut vcx, "bump 1 row").expect("a bump on row 0");
        assert_eq!(
            h.cell(&vcx, 0, 1),
            ("2.2500".to_string(), true),
            "amount (1.25 + 1) is edited"
        );
        assert_eq!(
            h.cell(&vcx, 0, 0),
            ("2026-12-18".to_string(), false),
            "ex is untouched — a date has nothing to add to"
        );
        assert_eq!(
            h.cell(&vcx, 0, 2),
            ("declared".to_string(), false),
            "status is untouched — a choice has nothing to add to"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            1,
            "one edit, not three"
        );
    }

    /// A row bump whose only `Number` column is itself NULL (spec §4.3):
    /// `ex`/`status` are skipped for their KIND, `amount` for being NULL
    /// — three cells, zero values, either way. What distinguishes this
    /// from the happy-path row-bump test above is the REFUSAL: it must
    /// still name two columns skipped for their kind, not fall back to
    /// the generic "no values to bump" a `CellKind`-blind row walk would
    /// produce (the outcome — nothing edited — is identical either way,
    /// so only the message tells the two apart).
    #[gpui::test]
    fn a_flat_panels_row_bump_names_the_kind_skipped_count(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot_with_null_amount(),
        );

        assert_eq!(
            h.command(&mut vcx, "bump 1 row"),
            Err("no numeric cells to bump (2 skipped)".to_string())
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    }

    /// A flat panel's column bump (spec §4.3): the cursor on `status` (a
    /// `Choice` column) refuses the whole column outright rather than
    /// silently bumping nothing.
    #[gpui::test]
    fn a_flat_panels_column_bump_on_a_non_numeric_column_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));

        assert_eq!(
            h.command(&mut vcx, "bump 1 col"),
            Err("not a numeric column".to_string())
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "nothing was written"
        );
    }

    /// The shipped `DIVIDEND` panel's own `Columns::Values` branch of
    /// `ty_of` (task 3 review, fix round 1): a row bump lands `amount` as
    /// `Value::F64` — proven against the REAL spec, not `SCHEDULE`'s
    /// three-column stand-in, since `ty_of`'s positional read off
    /// `spec.flat_columns()` is only as trustworthy as the spec it is
    /// actually tested against. `ex`/`announced`/`pay` are `Date` and
    /// `status` is a `Choice`, so the row bump reaches `amount` (index 3)
    /// alone, the same shape `a_flat_panels_row_bump_moves_only_the_number_column`
    /// proves over `SCHEDULE`.
    #[gpui::test]
    fn a_dividend_panels_row_bump_lands_amount_as_f64(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &DIVIDEND, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::dividend_snapshot(&[(
                "D1",
                "2026-12-18",
                "2026-11-01",
                "2027-01-05",
                1.25,
                "declared",
            )])),
        );

        h.command(&mut vcx, "bump 1 row").expect("a bump on row 0");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 3)).cloned()),
            Some(Value::F64(2.25)),
            "amount is DIVIDEND's flat column 3 (ex, announced, pay, amount, status)"
        );
    }

    /// An inserted row's own cells land at each column's DECLARED type,
    /// not a blanket `Value::F64` (task 3 review, fix round 1): `amt` is
    /// `F64`, `n` is `I64`, and a whole-number delta bumps both through
    /// the production `:bump` route, `set_row_cell`'s own door.
    #[gpui::test]
    fn an_inserted_rows_bump_lands_each_cells_declared_type(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::MIXED, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::mixed_snapshot(&[("M1", 1.0, 2)])),
        );

        h.dispatch(&mut vcx, "insert_below", None);
        vcx.run_until_parked();
        // `insert_below` opens the new row's first cell (`amt`) in insert
        // mode already; fill it, then move onto `n` and fill it too —
        // the same edit → commit route `a_commit_on_an_inserted_row_writes_its_own_cells`
        // uses.
        h.set_editor(&mut vcx, "1.5");
        h.dispatch(&mut vcx, "commit", None);
        h.tile.update(&mut vcx, |t, cx| t.cursor_to(1, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2");
        h.dispatch(&mut vcx, "commit", None);

        h.command(&mut vcx, "bump 2 row")
            .expect("a whole-number bump");
        let cells = h
            .tile
            .read_with(&vcx, |t, _| match t.draft().row_state("new-1") {
                Some(RowEdit::Inserted { cells, .. }) => cells.clone(),
                other => panic!("new-1 is still an inserted row, got {other:?}"),
            });
        assert_eq!(cells.get("amt"), Some(&Value::F64(3.5)));
        assert_eq!(cells.get("n"), Some(&Value::I64(4)));
    }

    /// The atomicity fix itself (task 3 review, fix round 1, controller
    /// ruling): a row bump across an inserted row's `F64` cell and `I64`
    /// cell, with a FRACTIONAL delta the `I64` cell refuses, must not
    /// land the `F64` cell first and then refuse — every cell's `bumped`
    /// result is computed before either write door (`Draft::bump`,
    /// `set_row_cell`) opens, so the refusal leaves BOTH cells exactly as
    /// they were.
    #[gpui::test]
    fn a_fractional_row_bump_on_a_mixed_inserted_row_writes_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::MIXED, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::mixed_snapshot(&[("M1", 1.0, 2)])),
        );

        h.dispatch(&mut vcx, "insert_below", None);
        vcx.run_until_parked();
        h.set_editor(&mut vcx, "1.5");
        h.dispatch(&mut vcx, "commit", None);
        h.tile.update(&mut vcx, |t, cx| t.cursor_to(1, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2");
        h.dispatch(&mut vcx, "commit", None);

        let err = h
            .command(&mut vcx, "bump 0.5 row")
            .expect_err("n cannot take a fractional delta");
        assert_eq!(err, "bump: n takes whole numbers");
        let cells = h
            .tile
            .read_with(&vcx, |t, _| match t.draft().row_state("new-1") {
                Some(RowEdit::Inserted { cells, .. }) => cells.clone(),
                other => panic!("new-1 is still an inserted row, got {other:?}"),
            });
        assert_eq!(
            cells.get("amt"),
            Some(&Value::F64(1.5)),
            "amt must NOT have been bumped ahead of n's refusal"
        );
        assert_eq!(cells.get("n"), Some(&Value::I64(2)), "n is untouched");
    }

    /// A session carrying an inserted row, a deleted row and a cell edit
    /// on the row below the insert, restored onto the schedule (spec
    /// §5.4): the first delivery splices the inserted row in, keeps the
    /// deleted row painted and marked, and lands the cell edit on D2 —
    /// which now sits one row lower than its document index — rather
    /// than on whatever row took its old index. The restore's rebase runs
    /// against the DOCUMENT's own grid: against the spliced model it would
    /// read the draft's own `new-1` as a row the document now carries and
    /// drop it as a conflict. A second inserted row chained on the first
    /// (`new-2 after new-1`) rides through that rebase with its anchor
    /// intact and paints directly under it.
    #[gpui::test]
    fn a_restored_draft_with_rows_splices_them_and_keeps_its_edits_in_place(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["D2", "amount", 0.75]]
[draft.rows.new-1]
after = "D1"
cells = {{ amount = 2.0 }}
[draft.rows.new-2]
after = "new-1"
cells = {{ amount = 3.0 }}
[draft.rows.D3]
deleted = true
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("a restored key requeries").tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "estimated"),
                ("D3", "2027-06-18", 0.9, "estimated"),
            ])),
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t
                .model()
                .rows
                .iter()
                .map(|r| r.label.to_string())
                .collect::<Vec<_>>()),
            ["D1", "new-1", "new-2", "D2", "D3"],
            "the chain paints in place after the resolving delivery"
        );
        assert_eq!(
            h.col_texts(&vcx, 1),
            ["1.2500", "2.0000", "3.0000", "0.7500", "0.9000"]
        );
        let states: Vec<Option<RowState>> = h
            .tile
            .read_with(&vcx, |t, _| (0..6).map(|r| t.row_state_at(r)).collect());
        assert_eq!(
            states,
            [
                Some(RowState::Document),
                Some(RowState::Inserted),
                Some(RowState::Inserted),
                Some(RowState::Document),
                Some(RowState::Deleted),
                None
            ]
        );
        assert_eq!(h.cell(&vcx, 3, 1), ("0.7500".to_string(), true));
        assert_eq!(h.cell(&vcx, 1, 0), ("·".to_string(), true));
        let (added, removed, edits, chained) = h.tile.read_with(&vcx, |t, _| {
            let d = t.draft();
            let chained = match d.row_state("new-2") {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            };
            (d.rows_added(), d.rows_removed(), d.edits.clone(), chained)
        });
        assert_eq!((added, removed), (2, 1));
        assert_eq!(
            chained.as_deref(),
            Some("new-1"),
            "the rebase kept the anchor on the surviving inserted row"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.notice().is_none()),
            "nothing was named dropped: {:?}",
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string))
        );
        assert_eq!(
            edits.keys().copied().collect::<Vec<_>>(),
            [(1, 1)],
            "the edit is keyed by D2's DOCUMENT row, not its painted one"
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));
        assert!(
            h.header_texts(&vcx)
                .contains(&"2 rows incomplete".to_string()),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// A commit on an inserted row's cell writes `RowEdit.cells` by column
    /// label, never `Draft::edits` by index (spec §5.1) — and paints on
    /// the same keystroke, patched; the incomplete chip follows. A row
    /// bump on it composes with what it painted, the same way.
    #[gpui::test]
    fn a_commit_on_an_inserted_row_writes_its_own_cells(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = []
[draft.rows.new-1]
after = "D1"
cells = {{ ex = {{ type = "date", value = "2027-01-15" }}, status = {{ type = "text", value = "declared" }} }}
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "estimated"),
            ])),
        );
        assert!(
            h.header_texts(&vcx)
                .contains(&"1 row incomplete".to_string())
        );
        h.tile.update(&mut vcx, |t, cx| t.cursor_to(1, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("·"));
        h.set_editor(&mut vcx, "2.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 1, 1), ("2.5000".to_string(), true));
        let (edits, cells) = h.tile.read_with(&vcx, |t, _| {
            let d = t.draft();
            let cells = match d.row_state("new-1") {
                Some(RowEdit::Inserted { cells, .. }) => cells.clone(),
                other => panic!("new-1 is still an inserted row, got {other:?}"),
            };
            (d.edits.clone(), cells)
        });
        assert!(edits.is_empty(), "no index-keyed edit for an inserted row");
        assert_eq!(cells.get("amount"), Some(&Value::F64(2.5)));
        assert!(
            !h.header_texts(&vcx)
                .contains(&"1 row incomplete".to_string()),
            "every required column is filled now: {:?}",
            h.header_texts(&vcx)
        );
        assert_eq!(
            h.cell(&vcx, 2, 1),
            ("0.5000".to_string(), false),
            "D2, below the insert, is untouched"
        );

        h.command(&mut vcx, "bump 1 row")
            .expect("a bump on the inserted row");
        assert_eq!(h.cell(&vcx, 1, 1), ("3.5000".to_string(), true));
        let (edits, amount) = h.tile.read_with(&vcx, |t, _| {
            let d = t.draft();
            let amount = match d.row_state("new-1") {
                Some(RowEdit::Inserted { cells, .. }) => cells.get("amount").cloned(),
                _ => None,
            };
            (d.edits.clone(), amount)
        });
        assert!(edits.is_empty());
        assert_eq!(amount, Some(Value::F64(3.5)));
    }

    /// A deleted row's cells refuse edits (spec §5.2): `i`, a choice
    /// step, and a row bump all answer "row is deleted — :revert restores
    /// it" and open nothing; a column bump skips the row rather than
    /// writing a value into a row that is going.
    #[gpui::test]
    fn a_deleted_rows_cells_refuse_edits(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = []
[draft.rows.D1]
deleted = true
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "estimated"),
            ])),
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.row_state_at(0)),
            Some(RowState::Deleted)
        );
        h.tile.update(&mut vcx, |t, cx| t.cursor_to(0, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(DELETED_REFUSED.to_string())
        );
        h.tile.update(&mut vcx, |t, cx| {
            t.notice = None;
            t.cursor_to(0, Some(2), cx);
        });
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(DELETED_REFUSED.to_string())
        );
        assert_eq!(
            h.cell(&vcx, 0, 2),
            ("declared".to_string(), false),
            "nothing stepped"
        );
        assert_eq!(
            h.command(&mut vcx, "bump 1 row"),
            Err(DELETED_REFUSED.to_string())
        );
        h.tile.update(&mut vcx, |t, cx| t.cursor_to(0, Some(1), cx));
        h.command(&mut vcx, "bump 1 col")
            .expect("a column bump skips the deleted row and moves D2");
        assert_eq!(h.col_texts(&vcx, 1), ["1.2500", "1.5000"]);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t
                .draft()
                .edits
                .keys()
                .copied()
                .collect::<Vec<_>>()),
            [(1, 1)]
        );
    }

    /// §4.4: a Text cell commits its text verbatim (trimmed); a Date cell
    /// opens the segmented date field IN the cell rather than a text
    /// input; both land in the draft as typed values and paint by the
    /// column's kind. The text half runs over [`SCHEDULE_REQUIRED_NOTE`]
    /// — [`SCHEDULE`]'s own `status` is a `Choice` column, which opens
    /// the typeahead popup (Task 5), never the text input.
    #[gpui::test]
    fn a_text_cell_commits_verbatim_and_a_date_cell_opens_the_date_field(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_spec(cx, &SCHEDULE_REQUIRED_NOTE, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_note_snapshot()));
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state().is_some()));
        h.set_editor(&mut vcx, "  paid ");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 0), ("paid".to_string(), true));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 0)).cloned()),
            Some(Value::Utf8("paid".into()))
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.notice().is_none()),
            "a clean commit leaves no notice"
        );

        // ex date is column 0 of the schedule: `i` opens the date field,
        // not a text input, seeded with the painted date and — the
        // strip's own rule — on the day segment.
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state().is_none()));
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-12-18"));
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the field's own handle holds the keyboard"
        );
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-date-{TILE}").into_boxed_str()
            ))
            .is_some(),
            "the field is painted in the cell"
        );
        // One day later, then commit — through the field's OWN listener,
        // the door the strip's `enter` takes too.
        type_keys(&mut vcx, "up enter");
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 0), ("2026-12-19".to_string(), true));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 0)).cloned()),
            Some(Value::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 12, 19).unwrap()
            ))
        );
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "the field gave the keyboard up before it was dropped"
        );
        // The other cells are untouched: a commit patches one cell.
        assert_eq!(h.cell(&vcx, 0, 1), ("1.2500".to_string(), false));
        assert_eq!(h.cell(&vcx, 1, 0), ("2027-03-19".to_string(), false));
    }

    /// The fragment's own `commit` verb reaches a date CELL's field exactly
    /// as the field's `enter` does — with a pending digit completed first
    /// (the strip's review I-1 rule) — and `cancel` blurs then drops it.
    #[gpui::test]
    fn a_date_cell_commits_and_cancels_through_the_fragments_verbs(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        type_keys(&mut vcx, "2");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 0), ("2026-12-02".to_string(), true));

        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
        h.dispatch(&mut vcx, "insert_up", Some(1));
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("2026-12-03"),
            "the neutral arrow pair steps a date cell's field"
        );
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 0), ("2026-12-02".to_string(), true));
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()));
    }

    /// An empty commit on a required Text cell is refused with the editor
    /// open, and a non-required one is allowed and paints blank.
    #[gpui::test]
    fn an_empty_required_text_commit_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &SCHEDULE_REQUIRED_NOTE, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_note_snapshot()));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "   ");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("a value is required".to_string())
        );
        assert_eq!(h.mode(&vcx), "insert", "the editor stays open");
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));

        let (h, mut vcx) = open_spec(cx, &SCHEDULE_OPTIONAL_NOTE, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_note_snapshot()));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 0), (String::new(), true));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 0)).cloned()),
            Some(Value::Utf8(String::new()))
        );
    }

    /// A one-column flat panel whose value is REQUIRED free text — the
    /// plain `Text` cell every text-editor test runs over, since
    /// [`SCHEDULE`]'s own `status` is a `Choice` (Task 5's popup, not the
    /// text input). Otherwise [`SCHEDULE_OPTIONAL_NOTE`]'s twin.
    const SCHEDULE_REQUIRED_NOTE: PanelSpec = PanelSpec {
        kind: "sched_note_req",
        title: "Dividends (note)",
        dataset: "div_schedule_note",
        document: "div_schedule_note",
        rows: RowAxis {
            column: "dividend_id",
            identity: RowIdentity::Minted,
            label: RowLabel::Shown,
        },
        columns: Columns::Values(&[ValueColumn {
            column: "note",
            label: "note",
            ty: ColumnType::Utf8,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: true,
        }]),
        header: &[],
        slice_values: &[],
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: &[],
    };

    /// A one-column flat panel whose value is optional free text — what
    /// tells "empty is refused" (a required column) from "empty is a
    /// value" (an optional one).
    const SCHEDULE_OPTIONAL_NOTE: PanelSpec = PanelSpec {
        kind: "sched_note",
        title: "Dividends (note)",
        dataset: "div_schedule_note",
        document: "div_schedule_note",
        rows: RowAxis {
            column: "dividend_id",
            identity: RowIdentity::Minted,
            label: RowLabel::Shown,
        },
        columns: Columns::Values(&[ValueColumn {
            column: "note",
            label: "note",
            ty: ColumnType::Utf8,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: false,
        }]),
        header: &[],
        slice_values: &[],
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: &[],
    };

    fn schedule_note_snapshot() -> Snapshot {
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into())]),
                ),
                (
                    meta("dividend_id", Attribution::Additive),
                    TestColumn::Dict(vec![Some("D1".into())]),
                ),
                (
                    meta("note", Attribution::DeterminedNonAdditive),
                    TestColumn::Dict(vec![Some("special".into())]),
                ),
            ],
            0,
            provenance(BASE),
        )
    }

    /// §4.5: a cell commit patches the ONE cell in the model the tile
    /// already holds rather than building a new model — the `Rc` the
    /// delegate paints from is the same allocation before and after, so
    /// a 10,000-row schedule pays for one cell per keystroke, not every
    /// row (the flat build is the per-commit cost `docs/perf.md` records
    /// at the edge of the 8 ms budget).
    #[gpui::test]
    fn a_cell_commit_patches_the_model_in_place(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        let before = h
            .tile
            .read_with(&vcx, |t, _| t.model() as *const MatrixModel);
        h.dispatch(&mut vcx, "right", Some(1));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2.5");
        h.dispatch(&mut vcx, "commit", None);
        let after = h
            .tile
            .read_with(&vcx, |t, _| t.model() as *const MatrixModel);
        assert_eq!(before, after, "the model was patched, not replaced");
        assert_eq!(h.cell(&vcx, 0, 1), ("2.5000".to_string(), true));
        // And the delegate paints from the same, patched model.
        let painted = h.tile.read_with(&vcx, |t, cx| {
            let d = t.table().read(cx).delegate();
            (
                Rc::as_ptr(&d.model),
                d.model.rows[0].cells[1].text.to_string(),
            )
        });
        assert_eq!(painted, (after, "2.5000".to_string()));
    }

    /// The delegate mirrors a date CELL's field exactly as it mirrors the
    /// text editor: the cell, and the field's own paint and focus handle,
    /// so `render_td` can paint the segments in the cell.
    #[gpui::test]
    fn the_delegate_mirrors_a_date_cells_field(cx: &mut gpui::TestAppContext) {
        use crate::delegate::DelegateEditorPaint;
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        let focus = h.tile.read_with(&vcx, |t, _| t.date_field_focus()).unwrap();
        let mirrored = h.tile.read_with(&vcx, |t, cx| {
            let d = t.table().read(cx).delegate();
            d.editor.as_ref().map(|e| {
                (
                    e.row,
                    e.col,
                    match &e.paint {
                        DelegateEditorPaint::Date { paint, focus } => {
                            Some((paint.segments.clone(), focus.clone()))
                        }
                        DelegateEditorPaint::Text(_) => None,
                    },
                )
            })
        });
        let (row, col, date) = mirrored.expect("the editor is mirrored");
        assert_eq!((row, col), (1, Some(0)));
        let (segments, mirrored_focus) = date.expect("as a date field");
        assert_eq!(
            segments
                .iter()
                .map(|s| s.text.to_string())
                .collect::<Vec<_>>(),
            vec!["2027", "03", "19"]
        );
        assert_eq!(mirrored_focus, focus);
        // A keystroke re-prepares the mirror's segments.
        draw(&mut vcx);
        type_keys(&mut vcx, "up");
        let segments = h.tile.read_with(&vcx, |t, cx| {
            match &t.table().read(cx).delegate().editor.as_ref().unwrap().paint {
                DelegateEditorPaint::Date { paint, .. } => paint
                    .segments
                    .iter()
                    .map(|s| s.text.to_string())
                    .collect::<Vec<_>>(),
                DelegateEditorPaint::Text(_) => unreachable!(),
            }
        });
        assert_eq!(segments, vec!["2027", "03", "20"]);
        h.dispatch(&mut vcx, "cancel", None);
        assert!(
            h.tile
                .read_with(&vcx, |t, cx| t.table().read(cx).delegate().editor.is_none()),
            "cancel clears the mirror"
        );
    }

    /// `nudge`'s own refusal (spec §4.3, `declared_type`'s door): a
    /// `Text` cell's editor is the plain text `Input`, so `insert_up` on
    /// a note is reachable and must refuse rather than step, leaving the
    /// typed text untouched — there is no unit to step a word by. Over
    /// [`SCHEDULE_REQUIRED_NOTE`], since `status` is a `Choice` whose
    /// `insert_up` moves the popup's highlight instead (Task 5).
    #[gpui::test]
    fn a_flat_panels_nudge_on_a_non_numeric_cell_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &SCHEDULE_REQUIRED_NOTE, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_note_snapshot()));

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("special"));
        h.dispatch(&mut vcx, "insert_up", None);

        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("not a numeric cell".to_string())
        );
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("special"),
            "the typed text is untouched"
        );
        assert_eq!(h.mode(&vcx), "insert");
    }

    /// The flat `Columns::Values` success arm (spec §4.3): `amount` is
    /// `F64`, so a commit on it parses and lands as `Value::F64`, and the
    /// cell paints back through its OWN format (four places).
    #[gpui::test]
    fn a_flat_panels_commit_on_amount_parses_as_f64(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(1));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2.5");
        h.dispatch(&mut vcx, "commit", None);

        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 1)).cloned()),
            Some(Value::F64(2.5))
        );
        assert_eq!(h.cell(&vcx, 0, 1), ("2.5000".to_string(), true));
    }

    /// `edit` + `insert_up` on `amount` exercises `nudge`'s own success
    /// arm for a flat panel: the text steps by one unit of the COLUMN's
    /// own precision (four places), not the panel's default.
    #[gpui::test]
    fn a_flat_panels_nudge_on_amount_steps_by_its_precision(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(1));

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("1.2500"));
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("1.2501"));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a nudge commits nothing"
        );
    }

    // ---- the Choice cell (dividend spec §4.4, Task 5) ----------------

    /// §4.4: `space`/`shift+space` (`step`/`step_back`) step a choice
    /// cell in place through the options in declared order, wrapping at
    /// both ends and composing on the draft's own current value; on any
    /// other kind of cell they say so and write nothing.
    #[gpui::test]
    fn space_steps_a_choice_cell_and_refuses_elsewhere(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2)); // status = declared
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(h.cell(&vcx, 0, 2), ("paid".to_string(), true));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 2)).cloned()),
            Some(Value::Utf8("paid".into())),
            "a step lands in the draft as the option's own text"
        );
        h.dispatch(&mut vcx, "step_back", None);
        h.dispatch(&mut vcx, "step_back", None);
        assert_eq!(
            h.cell(&vcx, 0, 2).0,
            "estimated",
            "two steps back from paid, composing on the draft's value"
        );
        h.dispatch(&mut vcx, "step_back", None);
        assert_eq!(
            h.cell(&vcx, 0, 2).0,
            "cancelled",
            "a step back from the first option wraps to the last"
        );
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.cell(&vcx, 0, 2).0,
            "estimated",
            "a step from the last option wraps to the first"
        );
        assert_eq!(h.mode(&vcx), "normal", "a step opens nothing");

        h.dispatch(&mut vcx, "left", None); // amount
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("not a choice cell".to_string())
        );
        assert_eq!(h.cell(&vcx, 0, 1), ("1.2500".to_string(), false));
    }

    /// A NULL choice cell has no current option: a step forward lands on
    /// the FIRST option and a step back on the last, rather than treating
    /// the hole as option 0 and skipping past it.
    #[gpui::test]
    fn a_step_on_a_null_choice_cell_lands_on_the_first_or_last_option(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot_with_null_status(),
        );
        h.dispatch(&mut vcx, "right", Some(2));
        assert_eq!(h.cell(&vcx, 0, 2), (String::new(), false));
        h.dispatch(&mut vcx, "step_back", None);
        assert_eq!(h.cell(&vcx, 0, 2), ("cancelled".to_string(), true));
        h.dispatch(&mut vcx, "revert", None);
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(h.cell(&vcx, 0, 2), ("estimated".to_string(), true));
    }

    /// A step is refused with the same gates `:bump` uses: no document,
    /// the cursor in the strip, and a draft that is `Behind`.
    #[gpui::test]
    fn a_step_is_refused_without_a_document_in_the_strip_and_while_behind(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_flat(cx);
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(NO_DOCUMENT.to_string())
        );

        // CVI has a strip; the schedule does not.
        let (h, mut vcx) = open(cx);
        let tag = h.with_document_tagged(&mut vcx);
        h.dispatch(&mut vcx, "up", None); // Attr(0)
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("step needs a grid cell — the cursor is in the header".to_string())
        );
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(BEHIND_REFUSED.to_string())
        );
    }

    /// `i` on a choice cell opens the typeahead popup with its field
    /// focused (insert mode, no text editor); `enter` picks the lit
    /// option — re-ranked from the field's live text — and commits it
    /// through the cell door; the field gives the keyboard up before it
    /// is dropped.
    #[gpui::test]
    fn i_on_a_choice_cell_opens_a_typeahead_and_enter_picks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.editor_state().is_none()),
            "a choice cell opens no text editor"
        );
        assert_eq!(h.mode(&vcx), "insert");
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the popup's field holds the keyboard"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.choice_highlighted()),
            Some("declared".into()),
            "the popup opens on the cell's current value"
        );
        draw(&mut vcx);
        vcx.simulate_input("can");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.choice_highlighted()),
            Some("cancelled".into()),
            "typing narrows the list live"
        );
        assert_eq!(
            h.delegate_choice_rows(&vcx),
            Some((vec!["cancelled".to_string()], 0)),
            "and the delegate's own paint follows on the same keystroke"
        );
        h.dispatch(&mut vcx, "commit", None);
        assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(h.cell(&vcx, 0, 2), ("cancelled".to_string(), true));
        assert_eq!(h.mode(&vcx), "normal");
        assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "blur, then drop"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.notice().is_none()),
            "a clean pick leaves no notice"
        );
    }

    /// `commit` re-feeds the field's LIVE text before picking — a value
    /// seeded through `set_value` (which emits no `Change`) is still what
    /// decides the pick; a text matching no option is refused with the
    /// popup left open; the neutral arrow pair moves the highlight; and
    /// `escape` closes it with nothing written.
    #[gpui::test]
    fn the_choice_popup_ranks_from_the_live_text_and_escape_writes_nothing(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));
        h.dispatch(&mut vcx, "edit", None);
        h.set_choice_text(&mut vcx, "zzz");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("no option matches".to_string())
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.choice_popup_open()),
            "the popup stays open for a retype"
        );
        h.set_choice_text(&mut vcx, "pa");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 0, 2), ("paid".to_string(), true));

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.choice_highlighted()),
            Some("paid".into())
        );
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.choice_highlighted()),
            Some("cancelled".into()),
            "the neutral arrow pair moves the highlight"
        );
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.choice_highlighted()),
            Some("estimated".into()),
            "a bare step wraps (§20.5)"
        );
        h.dispatch(&mut vcx, "cancel", None);
        assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, 2), ("paid".to_string(), true));
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()));
    }

    /// The popup is painted anchored under the cell it edits, a row click
    /// picks that row, and a click elsewhere closes it with nothing
    /// written — the picker's own mouse rules.
    #[gpui::test]
    fn the_choice_popup_hangs_under_its_cell_and_a_row_click_picks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(2));
        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        let cell = vcx
            .debug_bounds("marketdata-cell-0-3")
            .expect("the status cell is painted");
        let popup = vcx
            .debug_bounds(Box::leak(
                format!("marketdata-choice-{TILE}").into_boxed_str(),
            ))
            .expect("the popup is painted");
        assert!(
            popup.origin.y >= cell.bottom() - gpui::px(1.),
            "the popup hangs under the cell: popup {popup:?}, cell {cell:?}"
        );
        assert!(
            (popup.origin.x - cell.origin.x).abs() <= gpui::px(1.),
            "and starts at its left edge: popup {popup:?}, cell {cell:?}"
        );

        // Row 2 of the declared order under an empty query is `paid`.
        let row = centre_of(&mut vcx, &format!("marketdata-choice-row-{TILE}-2"));
        click_at(&mut vcx, row, 1);
        assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(h.cell(&vcx, 0, 2), ("paid".to_string(), true));
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.selection(&vcx),
            (Some(0), Some(3)),
            "a row click picks and never also selects the grid cell beneath the popup"
        );

        // "Elsewhere" is the HEADER, on purpose: a click on another grid
        // cell would also run the table's own `SelectCell` → `sync_cursor`,
        // which re-mirrors on its own and so could not tell whether the
        // popup's own close did. The header runs no dispatch tail at all,
        // so the delegate reading `None` here is `close_popup_with_window`'s
        // re-mirror and nothing else's.
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        let elsewhere = centre_of(&mut vcx, &format!("marketdata-header-{TILE}"));
        click_at(&mut vcx, elsewhere, 1);
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.choice_popup_open()),
            "a click elsewhere closes the popup"
        );
        assert_eq!(
            h.delegate_choice_rows(&vcx),
            None,
            "the mouse close re-mirrors: the delegate paints no popup"
        );
        assert_eq!(h.selection(&vcx), (Some(0), Some(3)), "the cursor stayed");
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-choice-{TILE}").into_boxed_str()
            ))
            .is_none(),
            "nothing hangs under the cell any more"
        );
        assert_eq!(
            h.cell(&vcx, 0, 2),
            ("paid".to_string(), true),
            "nothing written"
        );
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// A double-click on a choice cell opens the popup exactly as `i`
    /// does (header spec §8.8.6 applies to every kind), and `i` has the
    /// same refusal gate every editor has: `Behind` opens nothing.
    #[gpui::test]
    fn a_double_click_opens_the_choice_popup_and_behind_refuses_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        let at = centre_of(&mut vcx, "marketdata-cell-0-3");
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert!(h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(h.mode(&vcx), "insert");
        h.dispatch(&mut vcx, "cancel", None);

        // Behind: a step, then a different generation of the same rows.
        let rows = [("D1", "2026-12-18", 1.25, "declared")];
        let (h, mut vcx) = open_flat(cx);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot_at(&rows, BASE)),
        );
        h.dispatch(&mut vcx, "right", Some(2));
        h.dispatch(&mut vcx, "step", None);
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot_at(&rows, NEWER)),
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        h.dispatch(&mut vcx, "edit", None);
        assert!(!h.tile.read_with(&vcx, |t, _| t.choice_popup_open()));
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(BEHIND_REFUSED.to_string())
        );
    }

    /// A one-column flat panel whose value is `I64` — [`SCHEDULE`] has no
    /// integer column of its own, and this exists only to pin that a flat
    /// cell's declared type comes from the column's OWN `ValueColumn::ty`
    /// rather than being guessed from what the text happens to parse as:
    /// `"3"` is a valid `F64` too, so only checking the RESULT type
    /// (`Value::I64`, not `Value::F64`) proves which one was used.
    const SCHEDULE_I64: PanelSpec = PanelSpec {
        kind: "sched_i64",
        title: "Dividends (i64)",
        dataset: "div_schedule_i64",
        document: "div_schedule_i64",
        rows: RowAxis {
            column: "dividend_id",
            identity: RowIdentity::Minted,
            label: RowLabel::Shown,
        },
        columns: Columns::Values(&[ValueColumn {
            column: "units",
            label: "units",
            ty: ColumnType::I64,
            format: ColumnFormat::MEASURE,
            choices: None,
            required: true,
        }]),
        header: &[],
        slice_values: &[],
        value_type: ColumnType::F64,
        format: ColumnFormat::MEASURE,
        actions: &[],
    };

    fn schedule_i64_snapshot() -> Snapshot {
        Snapshot::for_tests_with_provenance(
            vec![
                (
                    meta("underlying_ref", Attribution::Additive),
                    TestColumn::Dict(vec![Some("SPX.Z".into())]),
                ),
                (
                    meta("dividend_id", Attribution::Additive),
                    TestColumn::Dict(vec![Some("D1".into())]),
                ),
                (
                    meta("units", Attribution::DeterminedNonAdditive),
                    TestColumn::I64(vec![1]),
                ),
            ],
            0,
            provenance(BASE),
        )
    }

    /// The flat `Columns::Values` success arm, at an `I64` column: a
    /// commit of `"3"` lands as `Value::I64(3)`, not `Value::F64(3.0)` —
    /// the declared-type door, not a parse-and-hope one.
    #[gpui::test]
    fn a_flat_panels_commit_on_an_i64_column_parses_by_its_declared_type(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_spec(cx, &SCHEDULE_I64, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_i64_snapshot()));

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "3");
        h.dispatch(&mut vcx, "commit", None);

        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().edits.get(&(0, 0)).cloned()),
            Some(Value::I64(3))
        );
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
            vec!["4500.00", "0.1800", "-1.0000", "0.1000", "0.2000", "0.3000"],
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

    /// Controller ruling 2026-09-14: `:revert` while `Behind` is the whole
    /// story — `Clean` means "on the live document" — so the newer
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
        assert_eq!(cell.text.to_string(), "4500.00", "the document's own value");
        assert!(!cell.edited);
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.header_dirty())
                && !chips.iter().any(|c| c.contains("update")),
            "nothing left to explain a generation that is no longer retained: {chips:?}"
        );
        // Rebase is now refused again — there is no draft to move.
        assert_eq!(
            h.command(&mut vcx, "rebase"),
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
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 4, col: 0 }
        );
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
        assert_eq!(cell.text.to_string(), "0.50", "the edit is still on screen");
        assert!(cell.edited);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let local = clock.hm(chrono::DateTime::parse_from_rfc3339(NEWER)
            .unwrap()
            .to_utc());
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c == &format!("update {local}")),
            "{chips:?}"
        );
    }

    /// A SECOND newer generation arriving on top of an already-`Behind`
    /// draft moves the header's own `newer` marker forward — but the
    /// `base_snapshot` a trader is still looking at must not move: they
    /// have not chosen `:rebase`/`:revert` for the FIRST newer document
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
        assert_eq!(cell.text.to_string(), "0.50", "the edit is untouched");
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let local = clock.hm(chrono::DateTime::parse_from_rfc3339(NEWEST)
            .unwrap()
            .to_utc());
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
            "0.70",
            "the kept edit, at its new index — a `fwd` cell, resolved by (term, \"fwd\")"
        );
        assert!(cell.edited);

        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            chips.iter().any(|c| c
                == "dropped 2 edits whose rows or columns the new document lacks: \
2026-10-16/fwd, 2026-10-16/atm"),
            "{chips:?}"
        );
    }

    /// The rebase guard (spec §2, amendment 4), through `:rebase` on a
    /// real dividend panel: an edit keyed by a same-date ORDINAL
    /// (`2026-09-18#2`) is refused when the newer document's group grew
    /// from two rows to three, and the notice names the row and the size
    /// change; the edit is gone rather than landing on whichever row
    /// `2026-09-18#2` now happens to be.
    #[gpui::test]
    fn rebase_refuses_a_same_day_group_edit_through_the_command_line(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_spec(cx, &DIVIDEND, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::dividend_snapshot(&[
                (
                    "2026-09-18",
                    "2026-09-18",
                    "2026-08-01",
                    "2026-10-01",
                    1.0,
                    "declared",
                ),
                (
                    "2026-09-18#2",
                    "2026-09-18",
                    "2026-08-01",
                    "2026-10-01",
                    2.0,
                    "declared",
                ),
            ])),
        );

        // Row 1 ("2026-09-18#2"), the "amount" column (index 3).
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.0");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 1);

        // A newer generation: the same date now carries a THIRD row, so
        // the group's ordinals have shifted.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::dividend_snapshot_at(
                &[
                    (
                        "2026-09-18",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        1.0,
                        "declared",
                    ),
                    (
                        "2026-09-18#2",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        2.0,
                        "declared",
                    ),
                    (
                        "2026-09-18#3",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        3.0,
                        "declared",
                    ),
                ],
                NEWER,
            )),
        );

        h.command(&mut vcx, "rebase")
            .expect("behind: rebase applies");

        let (len, chips) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().len(), t.header_texts()));
        assert_eq!(len, 0, "the same-day group edit did not survive the rebase");
        assert!(
            chips
                .iter()
                .any(|c| c.contains("2026-09-18#2") && c.contains("2 → 3")),
            "{chips:?}"
        );
    }

    /// The M-1 path (review finding 1): a draft restored from the session
    /// carries `groups` captured against its TRUE base, and that base
    /// generation is never delivered THIS session at all — the very first
    /// delivery is a newer one, so `base_snapshot` is never retained and
    /// `painted_snapshot()` falls back to that newer snapshot. A capture
    /// site that trusted the fallback would silently overwrite the
    /// restored `groups` with the newer document's own sizes (a group of
    /// two reading as "was two" instead of the true base's two) and the
    /// guard would never fire. `capture_groups_if_base` must instead
    /// leave the session's `groups` untouched, so `:rebase` still refuses
    /// the shifted edit.
    #[gpui::test]
    fn rebase_still_refuses_a_same_day_group_when_the_base_was_never_delivered(
        cx: &mut gpui::TestAppContext,
    ) {
        let restored: toml::Table = format!(
            r#"
underlying = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = [["2026-09-18#2", "amount", 9.0]]
[draft.groups]
"2026-09-18" = 2
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_spec(cx, &DIVIDEND, Some(restored));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        // The FIRST delivery this tile has ever seen — `BASE` never
        // arrives, so `base_snapshot` has nothing to retain and this is
        // exactly the M-1 path. The same-date group has grown from the
        // session's captured two rows to three.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::dividend_snapshot_at(
                &[
                    (
                        "2026-09-18",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        1.0,
                        "declared",
                    ),
                    (
                        "2026-09-18#2",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        2.0,
                        "declared",
                    ),
                    (
                        "2026-09-18#3",
                        "2026-09-18",
                        "2026-08-01",
                        "2026-10-01",
                        3.0,
                        "declared",
                    ),
                ],
                NEWER,
            )),
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| matches!(
                t.draft().state,
                DraftState::Behind { .. }
            )),
            "the base was never delivered: this must be the M-1 path"
        );

        h.command(&mut vcx, "rebase")
            .expect("behind: rebase applies");

        let (len, chips) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().len(), t.header_texts()));
        assert_eq!(
            len, 0,
            "the guard must still fire even though the painted snapshot \
             was never this draft's base"
        );
        assert!(
            chips
                .iter()
                .any(|c| c.contains("2026-09-18#2") && c.contains("2 → 3")),
            "{chips:?}"
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

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            h.command(&mut vcx, "rebase"),
            Err("nothing to rebase — the draft is on the live document".to_string()),
            "edits present, but still on the document they were made against"
        );
    }

    #[gpui::test]
    fn completions_offer_rebase_only_while_behind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        let before = h.tile.read_with(&vcx, |t, cx| t.completions("", 0, cx));
        assert!(!before.contains(&"rebase".to_string()));

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
            Some("the draft is behind — :rebase or :revert first".to_string())
        );
        assert_eq!(
            h.command(&mut vcx, "bump 1"),
            Err("the draft is behind — :rebase or :revert first".to_string())
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().len()),
            1,
            "no second edit was written"
        );
    }

    /// `:revert` is the escape from the BEHIND-refusal notice `i`/`edit`
    /// leaves behind — it must clear that specific notice on success, not
    /// just the draft, or the panel reads Behind's own refusal after the
    /// draft is already Clean and nothing is left to explain it.
    #[gpui::test]
    fn revert_clears_the_behind_refusal_notice_it_resolves(cx: &mut gpui::TestAppContext) {
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
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("the draft is behind — :rebase or :revert first".to_string())
        );
        assert_eq!(h.mode(&vcx), "normal");

        h.command(&mut vcx, "revert").expect("an edit to clear");

        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            None
        );
        assert_eq!(h.mode(&vcx), "normal");
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
            .read_with(&vcx, |t, _| t.model().rows[1].cells[SLICE + 1].clone());
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
            .read_with(&vcx, |t, _| t.model().rows[1].cells[SLICE + 1].clone());
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

    // ---- Task 5: the strip's own cursor, editing, and `:set` ---------

    /// `k` from the top row enters the strip at the nearest attribute —
    /// the grid paints no selection behind it — and `i`/`enter` edits the
    /// attribute exactly as a cell would: parsed, written to the draft,
    /// and painted with the dirty marker. `j` back out returns to the
    /// grid at column 1, the column the cursor left from.
    #[gpui::test]
    fn k_from_the_top_row_enters_the_strip_and_i_edits_the_attribute(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "up", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert_eq!(
            h.selection(&vcx),
            (None, None),
            "the grid paints no highlighted row behind the strip"
        );
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
        h.set_editor(&mut vcx, "4520");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        let (attrs, dirty) = h
            .tile
            .read_with(&vcx, |t, _| (t.model().header.clone(), t.header_dirty()));
        assert_eq!((attrs[1].text.as_ref(), attrs[1].edited), ("4520", true));
        assert!(dirty);
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .contains(&"spot 4520".to_string())
        );
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 1 }
        );
    }

    /// A refused parse stays in insert mode with the typed text intact —
    /// the cell rule, spec §5.2 — and writes nothing to the draft. On
    /// `spot_ref` (F64): a `Date` attribute opens the segmented field
    /// since 2026-09-19, which has nothing to refuse.
    #[gpui::test]
    fn a_bad_number_stays_in_insert_mode_with_the_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "up", None); // Attr(1) = spot_ref
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "abc");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("'abc' is not a number".into())
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    }

    // ---- nudging (2026-09-17) -----------------------------------------

    /// `up` in an open cell editor steps the text by one unit of the
    /// column's painted precision — a `param` at four places by `0.0001`
    /// — `shift+up` (`insert_up_big`) by ten units, and the two compose
    /// in the editor's text; `enter` then commits the nudged value, so
    /// the arrows write nothing of their own.
    #[gpui::test]
    fn up_steps_a_cell_one_unit_of_its_precision_and_shift_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(3)); // the first node: 0.1000
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.1000"));

        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.1001"));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a nudge commits nothing"
        );
        h.dispatch(&mut vcx, "insert_up_big", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.1011"));
        assert_eq!(h.mode(&vcx), "insert");

        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.cell(&vcx, 0, SLICE), ("0.1011".to_string(), true));
    }

    /// A slice column carries its own precision: `fwd` paints two places,
    /// so `down` steps it by `0.01`, not by the panel's `0.0001`.
    #[gpui::test]
    fn a_slice_column_nudges_at_its_own_precision(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None); // (0, 0) is `fwd`: 4500.00
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("4500.00"));
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("4499.99"));
    }

    /// A `Date` attribute's field steps whole days on the shell-dispatched
    /// `insert_*` verbs too — `shift+down` is ten of them — the same door
    /// the field's own listener takes, so the two cannot disagree; and an
    /// `F64` attribute steps at the places its text paints (`5000` by
    /// one).
    #[gpui::test]
    fn an_attribute_nudges_by_days_or_by_its_painted_places(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "up", None); // Attr(0) = anchor_date
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-12"));
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-02"));
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-03"));
        h.dispatch(&mut vcx, "cancel", None);

        h.dispatch(&mut vcx, "right", None); // Attr(1) = spot_ref
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5001"));
    }

    /// Text the arrows cannot step is left alone with a notice naming it,
    /// and `escape` after a nudge leaves the draft empty — the editor's
    /// text is the only thing a nudge ever touched.
    #[gpui::test]
    fn a_nudge_refuses_unparseable_text_and_escape_discards_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", Some(3));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "abc");
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("abc"));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("'abc' is not a number".into())
        );
        assert_eq!(h.mode(&vcx), "insert");

        h.set_editor(&mut vcx, "0.1000");
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("0.1001"));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            None,
            "a nudge that lands clears the refusal"
        );
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
    }

    /// With the picker open the same pair moves its highlight (the picker
    /// has no number to nudge and no "big": `_big` is one step too), and
    /// with neither input open the verbs are not handled at all.
    #[gpui::test]
    fn the_insert_pair_moves_the_picker_highlight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.dispatch(&mut vcx, "insert_down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("BBB.Z".into())
        );
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".into()),
            "big is one step in a list"
        );
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("BBB.Z".into())
        );
        assert_eq!(h.mode(&vcx), "insert", "the picker is still open");
        h.dispatch(&mut vcx, "cancel", None);

        let handled = vcx.update(|window, cx| {
            h.tile.update(cx, |t, cx| {
                t.dispatch(&ActionId("marketdata::insert_up".into()), None, window, cx)
            })
        });
        assert!(!handled, "nothing open: not handled");
    }

    /// `:set` writes the same draft `i`/`enter` would, without moving the
    /// cursor into the strip at all; an unknown attribute names the real
    /// vocabulary; `:revert` clears the attribute edit and the dirty dot
    /// along with any cell edits.
    #[gpui::test]
    fn set_writes_an_attribute_and_revert_clears_it_and_the_dot(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));
        assert!(
            h.command(&mut vcx, "set nope 1")
                .unwrap_err()
                .starts_with("no attribute 'nope'")
        );
        h.command(&mut vcx, "revert").unwrap();
        assert!(!h.tile.read_with(&vcx, |t, _| t.header_dirty()));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.model().header[1].text.to_string()),
            "5000"
        );
    }

    /// `:set <attr>` with no value answers the current value as a notice
    /// (spec §5.2) — an `Err`, since nothing was written.
    #[gpui::test]
    fn set_with_no_value_answers_the_current_value_as_a_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "set spot_ref"),
            Err("spot_ref = 5000".to_string())
        );
    }

    /// A single click on an attribute value moves the cursor to `Attr(i)`
    /// and opens nothing (spec §5.1) — the same rule a grid cell's single
    /// click keeps.
    #[gpui::test]
    fn a_click_on_an_attribute_moves_the_cursor_and_opens_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let at = centre_of(&mut vcx, &format!("marketdata-attr-{TILE}-1"));
        click_at(&mut vcx, at, 1);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.editor_value(&vcx), None, "the click opened no editor");
    }

    /// A double-click on an attribute value opens its editor (user ruling
    /// 2026-09-17), seeded with the painted value, in the strip — and
    /// the press's own listener does not swallow the event, so the
    /// host's tile-level mouse-down (the shell's focus re-arm stand-in)
    /// still saw both presses. After a draw the editor's input holds
    /// window focus.
    #[gpui::test]
    fn a_double_click_on_an_attribute_opens_its_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let before = h.host_clicks();
        let at = centre_of(&mut vcx, &format!("marketdata-attr-{TILE}-1"));
        click_at(&mut vcx, at, 1);
        click_at(&mut vcx, at, 2);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
        assert_eq!(
            h.host_clicks(),
            before + 2,
            "neither press was swallowed on its way to the host"
        );
        draw(&mut vcx);
        let input = h.tile.read_with(&vcx, |t, _| t.editor_state()).unwrap();
        assert!(
            vcx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
            "the attribute editor holds the keyboard after the next frame"
        );
        // And it is the real editor: `enter` commits it into the draft.
        h.set_editor(&mut vcx, "4520");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().attrs.len()), 1);
    }

    /// An attribute edit is unsent work exactly as a cell edit is: a
    /// newer generation under it goes `Behind`, and `:rebase` carries the
    /// attribute edit forward (by column name — spec §5.3) and marks the
    /// header attribute as edited on the newer document.
    #[gpui::test]
    fn an_attribute_edit_goes_behind_and_rebase_keeps_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        let tag = h.tile.read_with(&vcx, |t, _| t.tag);
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .iter()
                .any(|t| t.starts_with("update "))
        );
        h.command(&mut vcx, "rebase").unwrap();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().attrs.len()), 1);
        assert!(h.tile.read_with(&vcx, |t, _| t.model().header[1].edited));
    }

    /// `y`/`yy` in the strip yank the attribute's own value, and its
    /// label plus value tab-separated — the same shape a grid row's `yy`
    /// yanks. `yc` has no column to yank and answers with a notice
    /// instead of silently doing nothing.
    #[gpui::test]
    fn yank_in_the_strip_reads_the_attribute_and_yc_is_inert(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "up", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(0));
        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some("2026-09-12"));
        h.dispatch(&mut vcx, "yank_row", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some("anchor\t2026-09-12"));
        h.dispatch(&mut vcx, "yank_col", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("nothing to yank in a column here".to_string())
        );
    }

    // ---- the action list (Task 6, spec §6) ----------------------------

    #[gpui::test]
    fn dot_opens_the_menu_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        // `centre_of` panics if the selector is not painted — that is
        // this test's own "the popup is painted" assertion.
        centre_of(&mut vcx, &format!("marketdata-menu-{TILE}"));
        h.dispatch(&mut vcx, "menu_close", None);
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn enter_on_a_greyed_row_notices_and_keeps_the_menu(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "menu_down", None); // Upload (greyed: not built yet)
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.mode(&vcx), "menu");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("not built yet".into())
        );
    }

    #[gpui::test]
    fn an_unrelated_action_closes_the_menu_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
    }

    #[gpui::test]
    fn a_menu_row_click_dispatches_and_a_click_outside_closes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 1").unwrap();
        h.dispatch(&mut vcx, "menu", None);
        let revert = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-2")); // Revert edits on a dirty, not-behind draft
        click_at(&mut vcx, revert, 1);
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert_eq!(h.mode(&vcx), "normal");
        h.dispatch(&mut vcx, "menu", None);
        let cell = centre_of(&mut vcx, "marketdata-cell-1-1");
        click_at(&mut vcx, cell, 1);
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn the_menu_button_toggles_the_menu(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let button = centre_of(&mut vcx, &format!("marketdata-menu-button-{TILE}"));
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "menu");
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// Task 5's Chords fixture: the fragment's own bindings, built exactly
    /// as `content.rs`'s keyboard tests do (a registry from `ACTIONS`,
    /// `fragment_doc`, then `build_keymap`), installed as the
    /// `tips::Chords` global — the module-visible keymap read `chord_for`
    /// resolves a tooltip's chord against.
    fn install_fragment_chords(vcx: &mut gpui::VisualTestContext) {
        let mut registry = geode_shell::actions::ActionRegistry::default();
        for (id, title) in crate::content::ACTIONS {
            registry
                .register(geode_shell::actions::ActionDef {
                    id: ActionId((*id).to_string()),
                    title: (*title).to_string(),
                    category: "Market data".to_string(),
                })
                .expect("no duplicate ids");
        }
        let doc =
            geode_shell::keymap::fragments::fragment_doc(CVI.kind, crate::content::DEFAULT_KEYMAP)
                .expect("the fragment parses");
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &[doc],
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
        vcx.update(|_window, cx| {
            cx.set_global(geode_shell::tips::Chords(Arc::new(
                keymap.bindings().to_vec(),
            )));
        });
    }

    #[gpui::test]
    fn hovering_the_menu_button_names_actions_and_its_chord(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_fragment_chords(&mut vcx);

        let btn = centre_of(&mut vcx, &format!("marketdata-menu-button-{TILE}"));
        vcx.simulate_mouse_move(btn, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        let tip_selector: &'static str =
            Box::leak(format!("tip-marketdata-menu-button-{TILE}").into_boxed_str());
        assert!(vcx.debug_bounds(tip_selector).is_some());
        let chord_selector: &'static str =
            Box::leak(format!("tip-marketdata-menu-button-{TILE}-chord-.").into_boxed_str());
        assert!(
            vcx.debug_bounds(chord_selector).is_some(),
            "the fragment binds `.`"
        );
    }

    #[gpui::test]
    fn hovering_the_behind_state_explains_rebase_and_revert(cx: &mut gpui::TestAppContext) {
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

        let run_selector: &'static str =
            Box::leak(format!("marketdata-state-{TILE}").into_boxed_str());
        let run = centre_of(&mut vcx, run_selector);
        vcx.simulate_mouse_move(run, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        let tip_selector: &'static str =
            Box::leak(format!("tip-marketdata-state-{TILE}").into_boxed_str());
        assert!(vcx.debug_bounds(tip_selector).is_some());
    }

    /// The state run's `Behind`-explaining tooltip (`header.rs`'s
    /// `.when(matches!(h.badge, DraftBadge::Behind { .. }))`) must not
    /// paint for any other state that run shows — "no document yet" here,
    /// a real painted state run (badge `Clean`) with nothing behind to
    /// rebase or revert. Without the gate this negative case has no
    /// failing test to catch it.
    #[gpui::test]
    fn a_tile_that_is_not_behind_has_no_rebase_tooltip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);

        let run_selector: &'static str =
            Box::leak(format!("marketdata-state-{TILE}").into_boxed_str());
        let run = centre_of(&mut vcx, run_selector);
        vcx.simulate_mouse_move(run, gpui::MouseButton::Left, gpui::Modifiers::none());
        vcx.executor()
            .advance_clock(std::time::Duration::from_millis(600));
        vcx.run_until_parked();
        let tip_selector: &'static str =
            Box::leak(format!("tip-marketdata-state-{TILE}").into_boxed_str());
        assert!(vcx.debug_bounds(tip_selector).is_none());
    }

    #[gpui::test]
    fn a_kind_action_answers_not_built_yet(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "cvi_reanchor", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("not built yet".into())
        );
    }

    /// Fix round 1, IMPORTANT-1: the `⋯` button's capture-phase handler
    /// must decide the click first WITHOUT stopping propagation, or the
    /// shell's own tile-level bubble listeners (click-to-focus, drag
    /// arming, `pending_focus_restore`) never run for that click — this
    /// crate's harness has one tile and no shell, so [`Host`]'s own
    /// bubble-phase counter stands in for them. Both clicks (open, then
    /// close) must reach it.
    #[gpui::test]
    fn a_menu_button_click_still_reaches_the_tiles_own_listeners(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let button = centre_of(&mut vcx, &format!("marketdata-menu-button-{TILE}"));
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "menu");
        assert_eq!(
            h.host_clicks(),
            1,
            "the first click still bubbles to the tile's own listeners"
        );
        click_at(&mut vcx, button, 1);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.host_clicks(),
            2,
            "the second click — the one that closes the menu — bubbles too"
        );
    }

    /// User report 2026-09-17: "mousing over the list items should focus
    /// them but instead it's still changing the highlighting of the rows
    /// in the grid underneath." Two halves. The popup must OCCLUDE — a
    /// hover over it may not reach anything painted beneath (the host's
    /// hover-gated counter stands in for the grid's row hover) — and a
    /// hover over an action row moves the highlight, the mouse form of
    /// `j`/`k`.
    #[gpui::test]
    fn hovering_a_menu_row_moves_the_highlight_and_occludes_the_grid(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        // A move over the grid reaches the host: the counter works.
        let cell = centre_of(&mut vcx, "marketdata-cell-1-1");
        move_to(&mut vcx, cell);
        let over_grid = h.host_moves();
        assert!(over_grid >= 1, "a move over the grid is seen beneath");

        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_highlighted()), Some(0));
        // Row 2 is "Revert edits" on a clean draft — greyed, but a hover
        // is a hover: the highlight follows the mouse, enabled or not.
        let row = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-2"));
        move_to(&mut vcx, row);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.menu_highlighted()),
            Some(2),
            "the hovered row is the highlighted one"
        );
        assert_eq!(
            h.host_moves(),
            over_grid,
            "a move over the popup never reaches what is painted beneath it"
        );
        let first = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-0"));
        move_to(&mut vcx, first);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_highlighted()), Some(0));
    }

    /// The picker is the same popup shell and gets the same two halves.
    #[gpui::test]
    fn hovering_a_picker_row_moves_the_highlight_and_occludes_the_grid(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.with_document(&mut vcx);
        let cell = centre_of(&mut vcx, "marketdata-cell-1-1");
        move_to(&mut vcx, cell);
        let over_grid = h.host_moves();

        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("AAA.Z".into())
        );
        let row = centre_of(&mut vcx, &format!("marketdata-picker-row-{TILE}-2"));
        move_to(&mut vcx, row);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".into()),
            "the hovered row is the highlighted one"
        );
        assert_eq!(
            h.host_moves(),
            over_grid,
            "a move over the picker never reaches what is painted beneath it"
        );
    }

    // ---- the segmented date field (header spec §5.2, 2026-09-19) -------

    /// Opens the field on `anchor_date` (Attr 0) and paints it: the
    /// field's `on_key_down` is a listener on the painted, focused element,
    /// so every test that types into it draws first.
    fn open_date_field(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        h.dispatch(vcx, "up", None); // Attr(0) = anchor_date
        h.dispatch(vcx, "edit", None);
        draw(vcx);
    }

    fn date_segments(h: &Harness, vcx: &gpui::VisualTestContext) -> ([String; 3], Segment) {
        let field = h
            .tile
            .read_with(vcx, |t, _| t.date_field())
            .expect("an open date field");
        let v = field.segments();
        (
            [
                v[0].text.to_string(),
                v[1].text.to_string(),
                v[2].text.to_string(),
            ],
            field.segment(),
        )
    }

    fn type_keys(vcx: &mut gpui::VisualTestContext, keys: &str) {
        vcx.simulate_keystrokes(keys);
        draw(vcx);
    }

    /// `i` on a `Date` attribute opens the segmented field, not the text
    /// editor: insert mode, the field's own handle holding the keyboard,
    /// the fixture's date across the three segments, and the DAY active
    /// (user ruling 2026-09-19).
    #[gpui::test]
    fn i_on_a_date_attribute_opens_the_field_on_the_day_segment(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        assert_eq!(h.mode(&vcx), "insert");
        assert!(
            h.tile.read_with(&vcx, |t, _| t.editor_state()).is_none(),
            "a date attribute opens no text Input"
        );
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the field's own handle holds the keyboard"
        );
        let (texts, active) = date_segments(&h, &vcx);
        assert_eq!(texts, ["2026", "09", "12"]);
        assert_eq!(active, Segment::Day);
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-date-seg-{TILE}-2").into_boxed_str()
            ))
            .is_some(),
            "the day segment is painted"
        );
    }

    /// The arrows through the field's own listener: `up` steps the day,
    /// `shift-up` ten, `left` moves to the month (the day kept when it
    /// still fits), `left` again to the year, and `enter` commits the
    /// composed date into the draft — the header paints it edited, with
    /// the dirty dot.
    #[gpui::test]
    fn arrows_step_each_segment_and_enter_commits_the_date(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        type_keys(&mut vcx, "up up up");
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "09", "15"]);
        type_keys(&mut vcx, "shift-up");
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "09", "25"]);
        type_keys(&mut vcx, "left down");
        assert_eq!(
            date_segments(&h, &vcx),
            (["2026", "08", "25"].map(String::from), Segment::Month)
        );
        type_keys(&mut vcx, "left up");
        assert_eq!(
            date_segments(&h, &vcx),
            (["2027", "08", "25"].map(String::from), Segment::Year)
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "nothing is written until enter"
        );
        type_keys(&mut vcx, "enter");
        assert_eq!(h.mode(&vcx), "normal");
        let expected = Value::Date(chrono::NaiveDate::from_ymd_opt(2027, 8, 25).unwrap());
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().attrs.get("anchor_date").cloned()),
            Some(expected)
        );
        let (texts, dirty) = h
            .tile
            .read_with(&vcx, |t, _| (t.header_texts(), t.header_dirty()));
        assert!(
            texts.contains(&"anchor 2027-08-25".to_string()),
            "{texts:?}"
        );
        assert!(dirty);
        assert!(h.tile.read_with(&vcx, |t, _| t.model().header[0].edited));
    }

    /// Typing: `1` into the month waits (shown typing), `2` completes 12
    /// and advances to the day, `3` waits there, `0` completes 30 —
    /// `enter` commits the typed date.
    #[gpui::test]
    fn typed_digits_fill_a_segment_and_advance(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        type_keys(&mut vcx, "left 1");
        let field = h.tile.read_with(&vcx, |t, _| t.date_field()).unwrap();
        let month = &field.segments()[1];
        assert_eq!(
            (month.text.as_str(), month.typing, month.active),
            ("1", true, true)
        );
        assert_eq!(
            field.date(),
            chrono::NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()
        );
        type_keys(&mut vcx, "2");
        assert_eq!(
            date_segments(&h, &vcx),
            (["2026", "12", "12"].map(String::from), Segment::Day)
        );
        type_keys(&mut vcx, "3");
        assert_eq!(date_segments(&h, &vcx).0[2], "3");
        type_keys(&mut vcx, "9");
        assert_eq!(
            date_segments(&h, &vcx).0[2],
            "3",
            "39 is refused; the 3 stays"
        );
        type_keys(&mut vcx, "backspace");
        assert_eq!(
            date_segments(&h, &vcx).0[2],
            "12",
            "backspace shows the value again"
        );
        type_keys(&mut vcx, "3 0 enter");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().attrs.get("anchor_date").cloned()),
            Some(Value::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 12, 30).unwrap()
            ))
        );
    }

    /// `escape` after changes writes nothing: the draft stays empty and
    /// the attribute paints the date it had. And the field gives the
    /// keyboard up on its way out (blur, then drop — `close_editor`'s
    /// rule), so `Window::focused` is `None` for the shell's own net.
    #[gpui::test]
    fn escape_restores_the_painted_date_and_blurs_the_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        let focus = h.tile.read_with(&vcx, |t, _| t.date_field_focus()).unwrap();
        assert!(vcx.update(|window, _| focus.is_focused(window)));
        type_keys(&mut vcx, "up up left down escape");
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .contains(&"anchor 2026-09-12".to_string())
        );
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "escape must blur the field before dropping it"
        );
        // The fragment's own door (`marketdata::cancel`, the shell's
        // dispatch) closes it the same way.
        open_date_field(&h, &mut vcx);
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(vcx.update(|window, cx| window.focused(cx).is_none()));
    }

    /// `enter` with a single digit still waiting in a segment completes
    /// it as `0d` before committing (review I-1): `2` in the day then
    /// `enter` writes the 2nd, never the 12th that was there. Through
    /// both doors — the field's own `enter` and the fragment's `commit`.
    #[gpui::test]
    fn enter_completes_a_pending_digit_rather_than_committing_the_old_date(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        type_keys(&mut vcx, "2 enter");
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().attrs.get("anchor_date").cloned()),
            Some(Value::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 9, 2).unwrap()
            ))
        );
        // The fragment's door, with a waiting month digit.
        open_date_field(&h, &mut vcx);
        type_keys(&mut vcx, "left 1");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().attrs.get("anchor_date").cloned()),
            Some(Value::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()
            ))
        );
    }

    /// A pending entry that cannot complete — `0` in the day, a
    /// two-digit year — refuses the commit with a notice naming the
    /// segment, and the editor stays open with the digits as typed.
    #[gpui::test]
    fn enter_on_an_incompletable_digit_is_refused_naming_the_segment(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        type_keys(&mut vcx, "0 enter");
        assert_eq!(h.mode(&vcx), "insert", "the editor stays open");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("finish the day or backspace".into())
        );
        assert_eq!(date_segments(&h, &vcx).0[2], "0", "the typed digit is kept");
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        type_keys(&mut vcx, "left left 2 0");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("finish the year or backspace".into())
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        type_keys(&mut vcx, "backspace");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            None,
            "a keystroke that changes the field retires the refusal"
        );
        type_keys(&mut vcx, "enter");
        assert_eq!(
            h.mode(&vcx),
            "normal",
            "backspace then enter commits the value as it stood"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().attrs.get("anchor_date").cloned()),
            Some(Value::Date(
                chrono::NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()
            ))
        );
    }

    /// A click on a segment reclaims the keyboard for the field when it
    /// has lost it (review M4): the orphaned-editor state, where the
    /// field is still open but window focus is elsewhere.
    #[gpui::test]
    fn a_segment_click_refocuses_an_unfocused_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        vcx.update(|window, cx| window.blur(cx));
        assert!(!vcx.update(|window, cx| h.content.holds_focus(window, cx)));
        assert_eq!(h.mode(&vcx), "insert", "the editor is still open");
        let year = centre_of(&mut vcx, &format!("marketdata-date-seg-{TILE}-0"));
        click_at(&mut vcx, year, 1);
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the click gave the field the keyboard back"
        );
        assert_eq!(date_segments(&h, &vcx).1, Segment::Year);
    }

    /// A click on a segment selects it — the mouse form of `left`/`right`
    /// — and leaves the editor open: the segment's own mouse-down stops
    /// before the attribute value's `attr_clicked`, which would otherwise
    /// cancel the editor the click was aimed into.
    #[gpui::test]
    fn a_click_on_a_segment_selects_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        let year = centre_of(&mut vcx, &format!("marketdata-date-seg-{TILE}-0"));
        click_at(&mut vcx, year, 1);
        assert_eq!(
            h.mode(&vcx),
            "insert",
            "the click did not cancel the editor"
        );
        assert_eq!(date_segments(&h, &vcx).1, Segment::Year);
        let month = centre_of(&mut vcx, &format!("marketdata-date-seg-{TILE}-1"));
        click_at(&mut vcx, month, 1);
        assert_eq!(date_segments(&h, &vcx).1, Segment::Month);
        type_keys(&mut vcx, "up");
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "10", "12"]);
    }

    /// A chord is never the field's: `ctrl-k` (the palette, in the real
    /// shell) bubbles past it to the host — the stand-in for the shell
    /// root's own listener — with the field untouched and still open.
    #[gpui::test]
    fn a_chord_passes_through_the_field_to_the_shell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        open_date_field(&h, &mut vcx);
        let before = h.host_keys();
        type_keys(&mut vcx, "ctrl-k");
        assert_eq!(
            h.host_keys(),
            before + 1,
            "the chord reached the host's own listener"
        );
        assert_eq!(h.mode(&vcx), "insert");
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "09", "12"]);
        // A chord on a key the field WOULD otherwise handle is still not
        // its own: `ctrl-up` reaches the host and steps nothing.
        type_keys(&mut vcx, "ctrl-up");
        assert_eq!(h.host_keys(), before + 2, "ctrl-up passed through too");
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "09", "12"]);
        // And a bare key the field consumes does NOT reach it.
        type_keys(&mut vcx, "up");
        assert_eq!(
            h.host_keys(),
            before + 2,
            "a consumed key stops at the field"
        );
        assert_eq!(date_segments(&h, &vcx).0, ["2026", "09", "13"]);
    }

    /// `spot_ref` (F64) still opens the text editor — only a `Date`
    /// attribute opens the field.
    #[gpui::test]
    fn a_number_attribute_still_opens_the_text_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "up", None); // Attr(1) = spot_ref
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state()).is_some());
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field()).is_none());
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
    }

    /// User report 2026-09-18: `shift+up` in an open cell editor moved the
    /// GRID's row selection instead of nudging. gpui-base's `Input` binds
    /// `shift-up`/`shift-down` to `SelectUp`/`SelectDown`; a single-line
    /// input returns from `select_up` without stopping propagation, so the
    /// action bubbled to the enclosing `DataTable`'s own `SelectUp` handler
    /// — one action type, re-exported by gpui-component — and moved the
    /// selection out from under the tile's cursor, before the shell's key
    /// handler ever saw the keystroke. The shell now reclaims both keys in
    /// the `Input` context (`dialog::init_reclaimed_keybindings`), so the
    /// keystroke falls through to the keymap; this pins the half a panel
    /// harness can see — the table's selection stays put.
    #[gpui::test]
    fn shift_up_in_the_editor_no_longer_moves_the_tables_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "down", None);
        assert_eq!(h.selection(&vcx), (Some(1), Some(1)));
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.mode(&vcx), "insert");
        vcx.simulate_keystrokes("shift-up");
        draw(&mut vcx);
        assert_eq!(
            h.selection(&vcx),
            (Some(1), Some(1)),
            "the component's SelectUp must not reach the table from the editor"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        assert_eq!(h.mode(&vcx), "insert", "the editor is still open");
        vcx.simulate_keystrokes("shift-down");
        draw(&mut vcx);
        assert_eq!(h.selection(&vcx), (Some(1), Some(1)));
    }

    /// Fix round 1, IMPORTANT-2: `/` is a shell-owned `tile`-context
    /// binding that never reaches `dispatch`'s own "any other action
    /// closes the popup" guard, so `find` must close it itself — on the
    /// very first keystroke of a find session started with the menu
    /// open.
    #[gpui::test]
    fn a_find_keystroke_closes_the_popup(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("1M".into()), window, cx));
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// The other half of fix round 1, IMPORTANT-2: `:` is the same kind
    /// of shell-owned door, so `command` closes the popup itself for
    /// every parsed command but `Menu` (which toggles it).
    #[gpui::test]
    fn a_command_line_closes_the_popup(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        h.command(&mut vcx, "bump 1").unwrap();
        assert_eq!(h.mode(&vcx), "normal");
    }

    // ---- the underlying picker (Task 7, spec §7) ----------------------

    /// `u` opens the picker (`mode == insert`, its field holds the
    /// keyboard); typing filters `ranked` over the catalog's own keys;
    /// `enter` re-ranks from the CURRENT text (`set_picker_text` writes
    /// through `set_value`, which emits no `Change` at all — the trap
    /// `commit_picker`'s own re-rank exists to cover) and loads the top
    /// match through the same door `:underlying` uses.
    #[gpui::test]
    fn u_opens_the_picker_typing_filters_and_enter_loads(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["SPX.Z", "NKY.Z", "SX5E.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.set_picker_text(&mut vcx, "nky");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.mode(&vcx), "normal");
        let req = h
            .document_request()
            .expect("the pick requests its document");
        assert_eq!(req.document_key, vec!["NKY.Z".to_string()]);
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .contains(&"NKY.Z".to_string())
        );
    }

    /// Exactly as `:key`/`:underlying` now park rather than refuse
    /// (2026-09-19): `u` with a dirty draft OPENS the picker — a pick
    /// parks the draft under its underlying, so there is nothing to warn
    /// about and no `:revert` to name.
    #[gpui::test]
    fn the_picker_opens_while_the_draft_has_edits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 1").unwrap();
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        assert!(
            h.tile.read_with(&vcx, |t, _| t.notice().is_none()),
            "no refusal notice"
        );
    }

    /// `cancel` (`escape` in insert mode) must give the keyboard up
    /// BEFORE dropping the field — `close_editor`'s own order, here for
    /// `close_popup_with_window` — or `Window::focused` never reports
    /// `None` and the shell's own dropped-focus net can never fire.
    #[gpui::test]
    fn escape_closes_the_picker_and_gives_focus_up(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "blurred before dropped"
        );
    }

    /// Opening the picker re-requests the catalog unconditionally (spec
    /// §7), the same MIN-4 rule `:key`'s own line follows: a held catalog
    /// is not necessarily a fresh one, and a subscribed feed can publish
    /// a new key at any time.
    #[gpui::test]
    fn opening_the_picker_asks_for_a_fresh_catalog(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        // Drain `set_visible`'s own request first.
        h.diagnostics
            .update(&mut vcx, |d, _| d.take_pending_catalog_request());
        h.dispatch(&mut vcx, "load_underlying", None);
        assert!(
            h.diagnostics
                .read_with(&vcx, |d, _| d.pending_catalog_request()),
            "opening the picker asks again, regardless of what is already held"
        );
    }

    /// Review fix round 1, CRITICAL: `enter` must load whichever row is
    /// HIGHLIGHTED, not always the top match. The old `refilter` reset
    /// the highlight to 0 on every call, and `commit_picker` always
    /// calls it once (defensively, to cover a test harness's
    /// `set_value`, which fires no `Change` at all) — so `u`, `down`,
    /// `down`, `enter` used to silently request the TOP key regardless
    /// of where the trader had actually moved the highlight.
    #[gpui::test]
    fn enter_loads_the_highlighted_row_not_the_top_match(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.dispatch(&mut vcx, "menu_down", None);
        h.dispatch(&mut vcx, "menu_down", None);
        h.dispatch(&mut vcx, "commit", None);
        let req = h
            .document_request()
            .expect("the pick requests its document");
        assert_eq!(
            req.document_key,
            vec!["CCC.Z".to_string()],
            "the THIRD row, not the first"
        );
    }

    /// Review fix round 1, IMPORTANT-1: `marketdata::menu` (reachable via
    /// the palette's "Actions menu" row, not gated by mode at all) must
    /// close an open PICKER first, blurring before dropping it, rather
    /// than overwriting `self.popup` out from under a still-focused
    /// `InputState` — and then still open the menu, since that row's
    /// whole point was to open it.
    #[gpui::test]
    fn menu_closes_an_open_picker_with_a_blur_before_opening(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "the picker's field must be blurred, not just dropped"
        );
    }

    /// Review fix round 1, IMPORTANT-2: a diagnostics notification that
    /// carries no real catalog change (a source's health tick, or the
    /// SAME catalog reported again) must not reset the picker's
    /// highlight, and a catalog that genuinely changes must re-rank
    /// keeping the highlight on the same KEY rather than the same
    /// position.
    #[gpui::test]
    fn diagnostics_catalog_updates_preserve_the_highlight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.dispatch(&mut vcx, "menu_down", None);
        h.dispatch(&mut vcx, "menu_down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".to_string())
        );

        // The identical catalog again: nothing to rerank, the highlight
        // must not move.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".to_string()),
            "an unrelated notification must not reset the highlight"
        );

        // A grown catalog: CCC.Z is still there, just not necessarily at
        // the same RANKED position.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z", "DDD.Z"]));
            cx.notify();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".to_string()),
            "the highlight follows the KEY across a catalog change"
        );
    }

    /// Review fix round 2: the round-1 fix above still tracked the
    /// wrong row when the catalog RE-SORTS around the highlighted key —
    /// `catalog_keys()` returns a freshly sorted list, so a new key that
    /// sorts BEFORE the highlighted one shifts every later index, and
    /// the old "preserve by ALL-index" logic silently landed on whatever
    /// key now sat at that number (never falling back to 0, so nothing
    /// signalled the mistake). Identity must be the KEY STRING: `AAA.Z`
    /// sorting ahead of the highlighted `CCC.Z` shifts it from index 1
    /// to index 2, and the highlight must follow it there; a genuine
    /// removal still falls back to row 0.
    #[gpui::test]
    fn a_resorted_catalog_keeps_the_highlighted_key_not_its_old_index(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.dispatch(&mut vcx, "menu_down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".to_string()),
            "highlighted CCC.Z at its original index 1"
        );

        // AAA.Z sorts AHEAD of both — CCC.Z shifts from index 1 to 2.
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("CCC.Z".to_string()),
            "the highlight follows CCC.Z to its new index, not whatever \
             now sits at the old one"
        );

        // CCC.Z is removed entirely — falls back to row 0 (BBB.Z).
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["BBB.Z"]));
            cx.notify();
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some("BBB.Z".to_string()),
            "a genuine removal falls back to row 0"
        );
    }

    // ---- Final review (2026-09-17) ----------------------------------

    /// B2: a key change is navigation, and an open cell editor is
    /// CANCELLED across it — never committed into the new document's
    /// draft, and never left open and deaf under a swapped grid.
    /// Reachable for real via `i`, `mod+l` (focus to the shell root, the
    /// editor still open), then `:underlying`.
    #[gpui::test]
    fn a_key_change_cancels_an_open_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        assert_eq!(h.mode(&vcx), "insert");
        h.command(&mut vcx, "underlying NDX.Z").unwrap();
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(h.editor_value(&vcx), None, "the editor is gone");
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "cancelled, never committed: {:?}",
            h.tile.read_with(&vcx, |t, _| t.draft().count_phrase())
        );
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "blurred before dropped"
        );
        assert_eq!(
            h.document_request().map(|r| r.document_key),
            Some(vec!["NDX.Z".to_string()]),
            "and the new document was asked for"
        );
    }

    /// B4: a click on an attribute value while a cell editor is open
    /// cancels the editor first — `a_click_while_editing_cancels_the_
    /// editor_then_moves`, for the strip. Without it this was the one
    /// mouse door that left an editor open and deaf (the mouse-down has
    /// re-armed the shell's focus restore).
    #[gpui::test]
    fn an_attribute_click_cancels_the_editor_then_moves(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        assert_eq!(h.mode(&vcx), "insert");
        let at = centre_of(&mut vcx, &format!("marketdata-attr-{TILE}-1"));
        click_at(&mut vcx, at, 1);
        assert_eq!(h.editor_value(&vcx), None, "the click cancelled the editor");
        assert_eq!(h.mode(&vcx), "normal", "and insert mode went with it");
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a click is not `enter`: nothing was written"
        );
        assert_eq!(h.cell(&vcx, 0, 0).0, "4500.00");
    }

    /// The menu-row → picker path leaves the picker's field holding the
    /// keyboard. This no longer rests on the row's own
    /// `stop_propagation` (B5's original claim, since superseded — see
    /// `popup.rs`'s comment on that handler): the 2026-09-17 insert-focus
    /// rule (`occupant_holds_insert_focus`) means `render`'s own
    /// `pending_focus_restore` consumption is skipped whenever the
    /// focused tile's occupant holds its own input in insert mode, so a
    /// bubbled click could no longer take the keyboard back on the next
    /// render either way. [`Host`]'s counter (its own doc comment) still
    /// stands in for that bubble and still reads zero here, though the
    /// stop it once credited is now kept for an unrelated reason (a click
    /// that means "pick a row" must not also run the shell's ordinary
    /// tile click handling — see `popup.rs`).
    ///
    /// The cursor is put in the STRIP first, deliberately: with it in the
    /// grid, the pinned `TableState::set_selected_row` (which `sync_cursor`
    /// runs at the end of every `dispatch`) calls `cx.stop_propagation()`
    /// of its own and would hide the row handler's — in the strip,
    /// `sync_cursor` calls `clear_selection`, which stops nothing, so the
    /// row's own stop is the only thing between the click and the bubble.
    #[gpui::test]
    fn the_menu_row_to_picker_path_leaves_the_pickers_field_focused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "up", None);
        assert!(matches!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Attr(_)
        ));
        h.dispatch(&mut vcx, "menu", None);
        let row = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-0")); // Load underlying…
        click_at(&mut vcx, row, 1);
        assert_eq!(h.mode(&vcx), "insert", "the picker opened");
        let input = h
            .tile
            .read_with(&vcx, |t, _| t.picker_state())
            .expect("the picker's field exists");
        assert!(
            vcx.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
            "the picker's field holds the keyboard"
        );
        assert_eq!(
            h.host_clicks(),
            0,
            "the row click never bubbled to the tile's own listeners"
        );
    }

    /// T1: `:set <attr>` with no value and no document says there is no
    /// document, not that a declared attribute does not exist.
    #[gpui::test]
    fn set_with_no_value_and_no_document_says_no_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(
            h.command(&mut vcx, "set spot_ref"),
            Err(NO_DOCUMENT.to_string())
        );
    }

    /// T2: opening the action list with a cell editor open cancels the
    /// editor first (blur, then drop) — never commits it — and the menu
    /// then owns the keyboard with no field focused.
    #[gpui::test]
    fn toggle_menu_with_an_editor_open_cancels_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        assert_eq!(h.editor_value(&vcx), None);
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "blurred before dropped"
        );
    }

    /// T3: a click outside the open picker closes it through the
    /// blur-first door — the mouse's `escape`. The click lands on an
    /// attribute value rather than a grid cell: the pinned `DataTable`
    /// `track_focus`es its own handle, so a cell click would take focus on
    /// the same mouse-down (in the shell, `pending_focus_restore` returns
    /// it to the root a frame later — not this crate's to assert), and
    /// `focused.is_none()` would then say nothing about the blur. The
    /// strip focuses nothing, so `None` afterwards IS the blur.
    #[gpui::test]
    fn a_click_outside_the_picker_closes_it_and_gives_focus_up(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        let input = h
            .tile
            .read_with(&vcx, |t, _| t.picker_state())
            .expect("the picker's field exists");
        let at = centre_of(&mut vcx, &format!("marketdata-attr-{TILE}-1"));
        click_at(&mut vcx, at, 1);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(
            vcx.update(|window, cx| !input.read(cx).focus_handle(cx).is_focused(window)),
            "the picker's field gave the keyboard up"
        );
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "blurred before dropped"
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(1));
    }

    /// T4: a find started from the attribute strip cancels back INTO the
    /// strip — the origin is the whole `Cursor`, not a grid row.
    #[gpui::test]
    fn a_find_cancelled_from_the_strip_returns_to_the_strip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "up", None);
        let origin = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert!(matches!(origin, Cursor::Attr(_)), "{origin:?}");
        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("11-20".into()), window, cx)
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 },
            "the search runs from the top of the grid"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            origin,
            "escape returns to the strip, not to grid row 0"
        );
    }

    /// A3: the picker paints at most `PICKER_ROWS` of its ranked keys at
    /// once (the query narrows the rest). Window semantics (2026-09-19
    /// ruling): stepping past the cap does not stop the highlight at the
    /// last painted row — it drags the window along, so the highlight
    /// lands on the last DECLARED key with the window following.
    #[gpui::test]
    fn the_picker_paints_at_most_twelve_rows(cx: &mut gpui::TestAppContext) {
        use crate::popup::PICKER_ROWS;
        let (h, mut vcx) = open(cx);
        let keys: Vec<String> = (0..20).map(|i| format!("K{i:02}.Z")).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&refs));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        draw(&mut vcx);
        let last: &'static str =
            Box::leak(format!("marketdata-picker-row-{TILE}-{}", PICKER_ROWS - 1).into_boxed_str());
        let past: &'static str =
            Box::leak(format!("marketdata-picker-row-{TILE}-{PICKER_ROWS}").into_boxed_str());
        assert!(
            vcx.debug_bounds(last).is_some(),
            "row {} is painted",
            PICKER_ROWS - 1
        );
        assert!(vcx.debug_bounds(past).is_none(), "row {PICKER_ROWS} is not");
        h.dispatch(&mut vcx, "menu_down", Some(100));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some(keys[19].clone()),
            "the highlight follows to the last declared row, with the \
             window sliding to keep it painted"
        );
    }

    /// Re-review of the fix wave: a picker orphaned with the keyboard
    /// elsewhere (`u`, `ctrl+k`, the palette's "Find" — the shell's
    /// command line holds focus, the picker is still `Some`) is closed by
    /// the first find keystroke WITHOUT blurring whoever holds the
    /// keyboard. The second `InputState` stands in for the shell's
    /// `command_input`; an unconditional blur would have cancelled the
    /// trader's find after one character.
    #[gpui::test]
    fn a_find_keystroke_with_an_orphaned_picker_keeps_the_foreign_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "load_underlying", None);
        assert_eq!(h.mode(&vcx), "insert");
        let foreign = vcx.update(|window, cx| {
            let input = cx.new(|cx| InputState::new(window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            input
        });
        assert!(
            vcx.update(|window, cx| foreign.read(cx).focus_handle(cx).is_focused(window)),
            "the stand-in command line holds the keyboard"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("1M".into()), window, cx));
        assert_eq!(h.mode(&vcx), "normal", "the orphaned picker is closed");
        assert!(
            h.tile.read_with(&vcx, |t, _| t.picker_state().is_none()),
            "and dropped"
        );
        assert!(
            vcx.update(|window, cx| foreign.read(cx).focus_handle(cx).is_focused(window)),
            "the foreign field still holds the keyboard — nothing blurred it"
        );
    }

    // ---- the update policy (spec §8.4, 2026-09-19) ----------------------

    /// One committed edit, then a newer generation with the SAME labels
    /// under `:auto rebase`: no `Behind`, the newer document is painted,
    /// the edit is re-placed on it (still `edited`, value kept, base moved)
    /// and nothing is reported since nothing was dropped.
    #[gpui::test]
    fn auto_rebase_re_places_the_edits_onto_a_newer_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto rebase").unwrap();
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Rebase
        );
        let tag = h.with_document_tagged(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));

        let (state, base, source, rows, cell, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().base.clone(),
                t.model().source_time.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(state, DraftState::Editing, "never Behind under rebase");
        assert_eq!(
            base.as_deref(),
            Some(NEWER),
            "the draft now stands on the new base"
        );
        assert_eq!(
            source.as_deref(),
            Some(NEWER),
            "and the new document is painted"
        );
        assert_eq!(rows, 2);
        assert_eq!(
            cell.text.to_string(),
            "0.50",
            "the edit is re-placed, value kept"
        );
        assert!(cell.edited);
        assert_eq!(notice, None, "nothing dropped, nothing to say");
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            !chips.iter().any(|c| c.starts_with("update ")),
            "no Behind state run: {chips:?}"
        );
    }

    /// Under `:auto rebase`, a newer document lacking one of the edited
    /// terms drops that edit and names it — the same disclosure `:rebase`
    /// makes — while the edit whose labels survive is re-placed at its
    /// NEW index.
    #[gpui::test]
    fn auto_rebase_names_the_edits_the_new_document_cannot_carry(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto rebase").unwrap();
        let tag = h.with_document_tagged(&mut vcx);

        // Term 0's `fwd` (dropped by the newer document) and term 1's
        // `fwd` (kept — the newer document's only row).
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.7");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 2);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        let (state, len, rows, cell, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().len(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(state, DraftState::Editing);
        assert_eq!(len, 1, "one edit survived");
        assert_eq!(rows, 1, "painting the newer document");
        assert_eq!(
            cell.text.to_string(),
            "0.70",
            "the kept edit, at its new index"
        );
        assert!(cell.edited);
        assert_eq!(
            notice.as_deref(),
            Some("dropped 1 edit whose rows or columns the new document lacks: 2026-10-16/fwd")
        );
    }

    /// Under `:auto replace`, a newer generation drops every edit — cells
    /// and attributes — paints the new document, and the notice says
    /// exactly what went, in `count_phrase`'s own spelling.
    #[gpui::test]
    fn auto_replace_drops_the_edits_and_says_how_many(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto replace").unwrap();
        let tag = h.with_document_tagged(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.6");
        h.dispatch(&mut vcx, "commit", None);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 3);

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        let (draft, source, rows, cell, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().clone(),
                t.model().source_time.clone(),
                t.model().rows.len(),
                t.model().rows[0].cells[0].clone(),
                t.notice().map(str::to_string),
            )
        });
        assert!(draft.is_empty(), "every edit is gone: {draft:?}");
        assert_eq!(draft.state, DraftState::Clean);
        assert_eq!(source.as_deref(), Some(NEWER));
        assert_eq!(rows, 1, "the newer document is painted");
        assert!(!cell.edited);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        assert_eq!(
            notice,
            Some(format!(
                "update {} replaced 2 cells, spot_ref",
                local_hhmm(NEWER, clock)
            ))
        );
        let chips = h.tile.read_with(&vcx, |t, _| t.header_texts());
        assert!(
            !chips
                .iter()
                .any(|c| c.starts_with("update ") && !c.contains("replaced")),
            "no Behind state run: {chips:?}"
        );
    }

    /// The policy applies on the next NEW generation, never on the
    /// switch and never on a redelivery: a draft left `Behind` under
    /// `hold` stays exactly there when the trader switches to `rebase`,
    /// stays there again when the SAME newer generation is redelivered
    /// (a `data` bump from an unrelated publish, review I-1), and only a
    /// further generation moves the edits — onto that newest document.
    #[gpui::test]
    fn switching_to_auto_rebase_does_not_rebase_a_draft_already_behind(
        cx: &mut gpui::TestAppContext,
    ) {
        const NEWEST: &str = "2026-09-12T14:15:00Z";
        let (h, mut vcx) = open(cx);
        let tag = h.with_document_tagged(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));

        h.command(&mut vcx, "auto rebase").unwrap();
        let (state, source, rows) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().source_time.clone(),
                t.model().rows.len(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "still Behind after the switch, got {state:?}"
        );
        assert_eq!(source.as_deref(), Some(BASE), "still painting the base");
        assert_eq!(rows, 2);

        // The SAME newer generation redelivered (any dataset's publish
        // bumps `data` and this panel requeries): not a transition, so
        // the policy does nothing — a `replace` here would be a `:revert`
        // run by an unrelated publish.
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        let (state, source, len) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().source_time.clone(),
                t.draft().len(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "a redelivery never acts, got {state:?}"
        );
        assert_eq!(source.as_deref(), Some(BASE));
        assert_eq!(len, 1, "the edit is intact");

        // A further generation under the new policy: onto the NEWEST.
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWEST)));
        let (state, base, source, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().base.clone(),
                t.model().source_time.clone(),
                t.model().rows[0].cells[0].clone(),
            )
        });
        assert_eq!(state, DraftState::Editing, "got {state:?}");
        assert_eq!(base.as_deref(), Some(NEWEST));
        assert_eq!(source.as_deref(), Some(NEWEST));
        assert_eq!(cell.text.to_string(), "0.50");
        assert!(cell.edited);
    }

    /// `auto` rides the session only when it is not the default, and an
    /// unknown word restores as `hold` rather than refusing the tile.
    #[gpui::test]
    fn the_policy_round_trips_through_the_session(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = r#"
underlying = ["SPX.Z"]
auto = "replace"
"#
        .parse()
        .unwrap();
        let (h, mut vcx) = open_with(cx, Some(restored.clone()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Replace,
            "read from the session"
        );
        let written = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert_eq!(written, restored);

        h.command(&mut vcx, "auto hold").unwrap();
        let written = h.tile.read_with(&vcx, |t, _| t.serialize());
        assert!(
            !written.contains_key("auto"),
            "the default writes no key: {written:?}"
        );

        let unknown: toml::Table = r#"
underlying = ["SPX.Z"]
auto = "discard"
"#
        .parse()
        .unwrap();
        let (h, vcx) = open_with(cx, Some(unknown));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Hold,
            "an unknown word is the default, not a refusal"
        );
    }

    /// A bare `:auto` names the current policy through the command line's
    /// inline slot, and a typo is refused with the vocabulary.
    #[gpui::test]
    fn a_bare_auto_names_the_current_policy(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        assert_eq!(
            h.command(&mut vcx, "auto"),
            Err("auto is hold (hold, rebase, replace)".into())
        );
        h.command(&mut vcx, "auto rebase").unwrap();
        assert_eq!(
            h.command(&mut vcx, "auto"),
            Err("auto is rebase (hold, rebase, replace)".into())
        );
        assert!(h.command(&mut vcx, "auto discard").is_err());
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Rebase,
            "a refused word changes nothing"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, cx| t.completions("auto ", 5, cx)),
            vec!["hold", "rebase", "replace"]
        );
    }

    /// The menu's `On new document` section ticks the policy in force,
    /// and picking another row sets it and closes the menu — through the
    /// ordinary `menu_pick` path, four `j`s down from the first row on a
    /// clean draft (`Load`, `Upload`, `Revert`, `hold edits`, `rebase
    /// edits`).
    #[gpui::test]
    fn the_menu_ticks_the_policy_and_a_pick_sets_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        let checks = h.tile.read_with(&vcx, |t, _| t.menu_checks());
        let ticked: Vec<&str> = checks
            .iter()
            .filter(|(_, c)| *c == Some(true))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(ticked, vec!["hold edits"]);
        assert_eq!(
            checks.iter().filter(|(_, c)| c.is_some()).count(),
            3,
            "{checks:?}"
        );

        for _ in 0..4 {
            h.dispatch(&mut vcx, "menu_down", None);
        }
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.menu_highlighted()),
            Some(6),
            "on `rebase edits`"
        );
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.mode(&vcx), "normal", "the pick closed the menu");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Rebase
        );

        h.dispatch(&mut vcx, "menu", None);
        let checks = h.tile.read_with(&vcx, |t, _| t.menu_checks());
        let ticked: Vec<&str> = checks
            .iter()
            .filter(|(_, c)| *c == Some(true))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(ticked, vec!["rebase edits"], "the tick followed");
        // The palette door lands in the same place.
        h.dispatch(&mut vcx, "auto_replace", None);
        assert_eq!(
            h.mode(&vcx),
            "normal",
            "an unrelated action closes the menu"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.policy()),
            UpdatePolicy::Replace
        );
    }

    /// An editor open when the newer document lands under `rebase`: the
    /// editor stays open (a delivery never disturbs typing), and its
    /// commit files against the re-placed cell because the labels still
    /// match — `a_commit_whose_cell_moved_under_it_is_refused`'s shape,
    /// expecting success.
    #[gpui::test]
    fn an_editor_open_across_an_auto_rebase_commits_onto_the_new_document(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto rebase").unwrap();
        let tag = h.with_document_tagged(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.6");
        assert_eq!(h.mode(&vcx), "insert");

        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("0.6"),
            "the editor is untouched"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing
        );

        h.dispatch(&mut vcx, "commit", None);
        let (len, base, cells, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().len(),
                t.draft().base.clone(),
                t.model().rows[0].cells[..2].to_vec(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(notice, None);
        assert_eq!(len, 2, "the typed value landed beside the re-placed edit");
        assert_eq!(base.as_deref(), Some(NEWER), "both against the new base");
        assert_eq!(cells[0].text.to_string(), "0.50");
        assert_eq!(
            cells[1].text.to_string(),
            "0.6000",
            "an `atm` cell, four places"
        );
        assert!(cells[0].edited && cells[1].edited);
        assert!(
            h.editor_value(&vcx).is_none(),
            "and the editor closed on commit"
        );
    }

    /// A draft restored with `auto = "<policy>"` whose base differs from
    /// the first delivery: the restored edits, the base and the newer
    /// generation exactly as `hold` would leave them.
    fn restored_first_delivery(
        cx: &mut gpui::TestAppContext,
        policy: &str,
    ) -> (Harness, gpui::VisualTestContext, u64) {
        let restored: toml::Table = format!(
            r#"
underlying = ["SPX.Z"]
auto = "{policy}"
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
        (h, vcx, tag)
    }

    /// The first delivery after a restore is always `hold` (ruling
    /// 2026-09-19): under `replace`, a restored draft meeting a newer
    /// generation lands `Behind` with its edits intact — never dropped on
    /// a delivery the trader was not watching — and no `replaced` notice
    /// is written.
    #[gpui::test]
    fn a_restored_drafts_first_delivery_is_hold_under_replace(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, tag) = restored_first_delivery(cx, "replace");
        let (policy, state, len, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.policy(),
                t.draft().state.clone(),
                t.draft().len(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(
            policy,
            UpdatePolicy::Replace,
            "the policy itself is restored"
        );
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "Behind, as hold would leave it — got {state:?}"
        );
        assert_eq!(len, 1, "the restored edit survives");
        assert!(
            notice.as_deref().is_none_or(|n| !n.contains("replaced")),
            "nothing was replaced: {notice:?}"
        );

        // The same generation redelivered (a `data` bump): still not a
        // transition, so the restore's protection is not one bump long.
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        let (state, len) = h
            .tile
            .read_with(&vcx, |t, _| (t.draft().state.clone(), t.draft().len()));
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "a redelivery never acts, got {state:?}"
        );
        assert_eq!(len, 1);

        // A FURTHER generation: the policy resumes and acts.
        const NEWEST: &str = "2026-09-12T14:15:00Z";
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWEST)));
        let (draft, source, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().clone(),
                t.model().source_time.clone(),
                t.notice().map(str::to_string),
            )
        });
        assert!(
            draft.is_empty(),
            "replaced on the next new generation: {draft:?}"
        );
        assert_eq!(source.as_deref(), Some(NEWEST));
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        assert_eq!(
            notice,
            Some(format!(
                "update {} replaced 1 cell",
                local_hhmm(NEWEST, clock)
            ))
        );
    }

    /// The same rule under `rebase`: the restored draft is not rebased onto
    /// a generation the trader never chose — it lands `Behind`, and
    /// `:rebase` (or the policy, from the NEXT delivery) is what moves it.
    #[gpui::test]
    fn a_restored_drafts_first_delivery_is_hold_under_rebase(cx: &mut gpui::TestAppContext) {
        let (h, vcx, _) = restored_first_delivery(cx, "rebase");
        let (policy, state, base, len) = h.tile.read_with(&vcx, |t, _| {
            (
                t.policy(),
                t.draft().state.clone(),
                t.draft().base.clone(),
                t.draft().len(),
            )
        });
        assert_eq!(policy, UpdatePolicy::Rebase);
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "Behind, not rebased — got {state:?}"
        );
        assert_eq!(base.as_deref(), Some(BASE), "the base is the restored one");
        assert_eq!(len, 1);
    }

    /// An EMPTY new generation (no rows, but a source time) under
    /// `rebase` with a dirty draft: rebasing against an empty label map
    /// would drop every edit in silence, so the delivery takes the `hold`
    /// path — `Behind`, edits intact, the base still painted, no extra
    /// notice (review I-2).
    #[gpui::test]
    fn an_empty_new_document_never_auto_rebases_a_draft_away(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto rebase").unwrap();
        let tag = h.with_document_tagged(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        h.deliver(&mut vcx, tag, Arc::new(document_of(&[], &NODES, NEWER)));

        let (state, len, source, rows, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().len(),
                t.model().source_time.clone(),
                t.model().rows.len(),
                t.notice().map(str::to_string),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer == NEWER),
            "hold's path, got {state:?}"
        );
        assert_eq!(len, 1, "the edit is intact");
        assert_eq!(source.as_deref(), Some(BASE), "still painting the base");
        assert_eq!(rows, 2);
        assert_eq!(notice, None, "nothing dropped, nothing said");
    }

    /// The `replace` sibling of the editor-open-across-a-delivery test:
    /// the editor stays open through the replacing delivery, the
    /// `replaced` notice stands until the commit, and the commit lands as
    /// a fresh single edit on the NEW base.
    #[gpui::test]
    fn an_editor_open_across_an_auto_replace_commits_as_a_fresh_edit(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto replace").unwrap();
        let tag = h.with_document_tagged(&mut vcx);

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.6");
        assert_eq!(h.mode(&vcx), "insert");

        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("0.6"),
            "the editor is untouched"
        );
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let expected = format!("update {} replaced 1 cell", local_hhmm(NEWER, clock));
        let (draft, notice) = h.tile.read_with(&vcx, |t, _| {
            (t.draft().clone(), t.notice().map(str::to_string))
        });
        assert!(
            draft.is_empty(),
            "the committed edit was replaced: {draft:?}"
        );
        assert_eq!(notice.as_deref(), Some(expected.as_str()), "and said so");

        h.dispatch(&mut vcx, "commit", None);
        let (len, base, cells, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().len(),
                t.draft().base.clone(),
                t.model().rows[0].cells[..2].to_vec(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(len, 1, "a fresh single edit");
        assert_eq!(base.as_deref(), Some(NEWER), "on the new base");
        assert!(!cells[0].edited, "the replaced edit is gone");
        assert_eq!(cells[1].text.to_string(), "0.6000");
        assert!(cells[1].edited);
        assert_eq!(notice, None, "the commit clears the notice");
        assert!(h.editor_value(&vcx).is_none());
    }

    /// The rule (command-line locality spec §2): every `:` verb the panel
    /// accepts changes only the panel — never the frame or the app.
    /// Checks the frame's three counters (`scope`, `grouping`, `as_of`),
    /// not any slot/scope/as-of *value* — a `save_slot` would still
    /// bump `grouping` (and `config`) and be caught that way even
    /// though nothing here reads what it wrote.
    #[gpui::test]
    fn every_colon_command_leaves_the_frame_alone(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let lines = [
            "underlying SPX",
            "revert",
            "bump 0.5",
            "rebase",
            "upload",
            "set spot 100",
            "auto hold",
            "menu",
        ];
        for word in crate::commands::VERBS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.versions());
        for line in lines {
            assert!(
                crate::commands::parse(line).is_ok(),
                "`{line}` no longer parses"
            );
            let _ = vcx.update(|window, cx| h.content.command(line, window, cx));
            let after = h.frame.read_with(&vcx, |f, _| f.versions());
            assert_eq!(
                (after.scope, after.grouping, after.as_of),
                (before.scope, before.grouping, before.as_of),
                "`:{line}` moved the frame"
            );
            let (level, overlay) = h.diagnostics.update(&mut vcx, |d, _| {
                (d.take_pending_level(), d.take_pending_overlay_toggle())
            });
            assert!(level.is_none() && !overlay, "`:{line}` reached the app");
            // Any per-line outcome is fine (a refused key, nothing to
            // revert); the rule is about what it did NOT touch.
        }
    }
    // ---- row verbs: o / shift+o / d d (dividend spec §5.3) ------------

    /// The model's row labels in painted order.
    fn row_labels(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
        h.tile.read_with(vcx, |t, _| {
            t.model().rows.iter().map(|r| r.label.to_string()).collect()
        })
    }

    fn notice_of(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<String> {
        h.tile.read_with(vcx, |t, _| t.notice().map(str::to_string))
    }

    /// §5.3 on a `Minted` axis: `o` inserts `new-1` below the cursor row
    /// and lands the cursor on its first cell in insert mode; `shift+o`
    /// on that inserted row chains a new row ABOVE it (the new row takes
    /// `new-1`'s anchor, `new-1` re-anchors onto it — controller ruling);
    /// `d d` on the inserted row drops it outright, on a document row
    /// marks it `Deleted`, and a second `d d` there says so.
    /// A hidden row label (user ruling 2026-09-20): the table carries no
    /// label column, so table column 0 is the first value column; `yy`
    /// yanks the cells alone; `/` searches the painted cells rather than
    /// the id nobody can see; `o` still mints an id for the draft and
    /// lands the cursor on the first cell.
    #[gpui::test]
    fn a_hidden_row_label_withholds_the_label_column(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::HIDDEN_SCHEDULE, None);
        h.with_flat_document(&mut vcx);
        assert_eq!(h.columns(&vcx), 3, "ex, amount, status — no label column");
        assert_eq!(
            h.headers(&vcx),
            vec!["ex", "amount", "status"],
            "the first header is the first VALUE column, not the row axis"
        );
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-cell-0-0").is_some(),
            "table column 0 is painted"
        );
        assert!(
            vcx.debug_bounds("marketdata-th-3").is_none(),
            "no fourth header"
        );
        // The cell at table column 0 is the FIRST VALUE (ex date), not an id.
        h.dispatch(&mut vcx, "yank", None);
        assert_eq!(clipboard(&mut vcx).as_deref(), Some("2026-12-18"));
        h.dispatch(&mut vcx, "yank_row", None);
        assert_eq!(
            clipboard(&mut vcx).as_deref(),
            Some("2026-12-18\t1.2500\tdeclared"),
            "the row is its cells alone, no id"
        );
        // `/` matches the painted cells (a status, a date), never the id.
        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("estimated".into()), window, cx)
        });
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 },
            "the second row's status matched"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("D2".into()), window, cx));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 },
            "the hidden id is not searchable"
        );
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        // `o` mints the id for the draft and lands on the first cell.
        h.dispatch(&mut vcx, "insert_below", None);
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field().is_some()));
    }

    #[gpui::test]
    fn o_inserts_a_minted_row_and_dd_deletes(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx); // rows D1, D2
        h.dispatch(&mut vcx, "insert_below", None);
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.date_field().is_some()),
            "the first cell (ex, a Date) opened its editor"
        );
        assert_eq!(h.mode(&vcx), "insert");
        draw(&mut vcx);
        type_keys(&mut vcx, "escape");
        assert_eq!(h.mode(&vcx), "normal");
        assert_eq!(
            row_labels(&h, &vcx),
            ["D1", "new-1", "D2"],
            "escape on a minted row's cell keeps the row"
        );
        h.dispatch(&mut vcx, "insert_above", None);
        vcx.run_until_parked();
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-2", "new-1", "D2"]);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        let anchors = h.tile.read_with(&vcx, |t, _| {
            let d = t.draft();
            let after = |l: &str| match d.row_state(l) {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            };
            (after("new-2"), after("new-1"))
        });
        assert_eq!(
            anchors,
            (Some("D1".to_string()), Some("new-2".to_string())),
            "new-2 took new-1's anchor and new-1 re-anchored onto new-2"
        );
        draw(&mut vcx);
        type_keys(&mut vcx, "escape");
        h.dispatch(&mut vcx, "delete_row", None);
        vcx.run_until_parked();
        assert_eq!(
            row_labels(&h, &vcx),
            ["D1", "new-1", "D2"],
            "an inserted row is dropped outright"
        );
        h.tile.update(&mut vcx, |t, cx| {
            t.cursor_to(2, Some(0), cx);
        });
        h.dispatch(&mut vcx, "delete_row", None);
        vcx.run_until_parked();
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.row_state_at(2)),
            Some(RowState::Deleted)
        );
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        h.dispatch(&mut vcx, "delete_row", None);
        assert_eq!(
            notice_of(&h, &vcx).as_deref(),
            Some("row is already deleted — :revert restores it")
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.header_dirty()));
    }

    /// §5.3 on a `Typed(Date)` axis (CVI): `o` inserts a provisional row
    /// and opens the ROW-LABEL editor on it — the segmented date field,
    /// painted in the row-label column; `enter` with a new term renames
    /// the row and opens the first cell; a term already present is
    /// refused with the editor open; `escape` drops the provisional row.
    #[gpui::test]
    fn o_on_a_typed_axis_opens_the_label_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx); // terms 2026-10-16, 2026-11-20
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.date_field().is_some()),
            "a Date axis opens the segmented field"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t
                .date_field()
                .is_some_and(|f| f.segments()[Segment::Day as usize].active)),
            "the row-label editor opens on the day segment"
        );
        assert_eq!(h.mode(&vcx), "insert");
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-editor-1-{LABEL_COL}").into_boxed_str()
            ))
            .is_some(),
            "painted in the row-label column of the new row"
        );
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-date-{TILE}").into_boxed_str()
            ))
            .is_some(),
            "the field is painted"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        // Type an existing term into the field (year, month, day digits —
        // the field's own `digit` grammar; it opens on the DAY segment).
        type_keys(&mut vcx, "left left 2 0 2 6 1 0 1 6");
        type_keys(&mut vcx, "enter");
        assert!(
            h.tile.read_with(&vcx, |t, _| t.label_editor_open()),
            "refused, still open"
        );
        assert_eq!(
            notice_of(&h, &vcx).as_deref(),
            Some("'2026-10-16' is already a row")
        );
        type_keys(&mut vcx, "left left 2 0 2 7 0 1 1 5");
        type_keys(&mut vcx, "enter");
        assert_eq!(
            row_labels(&h, &vcx),
            ["2026-10-16", "2027-01-15", "2026-11-20"],
            "renamed in place, under the row o was pressed on"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t
                .model()
                .rows
                .iter()
                .filter(|r| r.state == RowState::Inserted)
                .count()),
            1
        );
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.label_editor_open()),
            "the label editor closed"
        );
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("·"),
            "and the first cell's editor opened on the renamed row"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        h.dispatch(&mut vcx, "cancel", None);
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows_added()), 2);
        type_keys(&mut vcx, "escape");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().rows_added()),
            1,
            "escape dropped the provisional row"
        );
        assert_eq!(
            row_labels(&h, &vcx),
            ["2026-10-16", "2027-01-15", "2026-11-20"]
        );
        assert_eq!(h.mode(&vcx), "normal");
        assert!(
            vcx.update(|window, cx| window.focused(cx).is_none()),
            "the field gave the keyboard up"
        );
    }

    /// Two `o`s on the same document row of a `Typed(Date)` axis (final
    /// review, Critical): the second `o` re-hangs the first typed row
    /// onto the MINTED label, and `commit_row_label`'s rename must carry
    /// that follower with it — otherwise the first row's anchor names a
    /// label no row holds, the splice cannot place it, and it paints at
    /// the TOP of the grid. Painted order is `[D1, second, first, D2]`
    /// with `first.after == Some(second)`.
    #[gpui::test]
    fn a_second_o_on_the_same_row_keeps_the_first_typed_row_below_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx); // terms 2026-10-16, 2026-11-20
        // First insert: `o` on 2026-10-16, type 2027-01-15, enter, cancel
        // the first cell's editor the commit opened.
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        type_keys(&mut vcx, "left left 2 0 2 7 0 1 1 5");
        type_keys(&mut vcx, "enter");
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(
            row_labels(&h, &vcx),
            ["2026-10-16", "2027-01-15", "2026-11-20"]
        );
        // Back onto 2026-10-16 and insert again below it.
        h.dispatch(&mut vcx, "up", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        type_keys(&mut vcx, "left left 2 0 2 7 0 2 1 5");
        type_keys(&mut vcx, "enter");
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(
            row_labels(&h, &vcx),
            ["2026-10-16", "2027-02-15", "2027-01-15", "2026-11-20"],
            "the second row sits immediately below the cursor row, the first below it"
        );
        let after = h
            .tile
            .read_with(&vcx, |t, _| match t.draft().row_state("2027-01-15") {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            });
        assert_eq!(
            after.as_deref(),
            Some("2027-02-15"),
            "the first row hangs off the second's TYPED label, not the minted one"
        );
    }

    /// The three verbs are refused while `Behind` (with the standing
    /// notice) and in the strip ("not a row"), writing nothing.
    #[gpui::test]
    fn row_verbs_are_refused_while_behind_and_in_the_strip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let tag = h.with_document_tagged(&mut vcx);
        h.dispatch(&mut vcx, "up", None); // Attr(0)
        for verb in ["insert_below", "insert_above", "delete_row"] {
            h.dispatch(&mut vcx, verb, None);
            assert_eq!(notice_of(&h, &vcx).as_deref(), Some("not a row"), "{verb}");
            assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
            assert!(h.editor_value(&vcx).is_none());
        }
        h.dispatch(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        for verb in ["insert_below", "insert_above", "delete_row"] {
            h.dispatch(&mut vcx, verb, None);
            assert_eq!(
                notice_of(&h, &vcx).as_deref(),
                Some(BEHIND_REFUSED),
                "{verb}"
            );
            assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows.len()), 0);
            assert!(!h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        }
        assert_eq!(h.rows(&vcx), 2);
    }

    /// With no document (`:underlying` never named), the row verbs answer
    /// `NO_DOCUMENT` and open nothing.
    #[gpui::test]
    fn row_verbs_are_refused_without_a_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        for verb in ["insert_below", "insert_above", "delete_row"] {
            h.dispatch(&mut vcx, verb, None);
            assert_eq!(notice_of(&h, &vcx).as_deref(), Some(NO_DOCUMENT), "{verb}");
            assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
            assert_eq!(h.mode(&vcx), "normal");
        }
    }

    /// `o` with a cell editor open cancels it first (a row insert is
    /// navigation, never a commit — `set_key`'s own rule), and the new
    /// row's own editor is the one left open.
    #[gpui::test]
    fn o_cancels_an_open_editor_before_inserting(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "right", None); // amount
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.9");
        h.dispatch(&mut vcx, "insert_below", None);
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(
            h.cell(&vcx, 0, 1),
            ("1.2500".to_string(), false),
            "the abandoned edit was never committed"
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.date_field().is_some()),
            "the new row's first cell (a Date) is the open editor"
        );
    }

    /// Row edits ride the session under `[drafts.<key>]` and the parked
    /// map like any other unsent work (spec §8.5): `o`, fill a cell,
    /// serialize → `[drafts."SPX.Z".rows.new-1]`; restore → `rows_added`
    /// 1 with the cell in place; a key switch parks the rows and a switch
    /// back restores them. `yy` on the inserted row yanks its cells as
    /// painted.
    #[gpui::test]
    fn row_edits_ride_the_session_and_the_parked_map(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        // The first cell's date field is open: commit today's date, then
        // fill the amount.
        type_keys(&mut vcx, "enter");
        assert_eq!(h.mode(&vcx), "normal");
        h.dispatch(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "2.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.cell(&vcx, 1, 1), ("2.5000".to_string(), true));
        h.dispatch(&mut vcx, "yank_row", None);
        let yanked = clipboard(&mut vcx).expect("a yank");
        assert!(
            yanked.starts_with("new-1\t") && yanked.ends_with("\t2.5000\t·"),
            "{yanked:?}"
        );

        let written = h.tile.read_with(&vcx, |t, _| t.serialize());
        let rows = written["drafts"]["SPX.Z"]["rows"]
            .as_table()
            .expect("a rows table");
        assert_eq!(rows["new-1"]["after"].as_str(), Some("D1"));
        assert_eq!(rows["new-1"]["cells"]["amount"].as_float(), Some(2.5));

        // A key switch parks the rows; a switch back restores them on the
        // next delivery.
        h.command(&mut vcx, "underlying NKY.Z").unwrap();
        let _nky = h.document_request().expect("NKY's own request");
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.parked()),
            vec![("SPX.Z".to_string(), "1 row added".to_string())]
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows_added()), 0);
        h.command(&mut vcx, "underlying SPX.Z").unwrap();
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "estimated"),
            ])),
        );
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(h.cell(&vcx, 1, 1), ("2.5000".to_string(), true));

        // A restart from the written session.
        let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(written));
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "estimated"),
            ])),
        );
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows_added()), 1);
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-1", "D2"]);
        assert_eq!(h.cell(&vcx, 1, 1), ("2.5000".to_string(), true));
    }

    /// `o` on a row that already has a direct follower puts the new row
    /// IMMEDIATELY below it (controller ruling): the existing follower is
    /// re-hung onto the new row rather than left as a label-ordered
    /// sibling — `o` on `D1` twice paints `D1, new-2, new-1`, never
    /// `D1, new-1, new-2`, and a later rename of either cannot re-sort
    /// the pair on commit.
    #[gpui::test]
    fn o_rehangs_the_existing_follower_onto_the_new_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        type_keys(&mut vcx, "escape");
        h.dispatch(&mut vcx, "up", None); // back on D1
        h.dispatch(&mut vcx, "insert_below", None);
        assert_eq!(row_labels(&h, &vcx), ["D1", "new-2", "new-1", "D2"]);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        let anchors = h.tile.read_with(&vcx, |t, _| {
            let d = t.draft();
            let after = |l: &str| match d.row_state(l) {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            };
            (after("new-2"), after("new-1"))
        });
        assert_eq!(
            anchors,
            (Some("D1".to_string()), Some("new-2".to_string())),
            "new-2 hangs off D1 and new-1 was re-hung onto new-2"
        );
    }

    /// A `Typed(I64)` axis (the `LADDER` fixture) opens the TEXT row-label
    /// editor on `o`: a label already present is refused with the editor
    /// open, a non-integer is refused the same way, a new label commits
    /// through `parse_attr` → `attr_text` (`" 007 "` names the row `7`)
    /// and opens the first cell, `up` in the field nudges the integer,
    /// and `escape` drops the provisional row.
    #[gpui::test]
    fn o_on_an_integer_axis_opens_the_text_label_editor(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::LADDER, None);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::ladder_snapshot(&[(100, 0.2), (110, 0.19)])),
        );
        assert_eq!(row_labels(&h, &vcx), ["100", "110"]);
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.date_field().is_none()),
            "an integer axis opens no date field"
        );
        assert_eq!(h.editor_value(&vcx).as_deref(), Some(""), "seeded blank");
        assert_eq!(h.mode(&vcx), "insert");
        assert!(
            vcx.debug_bounds(Box::leak(
                format!("marketdata-editor-1-{LABEL_COL}").into_boxed_str()
            ))
            .is_some(),
            "painted in the row-label column"
        );
        h.set_editor(&mut vcx, "110");
        h.dispatch(&mut vcx, "commit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        assert_eq!(
            notice_of(&h, &vcx).as_deref(),
            Some("'110' is already a row")
        );
        h.set_editor(&mut vcx, "abc");
        h.dispatch(&mut vcx, "commit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        assert_eq!(
            notice_of(&h, &vcx).as_deref(),
            Some("'abc' is not a whole number")
        );
        // The insert-mode arrows nudge the typed integer in place.
        h.set_editor(&mut vcx, "5");
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("6"));
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("-4"));
        h.set_editor(&mut vcx, " 007 ");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(
            row_labels(&h, &vcx),
            ["100", "7", "110"],
            "the canonical spelling is the label"
        );
        assert!(!h.tile.read_with(&vcx, |t, _| t.label_editor_open()));
        assert_eq!(
            h.editor_value(&vcx).as_deref(),
            Some("·"),
            "the first cell's editor opened on the renamed row"
        );
        assert!(matches!(
            h.tile
                .read_with(&vcx, |t, _| t.draft().row_state("7").cloned()),
            Some(RowEdit::Inserted { .. })
        ));
        h.dispatch(&mut vcx, "cancel", None);
        h.dispatch(&mut vcx, "insert_below", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().rows_added()), 2);
        h.dispatch(&mut vcx, "cancel", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().rows_added()),
            1,
            "escape dropped the provisional row"
        );
        assert_eq!(row_labels(&h, &vcx), ["100", "7", "110"]);
        assert_eq!(h.mode(&vcx), "normal");
    }
    fn publish_document_for(
        h: &Harness,
        vcx: &mut gpui::VisualTestContext,
        dataset: &str,
        batch: &str,
    ) {
        h.frame.update(vcx, |f, cx| {
            f.note_published(Publish {
                dataset: dataset.into(),
                batch: batch.into(),
                books: 0,
                at: chrono::Utc::now(),
            });
            cx.notify();
        });
    }

    #[gpui::test]
    fn publications_only_requery_the_selected_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap();
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        for _ in 0..64 {
            publish_document_for(&h, &mut vcx, "cvi_params", "NDX.Z");
            publish_document_for(&h, &mut vcx, "other", "SPX.Z");
            assert!(h.document_request().is_none());
        }
        publish_document_for(&h, &mut vcx, "cvi_params", "SPX.Z");
        assert!(h.document_request().unwrap().tag > first.tag);
        h.command(&mut vcx, "key NDX.Z").unwrap();
        h.document_request().unwrap();
        publish_document_for(&h, &mut vcx, "cvi_params", "SPX.Z");
        assert!(h.document_request().is_none());
        publish_document_for(&h, &mut vcx, "cvi_params", "NDX.Z");
        assert!(
            h.document_request().is_some(),
            "new document is watched before any rows arrive"
        );
    }

    #[gpui::test]
    fn unrelated_documents_cannot_release_a_barrier_or_discard_a_valid_stage(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let tag = h.document_request().unwrap().tag;
        publish_document_for(&h, &mut vcx, "cvi_params", "NDX.Z");
        assert!(h.document_request().is_none());
        assert!(
            h.frame
                .read_with(&vcx, |f, _| f.barrier_wants(QueryKey(TILE), f.versions())),
            "the real query is still in flight"
        );
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(h.rows(&vcx), 2, "staged");
        publish_document_for(&h, &mut vcx, "other", "SPX.Z");
        assert!(h.document_request().is_none());
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            f.arrived(other, now);
            cx.notify();
        });
        assert_eq!(
            h.rows(&vcx),
            5,
            "the unrelated publication cannot invalidate staged rows"
        );
    }
}
