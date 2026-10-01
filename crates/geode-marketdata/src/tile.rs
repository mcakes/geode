//! Market-data document tile: requests one dataset/key/as-of through DataHandle and
//! owns its MatrixIndex, draft, cursor, clipboard, find, and commands.
//!
//! The tile's cursor is authoritative; MatrixDelegate mirrors it. Every model
//! installation refreshes TableState's cached column groups so changed document axes
//! appear in the table header.
//!
//! Requests follow as-of and publication versions for this dataset and key. Scope,
//! grouping, and flip changes do not trigger document queries. The tile still answers
//! flip barriers: deliveries can wait for a coordinated promotion, while changes
//! requiring no query are acknowledged directly. Promotion checks the followed
//! versions, so an unrelated barrier replacement cannot discard the only staged answer
//! to the current request.
//!
//! Editors commit typed values into Draft. With a newer generation held Behind, the
//! tile retains the actual base snapshot when available. Rebase maps edits by labels
//! onto the newer document and reports drops; revert discards edits and shows the
//! newest usable snapshot. Hold/Rebase/Replace policies govern new live generations,
//! with restore and Sent-echo handling taking precedence. Behind drafts and differing
//! upload echoes refuse further edits until resolved.

use crate::commands::{self, BumpAxis, Command, KEY_DISPLAY_SEPARATOR};
use crate::core::cursor::{self, Cursor, Grid};
use crate::core::draft::{RowDelete, RowEdit, local_hhmm};
use crate::core::matrix::{RowState, base_of};
use crate::core::menu::{self, MenuInputs};
use crate::core::spec::RowIdentity;
use crate::core::{
    CellKind, Columns, DateTimeField, DocumentBase, Draft, DraftBadge, DraftState, FieldKey,
    MatrixIndex, PanelSpec, Precision, Segment, SegmentText, UpdatePolicy, attr_text, parse_attr,
    parse_cell, route,
};
use crate::delegate::{
    CellPointer, DelegateChoice, DelegateEditor, DelegateEditorPaint, MatrixDelegate,
};
use crate::header::{self, HeaderInputs, HeaderModel, Tone};
use crate::popup::{ChoicePopup, PickerRows, PickerState, Popup, render_picker};
use geode_core::colour::{Rgb, contrast_ratio, readable_on};
use geode_core::document::{DocumentRows, Value, split_key};
use geode_core::grid::selection::{Resolved, SelectKind, Selection};
use geode_core::query::{DocumentParams, QueryKey, QueryOutcome};
use geode_core::schema::ColumnType;
use geode_core::snapshot::Snapshot;
use geode_data::DataHandle;
use geode_shell::actions::ActionId;
use geode_shell::colfit::{
    FitMetrics, FittedWidths, NOTHING_TO_FIT, SESSION_KEY, widths_from_record, widths_to_toml,
};
use geode_shell::diagnostics::Diagnostics;
use geode_shell::frame::{FrameRef, FrameVersions, PublicationWatch};
use geode_shell::keymap::{Binding, KeyContext};
use geode_shell::linenumbers::{LineNumbers, UiSettings};
use geode_shell::module::{FindEvent, StackHandle, UploadDelivery};
use geode_shell::shell::aggregates;
use geode_shell::shell::colours::{to_hsla, to_rgb};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::vimfind::{FindDirection, find_match};
use geode_shell::vimnav::NavCommand;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::edit::EditCaret;
use geode_tile::following::{Delivered, FollowingQuery, FrameDoor, Promotion, Unanswered};
use geode_tile::header::HealthWatch;
use geode_tile::menu::{Menu, MenuHost, MenuIds};
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, Entity, FocusHandle, Focusable as _, Hsla, IntoElement,
    KeyDownEvent, SharedString, Window, div,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::table::{DataTable, TableDelegate as _, TableEvent, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, Theme, h_flex, v_flex};
use std::cell::{Cell as StdCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod select;
use select::{FINISH_EDIT_FIRST, StepsUndo};

/// The selection footer's height in design px — the blotter's, pricer's
/// and timeseries' footer value, so every grid's strip reads alike.
const FOOTER_HEIGHT: f32 = 20.0;

/// Find moves through visible row text without filtering the document axis. Row labels
/// are searched when painted; panels hiding those labels search their painted cells
/// instead. Escape restores the full starting cursor.
pub struct FindState {
    /// Full cursor captured when find starts. Escape can restore either a grid cell or
    /// an attribute-strip position.
    origin: Cursor,
    /// The last committed query, for `n`/`N`.
    committed: Option<String>,
}

/// Cached contrast-adjusted header tones. Warning text is adjusted against the window
/// background toward the theme foreground (danger text is the notice door's). The date field's active-segment
/// text is adjusted against primary toward whichever of black or white contrasts more,
/// allowing even a matching text/background pair to separate.
///
/// The five-input theme signature covers every colour read by derive. Refresh runs the
/// contrast calculation only when that signature changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FlooredTones {
    key: [Hsla; 5],
    pub(crate) warn: Hsla,
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
            primary_text: to_hsla(readable_on(
                to_rgb(theme.primary_foreground),
                primary,
                toward,
            )),
        }
    }

    fn key(theme: &Theme) -> [Hsla; 5] {
        [
            theme.background,
            theme.foreground,
            theme.warning,
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

/// Refusal when editing has no displayed document and therefore no cell or source
/// generation to attach an edit to.
const NO_DOCUMENT: &str = "no document to edit";

/// What a commit answers when the grid moved under the open editor — see
/// [`Editing::labels`] for how that happens and why it is refused.
const CELL_MOVED: &str = "the document changed under the edit — nothing was written";

/// Refusal for a Behind draft. The painted base is older than the held document; rebase
/// or revert must resolve that difference before further edits.
const BEHIND_REFUSED: &str = "the draft is behind — :rebase or :revert first";

/// Refusal while a differing upload echo is held. The sent edits still cover their
/// base; rebase or revert resolves the newer upstream document first.
const ECHO_REFUSED: &str = "the echo differs — :rebase or :revert first";

/// Refusal for editing a row marked Deleted. It remains visible, but new values would
/// target a row scheduled for removal. Revert restores the draft's rows.
pub(crate) const DELETED_REFUSED: &str = "row is deleted — :revert restores it";

/// Upload refusal while Behind: the displayed draft is based on an older document.
const UPLOAD_BEHIND: &str =
    "rebase or revert first: an upload must be of a document you have seen whole";

/// The notice every non-`y` answer to the confirm leaves — a key, focus
/// leaving the confirm, or a pointer press on the tile.
const UPLOAD_CANCELLED: &str = "upload cancelled";

/// The notice when a delivery moves the painted generation or the draft
/// while the confirm stands: the prompt's document is no longer the one
/// on screen, so the question is withdrawn at once rather than left
/// painted until the next key.
const UPLOAD_CANCELLED_ARRIVED: &str = "upload cancelled: a new document arrived";

/// What an armed upload confirm asks to send: the rows assembled when it
/// was armed, so the prompt's counts and the payload describe one document.
/// The prompt, its focus and its blur answer are the confirm door's.
/// Public (its fields are not) because it is the payload of the public
/// tile's `ConfirmHost` impl, which cannot name a crate-private type.
pub struct PendingUpload {
    target: String,
    rows: DocumentRows,
    /// The draft as it was when the confirm was armed. A delivery can
    /// land between the prompt and the answer (`Behind`, or a `replace`
    /// policy dropping the edits); `y` against a draft that no longer
    /// matches would send a document the trader is no longer looking at,
    /// so it cancels instead. Compared WHOLE — `base` included: an
    /// `:auto rebase` onto a same-shape newer generation leaves edits,
    /// attrs, rows and state equal while the rows were assembled from
    /// the superseded base.
    draft: Draft,
}

/// The upload submitted and not yet answered: which underlying it sent
/// and where. Kept across a key switch — the outcome still has to be
/// reported, and naming the key it sent is what stops an answer for SPX
/// from being read as NDX's.
struct InFlightUpload {
    key: Vec<String>,
    target: String,
}

/// Upload echo result in a header slot separate from transient errors. Matching echo
/// confirmation survives painting deliveries until the next edit; a differing echo
/// remains associated with its Sent draft.
#[derive(Debug, Clone)]
enum Echo {
    /// `sent HH:MM, confirmed HH:MM` — the draft has cleared; dropped by
    /// [`MarketDataTile::rebuild_chrome`] at the next edit.
    Confirmed(SharedString),
    /// The compared delivery's full base and the difference notice. Keep the
    /// Sent draft over its base and reuse this verdict only for an exactly equal
    /// `DocumentBase`. A changed generation, including known versus unknown,
    /// requires another comparison. Clear the verdict when the draft leaves Sent.
    Differs {
        newer: DocumentBase,
        text: SharedString,
    },
}

/// What [`MarketDataTile::echo_of`] decided about one delivery.
enum EchoStep {
    /// Not an echo: the draft is not `Sent`, or this is its own base.
    None,
    /// The delivered document is what was sent; the draft has reverted.
    Confirmed(SharedString),
    /// It is not (or could not be compared); the draft stays `Sent`
    /// over its base.
    Held(Echo),
    /// `Sent` with nothing to compare against: the draft went `Behind`.
    Unchecked,
}

/// What `:rebase`/`:revert` answer outside `Behind` — there is no
/// "newer" document to move onto or fall back to.
const NOT_BEHIND: &str = "nothing to rebase — the draft is on the live document";

/// Refuse rebase while Sent has no differing echo. Rebasing onto the same base would
/// make already-uploaded edits sendable again. Revert can explicitly drop the draft and
/// stop awaiting its echo.
const REBASE_AWAITING_ECHO: &str =
    "nothing newer to rebase onto — the upload is awaiting its echo; :revert to drop it";

/// Row-operation refusal when the cursor is in the attribute strip.
const NOT_A_ROW: &str = "not a row";

/// Refusal for deleting an already-Deleted row; revert is the recovery route.
const ALREADY_DELETED: &str = "row is already deleted — :revert restores it";

/// Open cell or attribute editor, including its input and original target.
struct Editing {
    state: EditorState,
    target: EditTarget,
    /// Set only on a text editor opened on a number cursor cell over a
    /// live selection.
    bulk: Option<Bulk>,
    /// The text the editor opened on: its seed, or a date field's painted
    /// date. Over a selection, a commit that still holds it (and a date
    /// field no digit was typed into) writes nothing — a no-op `enter`
    /// must never copy one cell's value across the selection.
    opened: String,
    /// A digit or backspace reached the date field since it opened, so a
    /// retyped same date is a deliberate write, not a no-op.
    typed: bool,
}

/// A text editor opened on a number cursor cell over a live selection
/// (any other cursor cell commits absolutely). While its text is
/// untouched, arrows step every selected number in the draft at once,
/// so the grid shows the steps as they are made; closing the editor any
/// way but a commit puts `before` back (while the steps are still the
/// draft's last change and the painted base has not moved; see
/// `undo_steps`), so a trader who escapes never leaves half a block
/// stepped.
struct Bulk {
    /// The draft when `i` opened the editor.
    before: Draft,
    /// The draft right after the last step landed (`before` until one
    /// does). A draft whose work no longer matches it was written by
    /// something else while the editor was open (a palette revert, a
    /// `:set`, a replace policy, a single-cell commit once the selection
    /// cleared), and restoring `before` over that would silently undo it.
    after: Draft,
    /// The generation painted then. An automatic rebase or replace while
    /// the editor is open moves it, and `before` is then keyed to a grid
    /// that is no longer painted: restoring it would put edits on the
    /// wrong cells, so the steps are kept instead.
    painted: Option<DocumentBase>,
    /// The text the tile last put in the editor. Any other value means
    /// the trader typed, which turns the edit absolute.
    seeded: String,
    /// Signed steps since `i`, for the notice.
    steps: i64,
    /// Whether any step reached the draft, so a close must undo it.
    stepped: bool,
    /// The upload state `i` found beside `before`; see [`UploadMarks`].
    upload: UploadMarks,
}

/// What the upload and echo machinery holds about the draft, which
/// `rebuild_chrome` lets go of once a step makes the draft `Editing`: the
/// rows a `Sent` draft's echo is compared with, the echo line, and a
/// failed upload's error. Restoring `before` without them would bring
/// back `Sent` with nothing to compare, so a matching echo would land as
/// `Behind` instead of confirming. `submitted` is not here: a step never
/// drops it (only the outcome, a refused submit or a key switch does),
/// and bringing back one the outcome consumed would resurrect a question
/// already answered.
struct UploadMarks {
    sent: Option<DocumentRows>,
    echo: Option<Echo>,
    upload_error: Option<(SharedString, Draft)>,
}

/// Text and segmented-date editor states, chosen by cell kind or attribute type.
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
        /// than a cancel-on-delivery path (which promotion could not
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
    /// Typed-axis row-label editor over a provisionally inserted row. Commit renames
    /// its minted label to the validated typed label.
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

/// Clipboard operation: cell, row, or column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Yank {
    Cell,
    Row,
    Col,
}

pub struct MarketDataTile {
    id: TileId,
    spec: Arc<PanelSpec>,
    frame: FrameRef,
    diagnostics: Entity<Diagnostics>,
    data: DataHandle,
    /// The document key, in the dataset's declared `key` order. The
    /// trader-facing word is "underlying" (`:underlying <value>`); the data
    /// tier's word is "key" (the field name and `DocumentParams.document_key`).
    /// `None` until `:underlying` names one — a fresh panel has no document
    /// to ask about, and guessing one would paint a document the trader never
    /// asked for.
    key: Option<Vec<String>>,
    /// This panel's document request under the flip barrier (see
    /// `geode_tile::following`): the versions it last asked under (whole,
    /// though only `as_of` and `data` decide a requery; the barrier is keyed
    /// by flip identity), whether it is out, the answer held for the barrier,
    /// the last flip seen, and the request tag.
    following: FollowingQuery<Arc<Snapshot>>,
    /// Resolved upload targets whose documents include this panel's document kind, in
    /// egress.toml order. Captured at construction because egress configuration
    /// requires restart; upload target resolution and completions use this list.
    egress_targets: Vec<SharedString>,
    publication: Option<PublicationWatch>,
    visible: bool,
    /// Newest accepted delivered snapshot, retained for rebase even when a Behind draft
    /// continues painting base_snapshot.
    snapshot: Option<Arc<Snapshot>>,
    /// Actual draft base retained when a newer generation arrives. A restored draft
    /// whose base was never delivered has no retained base; it paints against the
    /// newest snapshot while preserving Behind state until resolved.
    base_snapshot: Option<Arc<Snapshot>>,
    /// The whole-document index, shared with the delegate through Rc. Rebuilds
    /// replace it through install_model, which refills the delegate's window; a
    /// one-cell commit leaves it alone and refills that one window cell.
    model: Rc<MatrixIndex>,
    draft: Draft,
    /// What `/` searches, one string per row, prepared on first use and dropped
    /// by every install; a one-cell edit re-prepares its row under a hidden label.
    search_text: Option<Vec<String>>,
    /// How many times `search_text` was prepared, for tests that pin it to
    /// once per index build.
    #[cfg(test)]
    search_builds: usize,
    /// The last `/` open's result cells, for tests to read.
    #[cfg(test)]
    find_cells: Option<Rc<RefCell<crate::delegate::FindCells>>>,
    /// Policy for new live generations with edits: Hold, Rebase, or Replace. Changing
    /// it does not resolve an already-Behind draft or act on a redelivery. Commands,
    /// menu rows, and actions share the setter; nondefault policy persists in the
    /// session. Restore and Sent-echo rules take precedence.
    policy: UpdatePolicy,
    /// Draft edits restored as label pairs await a successfully built nonempty model
    /// before resolving grid positions. Set on session restore or switching to a parked
    /// nonempty draft; cleared only after resolution. The first usable restore delivery
    /// follows Hold regardless of the live update policy.
    unresolved_restore: bool,
    /// Other keys' drafts parked as portable TOML label-pair tables. Switching back
    /// removes and restores that entry. Parked state holds no grid, snapshot, or
    /// cursor; parsing occurs when needed for restore, picker marks, or serialization.
    /// Draft commands and the header dirty marker describe only the current key.
    parked: BTreeMap<Vec<String>, toml::Table>,
    /// Authoritative cursor: a grid cell or header attribute. Table selection mirrors
    /// it.
    cursor: Cursor,
    /// The grid column the cursor left from on entering the strip —
    /// `cursor::step`'s own memory, kept here because the tile is what
    /// owns the cursor across motions (`j` reads it back to return to the
    /// same column; `0` before the strip has ever been entered).
    last_grid_col: usize,
    /// A live `V`/`v` selection, anchored by row label and column label so
    /// it survives a redelivery, an inserted row or a rebase. `None` in
    /// the ordinary cursor-only state.
    selection: Option<Selection<SharedString, SharedString>>,
    /// `selection` re-resolved against the current model and cursor by
    /// `refresh_selection` — what the delegate's tint and every
    /// selection-wide verb read, so painting never resolves anything.
    resolved: Option<Resolved>,
    /// The footer's `"{rows} rows × {cols} cols"` readout while a
    /// selection is live; `None` otherwise. Prepared with `resolved` so
    /// render formats nothing.
    selection_extent: Option<SharedString>,
    /// The body: gpui-component's table over [`MatrixDelegate`]. Never
    /// focused (see `geode_marketdata::init`, which binds its context's
    /// keys to `NoAction` for the one frame a click gives it gpui focus).
    table: Entity<TableState<MatrixDelegate>>,
    /// Open text or date editor. Commit/cancel close it through close_editor, which
    /// blurs its input only if it owns focus before releasing it. Picker, choice, and
    /// confirmation state can also select insert mode.
    editor: Option<Editing>,
    find: Option<FindState>,
    fuzzy_find: Option<gpui::WeakEntity<geode_shell::fuzzyfind::FuzzyFind>>,
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
    /// The header's floored tone colours, refreshed at the top of `render`
    /// (see [`FlooredTones`]).
    tones: FlooredTones,
    /// Optional tile-local action menu, underlying picker, or cell choice popup.
    popup: Option<Popup>,
    /// The `⋯` button's tooltip selector (`"tip-marketdata-menu-button-
    /// {id}"`), built once here — it depends only on the tile id, never
    /// per render — and passed into [`header::render`].
    menu_tip_selector: SharedString,
    /// The `Behind` state run's tooltip selector (`"tip-marketdata-
    /// state-{id}"`), built once alongside `menu_tip_selector`.
    state_tip_selector: SharedString,
    /// The `⋯` button's debug selector (`"marketdata-menu-button-{id}"`),
    /// built once so the header's paint formats nothing.
    menu_selector: SharedString,
    /// The header's health half: the panel's dataset, re-asked when source
    /// health or descriptions move.
    health: HealthWatch,
    /// The action menu's element names, prepared once from the tile id.
    menu_ids: MenuIds,
    /// The keymap as last published, for the menu's key hints.
    chords: Arc<Vec<Binding>>,
    /// Stack membership rendered in the header; None outside a stack.
    stack: Option<StackHandle>,
    /// Prepared title from the panel title and key. Updated by set_key so title()
    /// returns cached text without formatting.
    title: SharedString,
    /// Clock copied from AppClock at construction and refreshed by its observer.
    /// Prepared header and menu formatting receive it as an explicit input.
    pub(crate) clock: geode_core::clock::Clock,
    /// Armed upload confirmation, if any.
    pending_upload: Option<Confirm<PendingUpload>>,
    /// Rows of the latest submitted upload, retained while submitted or Sent. Rebuild
    /// drops them once neither state applies, ending comparison after refusal, failure,
    /// draft changes, revert, rebase, or a matching echo. Only Sent compares new
    /// generations against these rows.
    sent: Option<DocumentRows>,
    /// What the last echo said; see [`Echo`].
    echo: Option<Echo>,
    /// The draft as it was when the last upload was submitted. An `Ok`
    /// outcome enters `Sent` only while the draft still matches it: an
    /// edit made while the upload was in flight is unsent work, and
    /// painting it as sent would claim a value went upstream that did
    /// not. Dropped on a key switch: the draft it describes is parked
    /// and gives up its echo check.
    submitted: Option<Draft>,
    /// The upload awaiting its outcome, if any. `:upload` is refused while
    /// one is (a second submission would race the first's echo), and an
    /// outcome for a key no longer shown is a notice that never touches
    /// the current draft.
    in_flight: Option<InFlightUpload>,
    /// This tile's upload counter, echoed in the outcome; an outcome whose
    /// tag is not the latest is ignored.
    upload_tag: u64,
    /// `upload failed: <e>`, painted in the header until the next edit or
    /// upload, with the draft it failed on — the "next edit" is any
    /// change to the draft's edits, attributes or rows, which
    /// `rebuild_chrome` compares against, so no edit door has to
    /// remember to clear it.
    upload_error: Option<(SharedString, Draft)>,
}

impl MarketDataTile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: TileId,
        spec: Arc<PanelSpec>,
        frame: FrameRef,
        diagnostics: Entity<Diagnostics>,
        data: DataHandle,
        stale_after: Rc<StdCell<Duration>>,
        egress_targets: Vec<SharedString>,
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
        // Restore per-key drafts from [drafts.<display key>]. Install the current key's
        // entry and leave the others parked. The legacy draft field supplies the
        // current draft only when the per-key table has no entry for it.
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
        // `[ui] line_numbers` arrives through the shell's `UiSettings`
        // global, read here and observed below (`on_ui_settings`).
        let line_numbers = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
        let table = cx.new(|cx| {
            let mut delegate = MatrixDelegate::new(&spec, weak_tile, id.0, tones);
            delegate.line_numbers = line_numbers;
            // A missing or garbled record is an empty map, never a refusal.
            delegate.fitted = widths_from_record(restored);
            TableState::new(delegate, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(true)
                .row_header(false)
                .loop_selection(false)
                .col_resizable(false)
                .col_movable(false)
                .sortable(false)
        });
        // SelectCell moves the cursor; row-label clicks preserve its grid column. The
        // second press emits SelectCell before DoubleClickedCell, so selection cancels
        // any prior editor before double-click opens the new cell editor. Row-label
        // double-clicks have no value cell to edit.
        //
        // Ignore SelectRow/SelectColumn: sync_cursor emits them while mirroring state,
        // and handling them here would reenter selection. Window access permits input
        // blur and focus; the shell preserves a focused insert-mode field across
        // render.
        cx.subscribe_in(&table, window, |this, _, event: &TableEvent, window, cx| {
            match event {
                TableEvent::SelectCell(row, col) => {
                    let col = this.table.read(cx).delegate().model_col(*col);
                    // A click inside the open editor's own cell is the
                    // editor's (caret, text selection, a date separator):
                    // never a cancel, and the cursor is already there.
                    if this.editor_cell() == Some((*row, col)) {
                        return;
                    }
                    // Cancel and blur an open editor before moving the cursor. Clicking
                    // another cell does not commit partially typed text.
                    if this.editor.is_some() {
                        this.close_editor(window, cx);
                        // `cancel`'s own chrome step in `dispatch`: the
                        // header is re-prepared once, off the render
                        // thread.
                        this.changed(cx);
                    }
                    this.cursor_to(*row, col, cx)
                }
                TableEvent::DoubleClickedCell(row, col) => {
                    // A double-click inside the open editor reopens
                    // nothing: `begin_edit` refuses while one is open.
                    if let Some(col) = this.table.read(cx).delegate().model_col(*col) {
                        this.cursor_to(*row, Some(col), cx);
                        this.begin_edit(EditCaret::End, window, cx);
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
        // Shift+click and drag: the delegate's own pointer events, which
        // reach `pointer` ahead of the table's `SelectCell` (that one is
        // emitted on the release). Window access for the editor's blur.
        cx.subscribe_in(
            &table,
            window,
            |this, _, event: &CellPointer, window, cx| this.pointer(*event, window, cx),
        )
        .detach();
        cx.observe(frame.entity(), |this, _frame, cx| {
            // Promote before the visibility check, so a panel hidden after
            // staging still lands its answer. A flip releases prepared
            // results; it never triggers a document query.
            let now = this.versions(cx);
            let promoted = this.following.on_flip(now, Self::differs_on_followed);
            if let Promotion::Apply(snapshot) = promoted {
                this.apply(snapshot, cx);
                this.changed(cx);
            }
            if !this.visible {
                return;
            }
            // Only `as_of` and `data` are followed (see the module doc);
            // a scope keystroke bumps `scope` on every character and must
            // not cost this panel a requery.
            if this.key.is_some()
                && this
                    .following
                    .follows_changed(now, Self::differs_on_followed)
            {
                // The barrier is answered on delivery instead, with the
                // versions this request was made under.
                this.requery(cx);
            } else {
                let key = QueryKey(this.id.0);
                this.following
                    .self_arrive(&mut FrameDoor::new(&this.frame, cx), key, now);
            }
        })
        .detach();
        cx.observe(&diagnostics, |this, _diagnostics, cx| {
            // Health first: the picker gate below returns on every other
            // notification.
            let dataset = this.spec.dataset.as_str();
            if this
                .health
                .refresh(cx, |d| d.health_for_datasets(&[dataset]))
            {
                cx.notify();
            }
            // Refresh prepared picker rows only while the picker is open. Command
            // completions read the catalog directly, and opening requests a fresh
            // catalog.
            if !matches!(this.popup, Some(Popup::Picker(_))) {
                return;
            }
            let all = this.catalog_keys(cx);
            let Some(Popup::Picker(p)) = &mut this.popup else {
                return;
            };
            // Diagnostics also notifies for health changes. Compare keys first so
            // unrelated notifications neither reset picker selection nor copy the same
            // catalog.
            if p.rows.all() == all.as_slice() {
                return;
            }
            // Preserve the highlighted key string across catalog replacement and
            // sorting.
            p.rows.replace_all(all);
            cx.notify();
        })
        .detach();
        // Observe line-number settings so the delegate and cached widths stay in sync.
        cx.observe_global::<UiSettings>(|this, cx| this.on_ui_settings(cx))
            .detach();
        // Refresh the cached clock and prepared time labels when AppClock changes.
        cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| {
            this.clock = cx
                .try_global::<geode_shell::clock::AppClock>()
                .map(|c| c.0)
                .unwrap_or_else(|| geode_core::clock::Clock::machine().0);
            this.rebuild_chrome();
            cx.notify();
        })
        .detach();
        // A keymap reload re-resolves an open menu's hints at once.
        cx.observe_global::<geode_shell::tips::Chords>(|this, cx| {
            this.chords = geode_tile::menu::live_bindings(cx);
            if let Some(Popup::Menu(m)) = &mut this.popup {
                m.rehint(&this.chords);
                cx.notify();
            }
        })
        .detach();

        let model = Rc::new(MatrixIndex::empty(&spec, key.as_deref().unwrap_or(&[])));
        let title = Self::compute_title(&spec, key.as_deref());
        // Asked once now, so a panel opened after a failure shows the chip
        // before any further diagnostics notification.
        let mut health = HealthWatch::new(diagnostics.clone(), id);
        health.reask(cx, |d| d.health_for_datasets(&[spec.dataset.as_str()]));
        let mut this = MarketDataTile {
            id,
            spec,
            frame,
            diagnostics,
            data,
            model,
            title,
            unresolved_restore: !draft.is_empty(),
            parked,
            key,
            following: FollowingQuery::new(),
            egress_targets,
            publication: None,
            visible: false,
            snapshot: None,
            base_snapshot: None,
            draft,
            search_text: None,
            #[cfg(test)]
            search_builds: 0,
            #[cfg(test)]
            find_cells: None,
            policy,
            cursor: Cursor::Cell { row: 0, col: 0 },
            last_grid_col: 0,
            selection: None,
            resolved: None,
            selection_extent: None,
            table,
            editor: None,
            find: None,
            fuzzy_find: None,
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
                upload_error: None,
                echo: None,
                prompt: None,
                time: None,
                time_stale: None,
                stale: false,
            },
            source_at: None,
            tones,
            popup: None,
            menu_tip_selector: format!("tip-marketdata-menu-button-{}", id.0).into(),
            state_tip_selector: format!("tip-marketdata-state-{}", id.0).into(),
            menu_selector: format!("marketdata-menu-button-{}", id.0).into(),
            health,
            menu_ids: MenuIds::new(
                format!("marketdata-menu-{}", id.0),
                format!("marketdata-menu-row-{}", id.0),
            ),
            chords: geode_tile::menu::live_bindings(cx),
            stack: None,
            clock: cx
                .try_global::<geode_shell::clock::AppClock>()
                .map(|c| c.0)
                .unwrap_or_else(|| geode_core::clock::Clock::machine().0),
            pending_upload: None,
            sent: None,
            echo: None,
            submitted: None,
            in_flight: None,
            upload_tag: 0,
            upload_error: None,
        };
        this.rebuild_chrome();
        // Install the tile's initial model into the delegate through the same path as
        // every later model replacement.
        this.install_model(cx);
        this
    }

    /// Keymap mode while focused: insert for an editor, picker, choice field, or upload
    /// confirmation; menu for the action list; normal otherwise. Mode reports open
    /// state, while holds_focus reports actual keyboard ownership.
    ///
    /// Insert contexts retain shell chords. Bare arrows route through insert_up/down to
    /// the active field's navigation or nudge behavior; the shell prevents typed digits
    /// from becoming count prefixes despite counts() remaining enabled.
    pub fn key_context(&self) -> KeyContext {
        let mode = self.mode();
        let mut ctx = KeyContext::new("marketdata").grid().pair("mode", mode);
        // The action menu holds j/k while open: the shared menu steps reach
        // it through this flag, and `mode == menu` keeps the grid's motions
        // off the cells beneath it.
        if mode == "menu" {
            ctx = ctx.tilelist();
        }
        if let Some(s) = &self.selection {
            ctx = ctx.pair(
                "select",
                match s.kind {
                    SelectKind::Rows => "rows",
                    SelectKind::Block => "block",
                },
            );
        }
        ctx.counts()
    }

    /// The key context's `mode`, and the header's mode icon read from it:
    /// `insert` while the editor, picker, choice field, or upload prompt
    /// holds the keys, `menu` while the action menu is up, `visual` while a
    /// selection is live, `normal` otherwise.
    pub(crate) fn mode(&self) -> &'static str {
        if self.pending_upload.is_some()
            || self.editor.is_some()
            || matches!(self.popup, Some(Popup::Picker(_) | Popup::Choice(_)))
        {
            "insert"
        } else if matches!(self.popup, Some(Popup::Menu(_))) {
            "menu"
        } else if self.selection.is_some() {
            "visual"
        } else {
            "normal"
        }
    }

    /// Whether this tile's editor, picker, choice field, or upload confirmation
    /// actually owns window focus. Open state alone is insufficient after focus moves
    /// to another tile or shell surface.
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
        let confirm = self
            .pending_upload
            .as_ref()
            .is_some_and(|c| c.holds_focus(window));
        editor || popup || confirm
    }

    // ---- the request -------------------------------------------------

    fn versions(&self, cx: &App) -> FrameVersions {
        self.frame.read(cx).versions_for(&self.publication)
    }

    /// Shared followed-version comparison for deciding both requery and staged-result
    /// validity. Only as-of and watched publication data affect this document request.
    fn differs_on_followed(versions: FrameVersions, now: FrameVersions) -> bool {
        versions.as_of != now.as_of || versions.data != now.data
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
            .is_none_or(|watch| !watch.is_for(&self.spec.dataset, Some(&batch)))
        {
            self.publication = Some(self.frame.update(cx, |frame, _| {
                frame.watch_publications(&self.spec.dataset, Some(&batch))
            }));
        }
        let (as_of, versions) = {
            let frame = self.frame.read(cx);
            (frame.as_of().clone(), frame.versions_for(&self.publication))
        };
        // `begin` also drops whatever was staged for the previous question.
        let submitted = Instant::now();
        let tag = self.following.begin(versions, submitted);
        let key = QueryKey(self.id.0);
        let queued = self.data.document(DocumentParams {
            key,
            tag,
            submitted,
            dataset: self.spec.dataset.to_string(),
            document_key,
            as_of,
        });
        if let Err(refusal) = &queued {
            self.notice = Some(format!("document request refused: {refusal}").into());
        }
        self.following.submitted(
            queued.is_ok(),
            Unanswered::Retry,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        self.changed(cx);
    }

    pub fn deliver(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        let now = self.versions(cx);
        let key = QueryKey(self.id.0);
        let delivered = self.following.deliver(
            outcome.tag,
            outcome.snapshot,
            now,
            Self::differs_on_followed,
            &mut FrameDoor::new(&self.frame, cx),
            key,
        );
        match delivered {
            // A newer request is out; its own outcome answers the barrier.
            Delivered::Stale => return,
            // `apply` clears notices only when it paints the delivery, before
            // it writes any new restore, policy or validation notice.
            Delivered::Apply(snapshot) => self.apply(snapshot, cx),
            // Superseded: asked under an as-of or document generation this
            // panel has since moved past (it was hidden across the change).
            // Applying would run the draft policy against a document nobody
            // is looking at; the reshow asks again.
            Delivered::Held | Delivered::Superseded => {}
            // Last good stays on screen: a failed select says nothing about
            // the document already painted.
            Delivered::Failed(e) => self.notice = Some(e.into()),
        }
        self.changed(cx);
    }

    /// Handle the latest tagged upload outcome. Success enters Sent only if the draft
    /// still matches the submitted snapshot; changes made in flight remain unsent.
    /// Failure leaves it editable and records an upload error.
    ///
    /// For an upload whose underlying is no longer displayed, report the key and target
    /// without changing the current document's draft or upload state.
    pub fn deliver_upload(&mut self, u: UploadDelivery, cx: &mut Context<Self>) {
        if u.tag != self.upload_tag {
            return;
        }
        let flight = self.in_flight.take();
        if let Some(flight) = flight.filter(|f| self.key.as_deref() != Some(f.key.as_slice())) {
            let key = display_key(&flight.key);
            let target = flight.target;
            let notice = match &u.result {
                Ok(()) => format!("upload of {key} to {target} sent"),
                Err(e) => format!("upload of {key} to {target} failed: {e}"),
            };
            tracing::info!(
                target: "geode::ingest",
                tile = self.id.0,
                key = %key,
                target = %target,
                tag = u.tag,
                ok = u.result.is_ok(),
                "upload outcome for an underlying no longer shown"
            );
            self.notice = Some(notice.into());
            self.changed(cx);
            return;
        }
        let submitted = self.submitted.take();
        match u.result {
            Ok(()) => {
                tracing::info!(
                    target: "geode::ingest",
                    tile = self.id.0,
                    target = %u.target,
                    tag = u.tag,
                    "upload accepted"
                );
                // The WHOLE draft, `base` included: a rebase while the
                // upload was in flight keeps the same edits on a base
                // that was never sent.
                let unchanged = submitted.is_some_and(|d| d == self.draft);
                if unchanged && self.draft.state == DraftState::Editing {
                    self.draft.state = DraftState::Sent {
                        at: chrono::Utc::now().to_rfc3339(),
                    };
                    self.rebuild_model(cx);
                }
            }
            Err(e) => {
                tracing::info!(
                    target: "geode::ingest",
                    tile = self.id.0,
                    target = %u.target,
                    tag = u.tag,
                    error = %e,
                    "upload failed"
                );
                self.upload_error =
                    Some((format!("upload failed: {e}").into(), self.draft.clone()));
                self.sent = None;
            }
        }
        self.changed(cx);
    }

    // Upload confirmation and submission.

    /// `:upload [target]`, the menu's `Upload` row and the palette's
    /// action: resolve the target, refuse what cannot be sent, assemble
    /// the document and arm the y/n confirm. Nothing is sent here.
    fn arm_upload(
        &mut self,
        target: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // An editor left open (orphaned by a focus move) is cancelled, as
        // every other verb that is not its commit does: blur, then drop.
        // First, before anything reads the draft: a selection editor's
        // close takes its live steps back out, so a document assembled
        // before it would send steps the draft no longer holds, and the
        // `y` check (armed draft against current) could not tell.
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        let document = &self.spec.document;
        let target = match target {
            Some(t) => {
                if !self.egress_targets.iter().any(|e| e.as_ref() == t) {
                    return Err(format!("{t} does not accept {document}"));
                }
                t
            }
            None => match self.egress_targets.as_slice() {
                [] => return Err(format!("no egress target accepts {document}")),
                [one] => one.to_string(),
                several => {
                    let names: Vec<&str> = several.iter().map(|t| t.as_ref()).collect();
                    return Err(format!("upload to which target? {}", names.join(", ")));
                }
            },
        };
        if let Some(flight) = &self.in_flight {
            let (key, target) = (display_key(&flight.key), &flight.target);
            return Err(format!("an upload of {key} to {target} is in flight"));
        }
        if self.draft.is_empty() {
            return Err("nothing to upload".into());
        }
        if let Some(refusal) = self.not_live(cx) {
            return Err(refusal);
        }
        if self.draft.is_behind() {
            return Err(UPLOAD_BEHIND.into());
        }
        if self.draft.is_sent() {
            return Err("already sent".into());
        }
        match self.draft.incomplete_rows(&self.spec, &self.model.columns) {
            0 => {}
            1 => return Err("1 row incomplete".into()),
            n => return Err(format!("{n} rows incomplete")),
        }
        let Some(snapshot) = self.painted_snapshot() else {
            return Err("no document to upload".into());
        };
        let rows = crate::core::upload::assemble(&snapshot, &self.spec, &self.model, &self.draft)?;
        let cells = match self.draft.cell_count() {
            1 => "1 cell".to_string(),
            n => format!("{n} cells"),
        };
        let added = match self.draft.rows_added() {
            1 => "1 row added".to_string(),
            n => format!("{n} rows added"),
        };
        let attrs = match self.draft.attr_count() {
            0 => String::new(),
            1 => "1 attribute, ".to_string(),
            n => format!("{n} attributes, "),
        };
        let key = self.key.as_deref().map(display_key).unwrap_or_default();
        let prompt = format!(
            "upload {cells}, {attrs}{added}, {} removed of {key} to {target}? (y/n)",
            self.draft.rows_removed()
        );
        // A confirm already armed is replaced, never stacked.
        confirm::arm(
            self,
            PendingUpload {
                target,
                rows,
                draft: self.draft.clone(),
            },
            prompt,
            window,
            cx,
        );
        self.notice = None;
        self.changed(cx);
        Ok(())
    }

    /// Why an upload cannot be sent now, if it cannot: the frame asks
    /// for a historical as-of, or the snapshot on screen (the one
    /// `:upload` assembles) was delivered for one. An upload is a whole
    /// document: one assembled over a historical generation would revert
    /// every untouched row upstream. The second check covers the window
    /// after the frame goes live, while the historical generation stays
    /// painted until a live one is applied — briefly behind the barrier,
    /// indefinitely if the live requery is refused or answers `Err`.
    fn not_live(&self, cx: &App) -> Option<String> {
        let now = chrono::Utc::now();
        if let geode_core::query::AsOf::At(at) = self.frame.read(cx).as_of() {
            let when = as_of_text(*at, now, self.clock);
            return Some(format!("upload: the panel shows {when}, not live"));
        }
        let painted = self
            .painted_snapshot()
            .and_then(|s| s.provenance().as_of_request.clone());
        if let Some(at) = painted {
            let when = chrono::DateTime::parse_from_rfc3339(&at)
                .map(|t| as_of_text(t.with_timezone(&chrono::Utc), now, self.clock))
                .unwrap_or(at);
            return Some(format!("upload: the panel shows {when}, not live"));
        }
        None
    }

    /// `y`: submit the document assembled at arm time. Refused by the data
    /// tier's bounded queue → a notice and nothing kept as `sent`.
    fn submit_upload(&mut self, pending: PendingUpload, cx: &mut Context<Self>) {
        // Re-checked at `y`: nothing that makes the panel historical
        // between arming and answering may slip through.
        if let Some(refusal) = self.not_live(cx) {
            self.notice = Some(refusal.into());
            self.changed(cx);
            return;
        }
        if pending.draft != self.draft {
            self.notice = Some("upload cancelled: the draft changed under the question".into());
            self.changed(cx);
            return;
        }
        self.upload_tag += 1;
        self.upload_error = None;
        let key = self.key.as_deref().map(display_key).unwrap_or_default();
        tracing::info!(
            target: "geode::ingest",
            key = %key,
            target = %pending.target,
            cells = self.draft.cell_count(),
            rows_added = self.draft.rows_added(),
            rows_removed = self.draft.rows_removed(),
            "upload submitted"
        );
        self.sent = Some(pending.rows.clone());
        self.submitted = Some(self.draft.clone());
        self.in_flight = Some(InFlightUpload {
            key: self.key.clone().unwrap_or_default(),
            target: pending.target.clone(),
        });
        let queued = self.data.upload(geode_data::UploadParams {
            key: QueryKey(self.id.0),
            tag: self.upload_tag,
            target: pending.target,
            document: self.spec.document.clone(),
            rows: pending.rows,
        });
        if let Err(refusal) = queued {
            self.notice = Some(format!("upload refused: {refusal}").into());
            self.sent = None;
            self.submitted = None;
            self.in_flight = None;
        }
        self.changed(cx);
    }

    /// Put a delivered snapshot on screen: the draft's own view of the
    /// generation, the model, the cursor and the scroll. Called by
    /// `deliver` for an outcome it paints at once and by the frame observer for
    /// a staged one, so the two paths cannot drift.
    fn apply(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let painted = self.painted_snapshot().and_then(|s| base_of(&s));
        self.apply_snapshot(snapshot, cx);
        self.withdraw_upload_if_moved(painted, cx);
    }

    /// A confirm armed over one document must not stand over another: if
    /// this delivery changed the painted generation or the draft (a
    /// `rebase` or `replace` policy, or `Behind` under `hold`), the
    /// question is withdrawn at once. The confirm door withdraws the prompt:
    /// it drops the blur answer first and blurs through the recorded window
    /// handle, deferred. `y`'s own re-check in [`Self::submit_upload`]
    /// stays as the second line.
    ///
    /// The painted-base comparison is whole-pair equality rather than
    /// [`DocumentBase::differs_from`]: withdrawing a question that did not
    /// need withdrawing costs a keystroke, while leaving one armed over a
    /// document it was never asked about sends the wrong rows.
    fn withdraw_upload_if_moved(&mut self, painted: Option<DocumentBase>, cx: &mut Context<Self>) {
        let now = self.painted_snapshot().and_then(|s| base_of(&s));
        let moved = |c: &Confirm<PendingUpload>| c.payload().draft != self.draft || now != painted;
        if !self.pending_upload.as_ref().is_some_and(moved) {
            return;
        }
        if confirm::withdraw(self, cx).is_none() {
            return;
        }
        // The policy's own disclosure (`replace`'s count, `rebase`'s
        // dropped edits) is kept behind the cancellation, never lost to it.
        self.notice = Some(match self.notice.take() {
            Some(n) => format!("{UPLOAD_CANCELLED_ARRIVED}; {n}").into(),
            None => UPLOAD_CANCELLED_ARRIVED.into(),
        });
        self.changed(cx);
    }

    /// [`Self::apply`]'s body: everything a delivered snapshot does to the
    /// panel, before the armed confirm is checked against it.
    fn apply_snapshot(&mut self, snapshot: Arc<Snapshot>, cx: &mut Context<Self>) {
        let base = base_of(&snapshot);
        // Evaluate delivery on a draft copy and build the resulting model before
        // committing either. An unbuildable generation changes only the notice, keeping
        // the last usable snapshot, draft state, and model together.
        let mut draft = self.draft.clone();
        // Capture the original base before policy handling: automatic rebase
        // replaces it with the delivered pair, which would compare equal to itself.
        let held = draft.base.clone();
        let mut moved = base.as_ref().is_some_and(|b| draft.on_delivered(b));
        // Evaluate the upload echo on the same draft copy before committing state.
        let echo = match self.echo_of(&snapshot, base.as_ref(), &mut draft) {
            Ok(echo) => echo,
            Err(unbuildable) => {
                self.notice = Some(unbuildable.into());
                return;
            }
        };
        if matches!(echo, EchoStep::Unchecked) {
            moved = true;
        }
        // Apply live update policy only to a transition to a different Behind
        // generation. Changing policy or redelivering the same generation leaves
        // existing state alone. Unresolved restores always take Hold, preserving unsent
        // work before its first usable model; Sent drafts use echo handling separately.
        // Commit any policy notice after the delivery's ordinary notice clear.
        let mut notice: Option<SharedString> = None;
        if moved
            && draft.is_behind()
            && self.policy != UpdatePolicy::Hold
            && !self.unresolved_restore
        {
            match self.policy {
                UpdatePolicy::Hold => unreachable!("guarded above"),
                UpdatePolicy::Rebase => {
                    // Capture same-day group sizes only from the outgoing snapshot when
                    // it is the draft's actual base. A fallback newer snapshot must not
                    // overwrite restored base-group metadata before rebase.
                    self.capture_groups_if_base(&mut draft);
                    // Two builds, on purpose: `Draft::rebase` re-places
                    // the edits by the NEW document's row and column
                    // labels, which only a model of that document
                    // carries — so the first build is against a clean
                    // draft for its labels alone (what `:rebase` does),
                    // and the second, below, paints the re-placed edits.
                    // A refusal of either changes nothing but the notice.
                    let clean = match MatrixIndex::build(&snapshot, &self.spec, &Draft::default()) {
                        Ok(model) => model,
                        Err(e) => {
                            self.notice = Some(e.into());
                            return;
                        }
                    };
                    // An empty new document supplies no label map for automatic rebase.
                    // Keep the draft Behind with edits intact; explicit rebase remains
                    // available.
                    if !clean.is_empty() {
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
                    let when = base
                        .as_ref()
                        .map(|b| local_hhmm(&b.as_of, self.clock))
                        .unwrap_or_default();
                    notice = Some(format!("update {when} replaced {phrase}").into());
                }
            }
        }
        // Disclose a same-time republish when it changes draft state. The timestamp
        // cannot show the change, and automatic rebase leaves no Behind badge.
        // A replacement or dropped-edit notice takes precedence. Redelivery and
        // returning to the base do not produce a republish notice.
        if moved
            && notice.is_none()
            && let Some(delivered) = &base
            && let Some(held) = &held
            && held.as_of == delivered.as_of
            && held.differs_from(delivered)
        {
            let when = local_hhmm(&delivered.as_of, self.clock);
            notice = Some(if draft.is_behind() {
                // The edits are still pending against the old generation, so
                // name the two keys that resolve them.
                format!("republished at {when} — :rebase or :revert").into()
            } else {
                // Rebased automatically by the trader's own standing policy:
                // nothing is pending, so there is no key to offer.
                format!("republished at {when} — your edits moved onto it").into()
            });
        }
        // Retain an outgoing snapshot only when its full base equals the draft's
        // and the new state still needs it. `differs_from` permits unknown-generation
        // fallbacks; using it here could pin another grid beneath position-keyed
        // edits. A restored draft's latest painted fallback is not proof of its base.
        let retained = if draft.is_behind() || matches!(echo, EchoStep::Held(_)) {
            match &self.base_snapshot {
                Some(base) => Some(Arc::clone(base)),
                None => self
                    .snapshot
                    .clone()
                    .filter(|s| draft.base.is_some() && base_of(s) == draft.base),
            }
        } else {
            None
        };
        // Validate the delivered snapshot even while Behind paints the retained base.
        // It becomes the target for rebase, so recording an unbuildable document would
        // leave the draft without a usable resolution target.
        let built = match MatrixIndex::build(&snapshot, &self.spec, &draft) {
            Ok(model) => model,
            Err(e) => {
                self.notice = Some(e.into());
                return;
            }
        };
        // With a base retained, the screen keeps the model it already has:
        // `self.model` is by construction the index of `painted_snapshot()`
        // under these very row edits, and `on_delivered` moves only the
        // draft's STATE, which `MatrixIndex::build` never reads (it reads
        // the row edits and the attributes, and a delivery moves neither;
        // the install below refills the window from the new state). Keeping
        // it is also what makes this one build per delivery rather than
        // two — the freshly built model above is a validation of the
        // delivered generation, not a paint.
        let model = match &retained {
            Some(_) => Rc::clone(&self.model),
            None => Rc::new(built),
        };

        // Commit only after validation. Replace the prior notice with the policy notice
        // on the delivery that paints, before this method writes any restore or
        // validation notice. Preserve the replace disclosure through this clear.
        self.notice = notice;
        self.draft = draft;
        match echo {
            EchoStep::Confirmed(line) => {
                self.sent = None;
                self.echo = Some(Echo::Confirmed(line));
            }
            EchoStep::Held(differs) => self.echo = Some(differs),
            // A delivery that compared nothing leaves a confirmation up
            // (it stands until the next edit) but not a difference: that
            // described a generation no longer held — the base itself
            // came back, say.
            EchoStep::None | EchoStep::Unchecked => {
                if matches!(self.echo, Some(Echo::Differs { .. })) {
                    self.echo = None;
                }
            }
        }
        self.base_snapshot = retained;
        self.snapshot = Some(snapshot);
        self.model = model;
        self.clamp_cursor();
        // Resolve restored label pairs only against a successfully built nonempty
        // document. Empty or refused deliveries leave the edits parked and visible as
        // unresolved work in the header.
        if self.unresolved_restore && !self.model.is_empty() {
            self.unresolved_restore = false;
            // Keep a restored Behind draft parked until explicit rebase or revert.
            if !self.draft.is_behind() {
                // Resolve against the document-only grid. A model containing this
                // draft's inserted rows would mistake them for upstream conflicts and
                // use post-splice indices for document edits. This extra build occurs
                // once per restore; failure leaves the draft unresolved.
                let clean = self
                    .painted_snapshot()
                    .and_then(|s| MatrixIndex::build(&s, &self.spec, &Draft::default()).ok());
                let Some(clean) = clean else {
                    self.unresolved_restore = true;
                    self.install_model(cx);
                    return;
                };
                let (_, dropped) = self.draft.rebase(&clean);
                self.rebuild_model(cx);
                // Report dropped restored edits. Rebuild first so its failure notice
                // takes precedence over the dropped-edit report.
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

    /// Compare a Sent draft's saved upload rows with a different delivered generation.
    /// Matching content clears the draft and follows the delivery; a difference keeps
    /// Sent over its base. Reuse a difference result for the same generation.
    ///
    /// Assemble incoming content from a clean document model. Empty or invalid upload
    /// content cannot confirm the send and produces a differing-echo notice. A
    /// model-build failure returns Err so apply commits nothing. Missing saved send
    /// rows instead transitions the draft to Behind rather than claiming confirmation.
    fn echo_of(
        &self,
        snapshot: &Arc<Snapshot>,
        delivered: Option<&DocumentBase>,
        draft: &mut Draft,
    ) -> Result<EchoStep, String> {
        let (DraftState::Sent { at }, Some(delivered)) = (&draft.state, delivered) else {
            return Ok(EchoStep::None);
        };
        if draft
            .base
            .as_ref()
            .is_some_and(|b| !b.differs_from(delivered))
        {
            return Ok(EchoStep::None);
        }
        let Some(sent) = self.sent.as_ref() else {
            draft.state = DraftState::Behind {
                newer: delivered.clone(),
            };
            return Ok(EchoStep::Unchecked);
        };
        if let Some(held @ Echo::Differs { newer, .. }) = &self.echo
            && newer == delivered
        {
            return Ok(EchoStep::Held(held.clone()));
        }
        let clean = MatrixIndex::build(snapshot, &self.spec, &Draft::default())?;
        let held = |text: String| {
            EchoStep::Held(Echo::Differs {
                newer: delivered.clone(),
                text: text.into(),
            })
        };
        let delivered =
            match crate::core::upload::assemble(snapshot, &self.spec, &clean, &Draft::default()) {
                Ok(delivered) => delivered,
                Err(e) => return Ok(held(format!("echo not comparable: {e}"))),
            };
        let differing = crate::core::upload::echo_differs(&self.spec, sent, &delivered);
        if differing == 0 {
            let line = format!(
                "sent {}, confirmed {}",
                local_hhmm(at, self.clock),
                local_hhmm(&chrono::Utc::now().to_rfc3339(), self.clock)
            );
            draft.revert();
            return Ok(EchoStep::Confirmed(line.into()));
        }
        Ok(held(format!("echo differs ({differing} rows)")))
    }

    /// Hiding cancels nothing and forgets nothing: the outstanding request
    /// finishes and its reply applies when it lands (it still answers any
    /// barrier it was enrolled in), unless a counter this panel follows
    /// moved since it asked: that reply is dropped as `Superseded`, not
    /// applied, so no draft policy runs against a document nobody asked
    /// about. Showing again requeries only if a
    /// counter this panel follows moved since it last asked. Closing is
    /// `closed`.
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
            if self.key.is_some()
                && self
                    .following
                    .follows_changed(now, Self::differs_on_followed)
            {
                self.requery(cx);
            }
        }
        self.changed(cx);
    }

    /// The shell is removing this panel: cancel the document request by key
    /// and answer any barrier still waiting on it, so a flip never waits out
    /// its deadline for a panel that is gone. Runs inside the shell's
    /// occupant reconciliation, so it updates only the frame and the data
    /// handle, never the shell.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        let key = QueryKey(self.id.0);
        self.data.cancel(key);
        self.following
            .close(&mut FrameDoor::new(&self.frame, cx), key);
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
            None => spec.title.clone().into(),
        }
    }

    // ---- the model ---------------------------------------------------

    /// The draft the installed index is painted with — what the delegate
    /// fills its window from when the table reports a new range.
    pub(crate) fn painted_draft(&self) -> &Draft {
        &self.draft
    }

    /// The snapshot on screen: the draft's own base generation while one
    /// is retained, else the newest delivered.
    fn painted_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.base_snapshot.clone().or_else(|| self.snapshot.clone())
    }

    /// Capture same-day group sizes only if the painted snapshot is the draft's own
    /// base generation. A restored or parked draft may paint a newer fallback because
    /// its real base was never delivered here; keep its stored base-group sizes in that
    /// case. All capture sites share this guard before calling Draft::capture_groups.
    fn capture_groups_if_base(&self, draft: &mut Draft) {
        let Some(base) = self.model.snapshot() else {
            return;
        };
        // Capture group sizes only from an exactly matching base. The source-time
        // fallback in `differs_from` cannot establish that a snapshot belongs to
        // the draft and must not replace restored group guards.
        if base_of(base) != draft.base {
            return;
        }
        // The installed index was built from that base; group sizes read its
        // document rows only, which a draft's inserts never are. No build:
        // `serialize` calls this on every session tick.
        draft.capture_groups(&self.model);
    }

    /// Rebuild the prepared grid. Called on a delivery and on a draft
    /// change — never from `render`.
    fn rebuild_model(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.painted_snapshot() else {
            self.model = Rc::new(MatrixIndex::empty(
                &self.spec,
                self.key.as_deref().unwrap_or(&[]),
            ));
            self.clamp_cursor();
            self.install_model(cx);
            return;
        };
        match MatrixIndex::build(&snapshot, &self.spec, &self.draft) {
            Ok(model) => self.model = Rc::new(model),
            // A document that cannot be laid out as a grid (a hole, a
            // repeated pair, a missing axis) leaves the last good model
            // on screen and says what it was — never a half-built grid.
            Err(e) => self.notice = Some(e.into()),
        }
        self.clamp_cursor();
        self.install_model(cx);
    }

    /// Share the current index with the delegate, then refresh cached columns
    /// and headers, refill the window over the range the table last reported
    /// (an unchanged range is never re-reported) and drop find's search text
    /// before synchronizing the cursor. An index swap can change the node
    /// ladder or gutter width; replacing only the delegate's index would leave
    /// TableState painting stale headers and widths. Sharing the index clones
    /// its Rc.
    fn install_model(&mut self, cx: &mut Context<Self>) {
        let model = Rc::clone(&self.model);
        let draft = &self.draft;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().model = model;
            t.refresh(cx);
            t.delegate_mut().refill_window(draft);
        });
        self.search_text = None;
        self.sync_cursor(cx);
    }

    /// The grid and strip shape `cursor::step`/`clamp` reason about —
    /// this panel's whole cursor vocabulary, in one small `Copy` value.
    fn grid(&self) -> Grid {
        Grid {
            rows: self.model.len(),
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

    /// Mirror line-number settings and refresh the table when they change.
    /// The pinned column includes the gutter, and TableState caches column widths.
    fn on_ui_settings(&mut self, cx: &mut Context<Self>) {
        let mode = cx
            .try_global::<UiSettings>()
            .map_or(LineNumbers::Off, |s| s.line_numbers);
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if d.line_numbers != mode {
                d.line_numbers = mode;
                t.refresh(cx);
                cx.notify();
            }
        });
    }

    /// Mirror cursor, selection, editor, and choice state into the delegate
    /// and table. Translate model columns through the optional row-label
    /// offset. Set the column before the row so the table finishes in
    /// row-selection mode while keeping the cell in view. An attribute cursor
    /// clears grid selection.
    ///
    /// Re-resolves the grid selection first, so every cursor or model change
    /// hands the delegate a current `Resolved`; a lost anchor re-prepares the
    /// header for its notice here, since not every caller rebuilds chrome.
    fn sync_cursor(&mut self, cx: &mut Context<Self>) {
        if self.refresh_selection() {
            self.rebuild_chrome();
        }
        let editor = self.delegate_editor();
        let choice = self.delegate_choice();
        let selected = self.resolved.clone();
        match self.cursor {
            Cursor::Cell { row, col } => self.table.update(cx, |t, cx| {
                let d = t.delegate_mut();
                d.cursor = Some((row, col));
                d.selected = selected;
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
                d.selected = None;
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
        let (row, col) = self.editor_cell()?;
        let paint = match &e.state {
            EditorState::Text(state) => DelegateEditorPaint::Text(state.clone()),
            EditorState::Date { paint, focus, .. } => DelegateEditorPaint::Date {
                paint: paint.clone(),
                focus: focus.clone(),
            },
        };
        Some(DelegateEditor { row, col, paint })
    }

    /// The grid cell the open editor is painted in, as `(model row, model
    /// column)`. A row-label editor sits in the row-label column (`col:
    /// None`), which `render_td`'s label arm paints through the same
    /// `render_editor` a cell's uses. `None` for an attribute editor (the
    /// header paints it) and with nothing open.
    fn editor_cell(&self) -> Option<(usize, Option<usize>)> {
        match &self.editor.as_ref()?.target {
            EditTarget::Cell { cell, .. } => Some((cell.0, Some(cell.1))),
            EditTarget::RowLabel { row, .. } => Some((*row, None)),
            EditTarget::Attr { .. } => None,
        }
    }

    /// Prepared choice popup and its cell anchor, mirrored into the delegate by Rc.
    /// Return None for other popup states without rebuilding choice rows.
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

    /// Select a clicked cell, clamping to the grid. A row-label click changes only the
    /// row, using last_grid_col when returning from the attribute strip.
    fn cursor_to(&mut self, row: usize, col: Option<usize>, cx: &mut Context<Self>) {
        if self.model.is_empty() {
            return;
        }
        let row = row.min(self.model.len().saturating_sub(1));
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

    /// Every mouse selection gesture lands here and goes through the same
    /// `start_selection`/`clear_selection` doors the keys use, so the mouse
    /// never reaches a selection the keys could not. A plain press clears
    /// and moves the cursor (the table's `SelectCell` on the release moves
    /// it again, to the same cell); a shift press or a drag starts a
    /// selection only when none is live — `Rows` from a row label or the
    /// gutter, `Block` from a value cell — then moves the cursor, which
    /// extends it.
    ///
    /// Ordering: the delegate emits the press on mouse-down and the table
    /// emits `SelectCell` only on the click (the release), so a shift
    /// press starts the selection at the PRE-press cursor with no capture
    /// of it needed. A drag anchors at its press cell because the plain
    /// press already moved the cursor there.
    ///
    /// Any gesture that gets here closes an open editor first: a click is
    /// a cancel (`close_editor`'s rule), and a drag never gets the
    /// `SelectCell` that would otherwise close it.
    fn pointer(&mut self, event: CellPointer, window: &mut Window, cx: &mut Context<Self>) {
        // What the header shows (mode, notice, footer extent) can only
        // move with one of these; a plain press that changes none of them
        // (the cursor cell, nothing selected) skips the rebuild.
        let before = (
            self.selection.is_some(),
            self.cursor,
            self.editor.is_some(),
            self.notice.clone(),
        );
        let kind_for = |label: bool| {
            if label {
                SelectKind::Rows
            } else {
                SelectKind::Block
            }
        };
        let (row, col, start) = match event {
            CellPointer::Press {
                row,
                col,
                shift: false,
            } => {
                self.clear_selection();
                (row, col, None)
            }
            CellPointer::Press {
                row,
                col,
                shift: true,
            } => (row, col, Some(kind_for(col.is_none()))),
            CellPointer::Drag { row, col, label } => {
                // Still inside the cell the cursor is on (a label or the
                // gutter keeps the column): nothing to start or extend.
                let here = matches!(
                    self.cursor,
                    Cursor::Cell { row: r, col: c } if r == row && col.is_none_or(|x| x == c)
                );
                if here {
                    return;
                }
                (row, col, Some(kind_for(label)))
            }
        };
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        if let Some(kind) = start
            && self.selection.is_none()
        {
            // From the header strip there is no grid cursor to anchor at
            // (`start_selection` would refuse): the press cell is the anchor.
            if matches!(self.cursor, Cursor::Attr(_)) {
                self.cursor_to(row, col, cx);
            }
            self.start_selection(kind);
        }
        // Ends in `sync_cursor`, which re-resolves the selection.
        self.cursor_to(row, col, cx);
        let after = (
            self.selection.is_some(),
            self.cursor,
            self.editor.is_some(),
            self.notice.clone(),
        );
        if after != before {
            self.changed(cx);
        }
    }

    /// Select an attribute without opening it. Cancel an existing editor first,
    /// blurring only its own focused input; clicking is not a commit.
    pub(crate) fn cursor_to_attr(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let attrs = self.model.header.len();
        if attrs == 0 {
            return;
        }
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        // The strip is never a selection member: leaving the grid ends the
        // selection here, before `sync_cursor` would read the strip cursor
        // as a lost anchor and say so.
        self.clear_selection();
        self.cursor = Cursor::Attr(i.min(attrs - 1));
        self.sync_cursor(cx);
        self.rebuild_chrome();
        cx.notify();
    }

    /// Attribute mouse-down selects the target; the second press also invokes the
    /// ordinary edit route, including its refusals. The shell preserves the newly
    /// focused field while the tile reports insert mode.
    pub(crate) fn attr_clicked(
        &mut self,
        i: usize,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor_to_attr(i, window, cx);
        if click_count == 2 && matches!(self.cursor, Cursor::Attr(_)) {
            self.begin_edit(EditCaret::End, window, cx);
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

    /// Prepare header identity, attributes, draft/upload state, notices, and
    /// source-time text from current tile state.
    fn rebuild_chrome(&mut self) {
        if self
            .upload_error
            .as_ref()
            .is_some_and(|(_, at)| !same_edits(at, &self.draft))
        {
            self.upload_error = None;
        }
        // `sent`'s one rule (see the field): kept for an upload in flight
        // or a draft `Sent` from it, and for nothing else.
        if self.submitted.is_none() && !self.draft.is_sent() {
            self.sent = None;
        }
        // A confirmation stands until the next edit; a difference only
        // while the draft is still `Sent` against it.
        let echo_over = match &self.echo {
            Some(Echo::Confirmed(_)) => !self.draft.is_empty(),
            Some(Echo::Differs { .. }) => !self.draft.is_sent(),
            None => false,
        };
        if echo_over {
            self.echo = None;
        }
        self.source_at = self
            .model
            .base
            .as_ref()
            .map(|b| b.as_of.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        self.header = HeaderModel::prepare(HeaderInputs {
            spec: &self.spec,
            key: self.key.as_deref(),
            model: &self.model,
            badge: self.draft.badge(),
            unresolved_restore: self.unresolved_restore,
            notice: self.notice.as_ref(),
            upload_error: self.upload_error.as_ref().map(|(e, _)| e),
            echo: self.echo.as_ref().map(|e| match e {
                Echo::Confirmed(text) => (text, Tone::Time),
                Echo::Differs { text, .. } => (text, Tone::Warn),
            }),
            prompt: self.pending_upload.as_ref().map(|c| c.prompt_text()),
            source_at: self.source_at,
            incomplete: self.draft.incomplete_rows(&self.spec, &self.model.columns),
            clock: self.clock,
        });
    }

    /// Whether the painted generation exceeds this panel's stale_after interval.
    fn is_stale(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.source_at.is_some_and(|at| {
            now.signed_duration_since(at).to_std().unwrap_or_default() > self.stale_after.get()
        })
    }

    // ---- keys --------------------------------------------------------

    /// Dispatch tile actions with window access for editor, popup, and confirmation
    /// focus transitions.
    pub fn dispatch(
        &mut self,
        action: &ActionId,
        count: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shared_motion = geode_tile::motion::parse(action, count);
        // A shared motion runs as the one "motion" verb, so the popup,
        // selection-editor and chrome rules below treat every motion alike.
        // The shared menu steps run as the panel's own menu verbs, so they
        // keep the open popup and its stepping rule (skip disabled rows).
        let verb = if shared_motion.is_some() {
            "motion"
        } else if action.0 == geode_tile::motion::MENU_DOWN {
            "menu_down"
        } else if action.0 == geode_tile::motion::MENU_UP {
            "menu_up"
        } else if let Some(verb) = action.0.strip_prefix("marketdata::") {
            verb
        } else {
            return false;
        };
        // An open selection editor's members are its operand. The palette
        // reaches these verbs in insert mode, and each would move or end
        // the selection under the editor, so the commit or the steps would
        // land on cells the trader did not open it over.
        if self.selection_editor_open() && self.changes_selection(verb) {
            self.notice = Some(FINISH_EDIT_FIRST.into());
            self.rebuild_chrome();
            cx.notify();
            return true;
        }
        // Close popups before unrelated actions. Menu commands, commit/cancel, and
        // insert navigation retain their active popup so they can act on it. Use the
        // window-aware close to blur a focused picker or choice field before dropping
        // it.
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
        // Rebuild prepared header text only when the action changes header state.
        // Grid-only motion, yank, and repeat-find need notification, while moving into
        // or out of the attribute strip also changes header cursor styling.
        let chrome = match verb {
            "motion" => {
                // `verb` is "motion" only when `parse` returned a motion; a
                // module id cannot reach this arm, since no `marketdata::`
                // action is named `motion`.
                let motion =
                    shared_motion.expect("the motion verb comes only from a parsed motion");
                let was_attr = matches!(self.cursor, Cursor::Attr(_));
                let grid = self.grid();
                // A live selection's motions clamp at the grid's edges and
                // never enter the strip, which is never a member.
                self.cursor = if self.selection.is_some() {
                    cursor::step_clamped(self.cursor, motion, grid)
                } else {
                    cursor::step(self.cursor, &mut self.last_grid_col, motion, grid)
                };
                was_attr != matches!(self.cursor, Cursor::Attr(_))
            }
            // With a selection live, `y` copies the selection rather than
            // the cursor cell, and consumes it.
            "yank" if self.selection.is_some() => {
                if let Some(text) = self.selection_tsv() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                self.clear_selection();
                false
            }
            "yank" | "yank_row" | "yank_col" => {
                let what = match verb {
                    "yank" => Yank::Cell,
                    "yank_row" => Yank::Row,
                    _ => Yank::Col,
                };
                // Column yank has no target in the attribute strip; report that
                // refusal.
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
            "edit" | "edit_start" => {
                let caret = if verb == "edit_start" {
                    EditCaret::Start
                } else {
                    EditCaret::End
                };
                self.begin_edit(caret, window, cx);
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
            "visual_rows" | "visual_block" => {
                let kind = if verb == "visual_rows" {
                    SelectKind::Rows
                } else {
                    SelectKind::Block
                };
                // `true`: a refusal in the strip leaves a notice.
                self.start_selection(kind);
                true
            }
            "escape" => {
                if self.selection.is_some() {
                    // The first escape ends only the selection; find and
                    // the notice wait for the next one. The tail's
                    // `sync_cursor` hands the delegate the cleared tint.
                    self.clear_selection();
                    false
                } else {
                    self.find = None;
                    // Only when there WAS one: `escape` on a clean header
                    // changes nothing the chips show.
                    self.notice.take().is_some()
                }
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
                    Some(Popup::Menu(m)) => m.step(delta),
                    Some(Popup::Picker(p)) => p.rows.step_highlighted(delta),
                    // Reachable from the palette alone (`mode == menu` is
                    // never reported with a choice popup open), and it
                    // has nothing to move there.
                    Some(Popup::Choice(_)) | None => {}
                }
                false
            }
            // Insert arrows act on the open input: clamped picker navigation, wrapping
            // choice navigation, or numeric nudging. Large picker steps still move one
            // row. With no applicable input, return unhandled.
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
                if let Some(index) = match &self.popup {
                    Some(Popup::Menu(m)) => m.highlighted(),
                    _ => None,
                } {
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
            // Step a choice cell forward or backward without opening its popup.
            "step" | "step_back" => {
                let delta = if verb == "step" { n } else { -n };
                if let Err(e) = self.step_choice(delta, window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            // Insert/delete row actions update dirty, incomplete, or refusal header
            // state.
            "insert_below" | "insert_above" => {
                if let Err(e) = self.insert_row(verb == "insert_below", window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            "delete_row" => {
                let deleted = if self.selection.is_some() {
                    self.delete_selected_rows(window, cx)
                } else {
                    self.delete_row(window, cx)
                };
                if let Err(e) = deleted {
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
            // The menu and palette upload action uses the no-argument upload route.
            "upload" => {
                if let Err(e) = self.arm_upload(None, window, cx) {
                    self.notice = Some(e.into());
                }
                true
            }
            _ if self
                .spec
                .actions
                .iter()
                .any(|a| a.id.strip_prefix("marketdata::") == Some(verb)) =>
            {
                // Unbuilt kind actions report their unavailable status; computation and
                // outbound requests require their own implemented handler.
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

    /// Editing is blocked by Behind state or a held differing echo. Both require rebase
    /// or revert before further changes against the painted generation.
    fn held_refusal(&self) -> Option<&'static str> {
        if self.draft.is_behind() {
            Some(BEHIND_REFUSED)
        } else if self.draft.is_sent() && matches!(self.echo, Some(Echo::Differs { .. })) {
            Some(ECHO_REFUSED)
        } else {
            None
        }
    }

    /// Return the edit's base generation, or a refusal when no usable document
    /// exists. A model with no provenance yields the default base — an empty
    /// source time and an unknown generation. Production document queries supply
    /// one; an empty base will differ from a later dated delivery, making the
    /// provenance gap visible through Behind state.
    fn edit_base(&self) -> Result<DocumentBase, String> {
        if self.model.is_empty() || self.model.columns.is_empty() {
            return Err(NO_DOCUMENT.to_string());
        }
        Ok(self.model.base.clone().unwrap_or_default())
    }

    /// The attribute strip's own [`Self::edit_base`]: an attribute needs
    /// no row or column, only a header to belong to, which the model
    /// carries exactly when it carries any rows at all (see
    /// `MatrixIndex::build`'s early return for an empty document).
    fn attr_edit_base(&self) -> Result<DocumentBase, String> {
        if self.model.header.is_empty() {
            return Err(NO_DOCUMENT.to_string());
        }
        Ok(self.model.base.clone().unwrap_or_default())
    }

    /// Open the cursor's cell or attribute editor seeded from its painted draft value.
    /// CellKind chooses text, segmented date, or choice typeahead; attribute type
    /// chooses text or date. Apply the same document, draft-state, and deleted-row
    /// guards before opening any form. Editable NULL cells open empty.
    fn begin_edit(&mut self, caret: EditCaret, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            // Already editing. `i` is not bound in insert mode, so this is
            // the palette's route in, and re-seeding would throw away what
            // the trader has typed.
            return;
        }
        if let Some(refusal) = self.held_refusal() {
            self.notice = Some(refusal.into());
            return;
        }
        let (text, target, wants_date) = match self.cursor {
            Cursor::Cell { row, col } => {
                if let Err(e) = self.edit_base() {
                    self.notice = Some(e.into());
                    return;
                }
                // With a selection live every commit and step goes to its
                // members; opened on a cell that is not one, the typed
                // value would land in the members while this cell stayed
                // as it was.
                if self.selection.is_some() && !self.selection_holds((row, col)) {
                    self.notice = Some(select::NOT_A_MEMBER.into());
                    return;
                }
                // Reject Deleted rows before creating either an editor or choice popup.
                if self.model.state(row) == Some(RowState::Deleted) {
                    self.notice = Some(DELETED_REFUSED.into());
                    return;
                }
                let cell = (row, col);
                let text = self.model.format_cell(&self.draft, row, col);
                let labels = self.model.label_of(cell);
                if let Some(CellKind::Choice(options)) = self.model.kind_of(col) {
                    self.open_choice(cell, labels, &text, Arc::clone(options), window, cx);
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
        let (state, opened) = if wants_date {
            // Seed a date field from painted text, falling back to today's date on the
            // configured clock when the text is empty or invalid.
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
            (
                EditorState::Date {
                    field,
                    focus,
                    paint,
                },
                date.format("%Y-%m-%d").to_string(),
            )
        } else {
            let state = cx.new(|cx| InputState::new(window, cx));
            state.update(cx, |s, cx| caret.seed(s, text.clone(), window, cx));
            state.read(cx).focus_handle(cx).focus(window, cx);
            (EditorState::Text(state), text.to_string())
        };
        // Only a number cursor cell steps the selection live. A text or
        // date cursor cell commits absolutely once it has changed; an
        // untouched `enter` writes nothing (`Editing::opened`).
        let bulk = (self.selection.is_some()
            && matches!(state, EditorState::Text(_))
            && matches!(
                target,
                EditTarget::Cell { cell: (_, col), .. }
                    if matches!(self.model.kind_of(col), Some(CellKind::Number(_)))
            ))
        .then(|| Bulk {
            before: self.draft.clone(),
            after: self.draft.clone(),
            painted: self.model.base.clone(),
            seeded: text.to_string(),
            steps: 0,
            stepped: false,
            upload: UploadMarks {
                sent: self.sent.clone(),
                echo: self.echo.clone(),
                upload_error: self.upload_error.clone(),
            },
        });
        self.editor = Some(Editing {
            state,
            target,
            bulk,
            opened,
            typed: false,
        });
        self.notice = None;
    }

    /// Handle date-field keys before the shell listener. Shared datefield routing owns
    /// segment edits; this tile owns commit/cancel. Consumed keys stop propagation,
    /// while Ctrl/Alt/Cmd chords continue to the shell. Shift modifies arrow stepping.
    /// Both direct Enter/Escape and registered commit/cancel actions use the same
    /// editor lifecycle.
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
            typed,
            ..
        }) = self.editor.as_mut()
        else {
            return false;
        };
        if matches!(key, FieldKey::Digit(_) | FieldKey::Backspace) {
            *typed = true;
        }
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

    /// Select a date segment and focus the field's own handle, including when the
    /// editor remained open after focus moved elsewhere.
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
                if editing.bulk.as_ref().is_some_and(|b| text == b.seeded) {
                    // Untouched text: the live steps are the edit. Taken
                    // before the close, which would otherwise undo them.
                    editing.bulk = None;
                    self.close_editor(window, cx);
                    return true;
                }
                if self.selection.is_some() && text == editing.opened {
                    // Untouched over a selection: nothing to write.
                    self.close_editor(window, cx);
                    return true;
                }
                self.commit_cell_edit(cell, labels, &text, window, cx)
            }
            (EditorState::Text(state), EditTarget::Attr { index, column }) => {
                let text = state.read(cx).value().to_string();
                self.commit_attr_edit(index, column, AttrInput::Text(&text), window, cx)
            }
            (EditorState::Date { field, paint, .. }, EditTarget::Attr { index, column }) => {
                // Finish a pending segment digit before committing. Invalid partial
                // dates keep the editor open with a segment-specific refusal. Both
                // keyboard commit routes use this check.
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
                // Finish pending date digits before passing the validated cell date to
                // the shared value commit path.
                if let Err(segment) = field.complete_pending() {
                    self.notice =
                        Some(format!("finish the {} or backspace", segment.name()).into());
                    return true;
                }
                *paint = DateFieldPaint::of(field, self.id.0);
                if self.selection.is_some() {
                    let text = field.date().format("%Y-%m-%d").to_string();
                    if !editing.typed && text == editing.opened {
                        // Untouched over a selection: nothing to write.
                        self.close_editor(window, cx);
                        return true;
                    }
                    return self.commit_bulk(&text, window, cx);
                }
                let value = Value::Date(field.date());
                self.commit_cell_value(cell, labels, value, window, cx)
            }
            (EditorState::Text(state), EditTarget::RowLabel { row, label }) => {
                // Parse typed row labels using the axis type and canonicalize their
                // spelling. A Minted axis has no row-label editor; encountering one is
                // treated as a moved-target refusal.
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

    /// Commit a canonical row label only if the provisional target still occupies its
    /// opening row and the label is unused, including by deleted rows. Rename before
    /// closing so close_editor retains the row, then rebuild and open its first value
    /// cell.
    fn commit_row_label(
        &mut self,
        row: usize,
        label: SharedString,
        new: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model.label(row) != Some(&label) {
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        if self.model.row_of(&new).is_some() {
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
        if let Some(at) = self.model.row_of(&new) {
            self.cursor = Cursor::Cell { row: at, col: 0 };
            self.begin_edit(EditCaret::End, window, cx);
        }
        true
    }

    /// Nudge the open numeric editor by the target's displayed precision without
    /// committing. Cells use their column format; numeric attributes derive precision
    /// from their own text because attributes have no column format. Date fields handle
    /// their own segment stepping. Invalid text remains unchanged with a notice. Return
    /// whether the header needs rebuilding.
    fn nudge(&mut self, steps: i64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(chrome) = self.bulk_step(steps, window, cx) {
            return chrome;
        }
        let Some(editing) = self.editor.as_mut() else {
            return false;
        };
        let state = match &mut editing.state {
            EditorState::Text(state) => state.clone(),
            EditorState::Date { field, paint, .. } => {
                // Insert navigation that reaches a date editor delegates to the field's
                // segment stepping, matching its direct key listener.
                field.step(steps);
                *paint = DateFieldPaint::of(field, self.id.0);
                return self.notice.take().is_some();
            }
        };
        let text = state.read(cx).value().to_string();
        let (ty, precision) = match &editing.target {
            EditTarget::Cell { cell: (_, col), .. } => {
                // Only Number cells support arithmetic nudging. Reject other kinds
                // without changing typed text. This declared-type check also
                // establishes the kind used by the precision lookup below.
                let Some(ty) = declared_type(&self.spec, &self.model, *col) else {
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

    /// Parse cell input before writing: Number uses its declared numeric type, Text is
    /// trimmed with requiredness checked, Date uses ISO text, and Choice uses text.
    /// Date/Choice normally open their specialized fields, but this route still
    /// validates them. Refusal keeps the text editor open; valid values proceed through
    /// commit_cell_value.
    fn commit_cell_edit(
        &mut self,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.selection.is_some() {
            return self.commit_bulk(text, window, cx);
        }
        let value = match self.model.kind_of(cell.1) {
            Some(CellKind::Number(_)) => {
                // `declared_type` answers `Some` for every `Number` column
                // (its match is on the same `kind_of` as this one), so
                // this `else` cannot run — spelled as the moved-grid
                // refusal rather than an `unwrap`, because a panic on the
                // render thread is never the answer.
                let Some(ty) = declared_type(&self.spec, &self.model, cell.1) else {
                    self.close_editor(window, cx);
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                };
                match parse_cell(text, ty) {
                    Ok(parsed) => parsed,
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

    /// Flat-column requiredness comes from ValueColumn. Pivot NULL cells are permitted.
    fn column_required(&self, col: usize) -> bool {
        self.spec
            .flat_columns()
            .get(col)
            .is_some_and(|vc| vc.required)
    }

    /// Commit a validated cell value after checking its opening identity and base.
    /// Document rows write Draft::edits by the cell's document reference, not its
    /// post-insertion grid index. Inserted rows write RowEdit.cells by column label;
    /// Deleted rows refuse changes.
    ///
    /// Write the draft, then re-prepare that one window cell; the index is not
    /// rebuilt. Rebuild instead when the draft was Sent (every cell's sent mark
    /// changes).
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
        let (state, cell_ref) = (self.model.state(cell.0), self.model.cell_ref(cell));
        match state {
            None => {
                self.close_editor(window, cx);
                self.notice = Some(CELL_MOVED.into());
                return true;
            }
            Some(RowState::Deleted) => {
                self.close_editor(window, cx);
                self.notice = Some(DELETED_REFUSED.into());
                return true;
            }
            Some(RowState::Inserted) => {
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
            Some(RowState::Document) => {
                let Some(cell_ref) = cell_ref else {
                    self.close_editor(window, cx);
                    self.notice = Some(CELL_MOVED.into());
                    return true;
                };
                self.draft.set(
                    cell_ref,
                    (labels.0.to_string(), labels.1.to_string()),
                    value,
                    &base,
                );
            }
        }
        // A written value is kept: the close below must never restore the
        // pre-`i` draft over it, even when the write left the draft equal
        // to what the steps had made it. A stepping editor reaches this
        // single-cell commit only after a delivery cleared its selection,
        // which also moves the painted generation, so `undo_steps` would
        // keep the steps anyway; the take states the rule rather than
        // relying on that.
        drop(self.editor.as_mut().and_then(|e| e.bulk.take()));
        self.close_editor(window, cx);
        self.notice = None;
        if was_sent {
            // Leaving Sent changes every cell's sent mark: rebuild, and the
            // install refills the window.
            self.rebuild_model(cx);
            return true;
        }
        // One cell changed. The index holds no cell text, so nothing in it
        // changes: re-prepare that window cell and that row's find text.
        let draft = &self.draft;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().refill_cell(draft, cell);
            cx.notify();
        });
        self.refresh_find_row(cell.0);
        self.sync_cursor(cx);
        true
    }

    /// Parse and commit attribute text using its declared type before mutating the
    /// draft.
    fn commit_attr_edit(
        &mut self,
        index: usize,
        column: SharedString,
        input: AttrInput<'_>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model.header.get(index).map(|h| &h.column) != Some(&column) {
            // Reject a changed attribute target rather than writing to its replacement.
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        }
        // A handle, not a borrow of `self`: the editor closes (`&mut self`)
        // between finding the attribute and writing it.
        let spec = Arc::clone(&self.spec);
        let Some(attr) = spec.header.iter().find(|a| a.column == column.as_ref()) else {
            self.close_editor(window, cx);
            self.notice = Some(CELL_MOVED.into());
            return true;
        };
        let value = match input {
            AttrInput::Value(value) => value,
            AttrInput::Text(text) => match parse_attr(text, attr.ty) {
                Ok(value) => value,
                Err(e) => {
                    // Keep refused text in the focused editor for correction.
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
        self.draft.set_attr(&attr.column, value, &base);
        self.close_editor(window, cx);
        self.notice = None;
        self.rebuild_model(cx);
        true
    }

    /// Blur a focused editor before releasing it. Root can retain a strong input handle
    /// after its element leaves the tree, so dropping alone does not release keyboard
    /// ownership. Never blur another surface when this editor has lost focus.
    ///
    /// Closing an unfinished typed row-label editor removes its provisional row.
    /// Successful commit renames that row first, so the minted target is no longer
    /// present for cancellation to remove.
    ///
    /// Closing a selection editor that stepped undoes its steps: every
    /// commit that keeps them takes the `bulk` out first, so a close here
    /// is a cancel (`escape`, a click elsewhere, the menu, a row verb, a
    /// key switch before the draft is parked).
    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = &self.editor
            && e.state.is_focused(window, cx)
        {
            window.blur(cx);
        }
        let Some(editing) = self.editor.take() else {
            return;
        };
        // After the take, so the rebuild's mirror no longer paints the
        // closed editor in its cell.
        if let Some(bulk) = editing.bulk
            && self.undo_steps(bulk) == StepsUndo::Restored
        {
            self.rebuild_model(cx);
        }
        let EditTarget::RowLabel { label, .. } = editing.target else {
            return;
        };
        let provisional = matches!(
            self.draft.row_state(label.as_ref()),
            Some(RowEdit::Inserted { cells, .. }) if cells.is_empty()
        );
        if provisional {
            let base = self.model.base.clone().unwrap_or_default();
            self.draft.delete_row(label.as_ref(), &base);
            self.rebuild_model(cx);
        }
    }

    // Row insertion and deletion.

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
    ) -> Result<(usize, DocumentBase), String> {
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        if let Some(refusal) = self.held_refusal() {
            return Err(refusal.to_string());
        }
        let Cursor::Cell { row, .. } = self.cursor else {
            return Err(NOT_A_ROW.to_string());
        };
        let base = self.edit_base()?;
        Ok((row, base))
    }

    /// Insert beside the cursor and start entry. Below anchors on the selected row and
    /// rehangs its prior follower beneath the new row. Above a document row uses the
    /// preceding painted row as anchor; above an inserted row takes its anchor and
    /// reanchors the original row beneath it. These links preserve insertion order
    /// through rename and rebase.
    ///
    /// Minted axes open the first value cell. Typed axes first open a provisional
    /// row-label editor: a date field seeded from the configured clock, or blank text.
    /// Label commit renames the row then opens its first cell; cancellation drops it.
    fn insert_row(
        &mut self,
        below: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let (row, base) = self.row_verb_target(window, cx)?;
        let cursor_label = self
            .model
            .label(row)
            .ok_or_else(|| NOT_A_ROW.to_string())?
            .to_string();
        let label = self.draft.mint_label(|l| self.model.row_of(l).is_some());
        // The anchor, and — on `shift+o` over an inserted row — the row
        // to hang off the new one afterwards. On `o`, whatever already
        // hung off the cursor row moves onto the new row FIRST, before
        // the new row itself is anchored there.
        let (anchor, rehang) = if below {
            self.draft
                .rehang_followers(Some(&cursor_label), Some(label.clone()));
            (Some(cursor_label), None)
        } else if self.model.state(row) == Some(RowState::Inserted) {
            let inherited = match self.draft.row_state(&cursor_label) {
                Some(RowEdit::Inserted { after, .. }) => after.clone(),
                _ => None,
            };
            (inherited, Some(cursor_label))
        } else {
            (
                row.checked_sub(1)
                    .and_then(|above| self.model.label(above))
                    .map(|l| l.to_string()),
                None,
            )
        };
        self.draft.insert_row(label.clone(), anchor, &base);
        if let Some(old) = rehang {
            self.draft.reanchor_row(&old, Some(label.clone()));
        }
        self.rebuild_model(cx);
        let Some(at) = self.model.row_of(&label) else {
            // The rebuild refused the grid (its notice says why); the
            // draft still carries the row for the next build to place.
            return Ok(());
        };
        self.cursor = Cursor::Cell { row: at, col: 0 };
        self.notice = None;
        match self.spec.rows.identity {
            RowIdentity::Minted => self.begin_edit(EditCaret::End, window, cx),
            RowIdentity::Typed(ty) => self.begin_label_edit(at, label.into(), ty, window, cx),
        }
        Ok(())
    }

    /// Open the provisional row's label editor for a Typed axis. Date starts at today
    /// on the configured clock; other types start as empty text. Focus its own input so
    /// bare keys reach entry.
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
            bulk: None,
            opened: String::new(),
            typed: false,
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
        let label = self
            .model
            .label(row)
            .ok_or_else(|| NOT_A_ROW.to_string())?
            .to_string();
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

    /// Toggle the action menu. Close a picker/choice through the focus-aware popup
    /// closer and cancel any editor before opening the menu. Switching from a picker
    /// continues into menu construction; it does not merely dismiss the picker.
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
                upload_built: true,
                policy: self.policy,
                kind_title: &self.spec.title,
                kind_actions: &self.spec.actions,
            },
            self.clock,
        );
        self.popup = Some(Popup::Menu(Menu::new(rows, &self.chords)));
        cx.notify();
    }

    /// Close any popup. Blur picker/choice input only when it owns focus, then drop it;
    /// an orphaned popup must not blur the shell's command field or another tile.
    /// Refresh the delegate's choice mirror because outside-click closure has no
    /// dispatch tail to do so.
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

    /// A hover over painted picker row `row` — the mouse form of
    /// `up`/`down`; change-only, as the menu's hover is.
    pub(crate) fn picker_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(Popup::Picker(p)) = &mut self.popup else {
            return;
        };
        if p.rows.highlighted() == row || !p.rows.set_highlighted(row) {
            return;
        }
        cx.notify();
    }

    // ---- the underlying picker -----------------------------------

    /// The shell created this panel through `add_tile` and it is focused: a
    /// panel with no underlying is useless, so ask for one at once. A
    /// launched panel that already has a key (a context launch, a
    /// duplicate) does nothing. Escape leaves the empty panel, as `u` does.
    pub(crate) fn launched(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.key.is_none() && self.popup.is_none() {
            self.open_picker(window, cx);
        }
    }

    /// Open the underlying picker with ranked catalog keys and prepared parked-draft
    /// marks. Dirty drafts do not block it: selecting a key parks the current work.
    /// Request a fresh catalog on entry and incorporate later catalog changes through
    /// the diagnostics observer.
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

    /// Before picker commit, reconcile the ranking with the input's current text;
    /// programmatic set_value emits no Change event. Unchanged text preserves the
    /// highlight, and a changed query retains key identity where possible. With no
    /// painted option, leave the picker open.
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

    /// Resolve the painted, window-relative picker index, close its input, then switch
    /// keys through the same route as key/underlying commands.
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

    // Choice-cell typeahead and stepping.

    /// Open a choice typeahead from the column vocabulary, seeded with its current cell
    /// text. Close any existing popup through the focus-aware path before installing
    /// the new field.
    fn open_choice(
        &mut self,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
        current: &str,
        options: Arc<[String]>,
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
        if self.selection.is_some() && option == c.opened {
            // `enter` on the value the cell already holds, over a
            // selection: nothing to write. A row click stays a pick.
            self.close_popup_with_window(window, cx);
            return true;
        }
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

    /// The pick itself. Under a live selection the option goes to
    /// [`Self::commit_bulk`], which writes it to every accepting member
    /// and closes the popup itself, or leaves it open when nothing
    /// accepts. Otherwise close the popup FIRST (blur, then drop — its
    /// field is done being useful the moment an option is chosen, as
    /// `picker_pick` closes before `set_key`), then write through
    /// [`Self::commit_cell_value`], the one door every cell value lands
    /// through, so the identity check and the patch are spelled once.
    fn pick_option(&mut self, option: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Popup::Choice(c)) = &self.popup else {
            return false;
        };
        let (cell, labels) = c.target();
        if self.selection.is_some() {
            return self.commit_bulk(&option, window, cx);
        }
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
        if let Some(refusal) = self.held_refusal() {
            return Err(refusal.to_string());
        }
        self.edit_base()?;
        let Cursor::Cell { row, col } = self.cursor else {
            return Err("step needs a grid cell — the cursor is in the header".to_string());
        };
        if self.model.state(row) == Some(RowState::Deleted) {
            return Err(DELETED_REFUSED.to_string());
        }
        let Some(CellKind::Choice(options)) = self.model.kind_of(col) else {
            return Err("not a choice cell".to_string());
        };
        let options = Arc::clone(options);
        let len = options.len() as isize;
        if len == 0 {
            // A spec declaring `choices: Some(&[])` — nothing to step
            // through, and `rem_euclid(0)` below would panic.
            return Err("the column declares no options".to_string());
        }
        let current = self.model.format_cell(&self.draft, row, col);
        let next = match options.iter().position(|o| o.as_str() == current.as_ref()) {
            Some(i) => (i as isize + delta).rem_euclid(len),
            None if delta > 0 => 0,
            None => len - 1,
        };
        let cell = (row, col);
        let labels = self.model.label_of(cell);
        self.commit_cell_value(
            cell,
            labels,
            Value::Utf8(options[next as usize].clone()),
            window,
            cx,
        );
        Ok(())
    }

    /// Discard draft edits and return to the newest usable document. Clear retained
    /// base state as well as Draft so a Clean panel cannot keep painting an obsolete
    /// base. Report the no-edits case without pretending a change occurred.
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
        if matches!(self.notice.as_deref(), Some(BEHIND_REFUSED | ECHO_REFUSED)) {
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
    /// shape a trader nudges) or down its column on request. With no axis
    /// word and a selection live, every selected number instead
    /// (`bump_selection`); a typed `row` or `col` keeps its own meaning.
    ///
    /// Each cell's CURRENT painted value is what is added to, which is the
    /// draft's own value wherever one exists, so two bumps compose instead
    /// of the second reading through to the document underneath
    /// (`current_numeric`). A NULL cell is skipped: there is no number to
    /// add to, and inventing one would put a value on screen the document
    /// never carried. A cell whose column is not `CellKind::Number` (a
    /// flat panel's date or status column) is skipped the same way —
    /// `:bump` is arithmetic, and a schedule's non-numeric columns have
    /// nothing to add to either.
    fn bump(
        &mut self,
        delta: f64,
        axis: Option<BumpAxis>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if axis.is_none() && self.selection.is_some() {
            return self.bump_selection(delta, cx);
        }
        let axis = axis.unwrap_or_default();
        if let Some(refusal) = self.held_refusal() {
            return Err(refusal.to_string());
        }
        self.edit_base()?;
        let Cursor::Cell { row, col } = self.cursor else {
            // Bump requires grid cells; the attribute strip has no row or column
            // target.
            return Err("bump needs a grid cell — the cursor is in the header".to_string());
        };
        // Reject a bump of a Deleted row. Column bumps skip Deleted rows individually.
        if matches!(axis, BumpAxis::Row) && self.model.state(row) == Some(RowState::Deleted) {
            return Err(DELETED_REFUSED.to_string());
        }
        let mut skipped = 0usize;
        // Each cell to bump as (model cell, its current value, its declared type).
        let values: Vec<((usize, usize), Value, ColumnType)> = match axis {
            // A row bump walks the LADDER: the leading `slice_columns`
            // cells are the term's own forward/atm/skew, and bumping a
            // term's vols must not move its forward with them. A column
            // bump on a slice column still bumps that column down every
            // term, which is what a bump on `fwd` means.
            BumpAxis::Row => (self.model.slice_columns..self.model.columns.len())
                .filter_map(|ci| {
                    if !matches!(self.model.kind_of(ci), Some(CellKind::Number(_))) {
                        skipped += 1;
                        return None;
                    }
                    self.current_numeric(row, ci)
                        .map(|v| ((row, ci), v, self.column_type(ci)))
                })
                .collect(),
            BumpAxis::Col => {
                if !matches!(self.model.kind_of(col), Some(CellKind::Number(_))) {
                    return Err("not a numeric column".to_string());
                }
                let ty = self.column_type(col);
                (0..self.model.len())
                    .filter(|&ri| self.model.state(ri) != Some(RowState::Deleted))
                    .filter_map(|ri| self.current_numeric(ri, col).map(|v| ((ri, col), v, ty)))
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
        self.write_steps(
            values
                .into_iter()
                .map(|(cell, v, ty)| (cell, v, ty, delta))
                .collect(),
        )?;
        self.rebuild_model(cx);
        self.changed(cx);
        Ok(())
    }

    /// A model cell's current number: the draft's own value where one
    /// exists, else the document's — `value_at` reads both. `None` for NULL,
    /// text, a date, or out of range.
    pub(super) fn current_numeric(&self, row: usize, col: usize) -> Option<Value> {
        match self.model.value_at(&self.draft, row, col)? {
            value @ (Value::F64(_) | Value::I64(_)) => Some(value),
            Value::Utf8(_) | Value::Date(_) => None,
        }
    }

    /// A model column's declared type, which picks a step's arithmetic.
    /// Flat panels follow `flat_columns` order; pivot values use
    /// `value_type`, while a leading slice-value cell (fwd/atm/skew) is F64.
    pub(super) fn column_type(&self, col: usize) -> ColumnType {
        match self.spec.columns {
            Columns::Values(_) => self
                .spec
                .flat_columns()
                .get(col)
                .map(|vc| vc.ty)
                .unwrap_or(self.spec.value_type),
            Columns::Axis(_) if col < self.model.slice_columns => ColumnType::F64,
            Columns::Axis(_) => self.spec.value_type,
        }
    }

    /// Rebase edits by labels onto the newest document and begin painting it. Build the
    /// label map with an empty draft so inserted rows and painted edit values cannot
    /// affect target identity. A Sent draft can rebase only after a differing echo; the
    /// result becomes unsent Editing state.
    fn rebase(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.draft.is_behind() && !self.draft.is_sent() {
            return Err(NOT_BEHIND.to_string());
        }
        // Sent without a differing echo has no newer target. Refuse a rebase that would
        // make already-submitted edits sendable on the same generation.
        if self.draft.is_sent() && !matches!(self.echo, Some(Echo::Differs { .. })) {
            return Err(REBASE_AWAITING_ECHO.to_string());
        }
        // Capture outgoing group sizes only from the actual base snapshot. Build
        // failure or a newer fallback leaves stored group metadata intact without
        // blocking rebase. Temporarily take the draft so the capture helper can also
        // borrow tile state.
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
        let newer_model = MatrixIndex::build(&snapshot, &self.spec, &Draft::default())?;
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

    /// Find's starting grid row, or zero when invoked from the attribute strip. Find
    /// searches document rows and never attribute values.
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

    /// Yank prepared display text: a cell, a tab-separated row, or a newline-separated
    /// column. Include row labels only when painted; NULL remains empty. In the
    /// attribute strip, cell/row yank use the value or label/value pair, while column
    /// yank has no target.
    fn yank_text(&self, what: Yank) -> Option<String> {
        match self.cursor {
            Cursor::Cell { row: r, col: c } => {
                let label = self.model.label(r)?;
                if c >= self.model.columns.len() {
                    return None;
                }
                Some(match what {
                    Yank::Cell => self.model.format_cell(&self.draft, r, c).to_string(),
                    Yank::Row => {
                        // The copied line is what the trader SEES: the
                        // label leads it only where the label column is
                        // painted (`RowLabel::Shown`).
                        let label = self.spec.rows.shown().then(|| label.to_string());
                        label
                            .into_iter()
                            .chain(
                                (0..self.model.columns.len()).map(|col| {
                                    self.model.format_cell(&self.draft, r, col).to_string()
                                }),
                            )
                            .collect::<Vec<_>>()
                            .join("\t")
                    }
                    // Every row, on screen or not, formatted on demand.
                    Yank::Col => (0..self.model.len())
                        .map(|row| self.model.format_cell(&self.draft, row, c).to_string())
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
    /// and a hidden `dividend_id` is not that. Prepared once per index
    /// build, on first use.
    fn search_text(&mut self) -> &[String] {
        if self.search_text.is_none() {
            #[cfg(test)]
            {
                self.search_builds += 1;
            }
            let text = if self.spec.rows.shown() {
                self.model.rows().map(|r| r.label.to_string()).collect()
            } else {
                (0..self.model.len())
                    .map(|row| row_search_text(&self.model, &self.draft, row))
                    .collect()
            };
            self.search_text = Some(text);
        }
        self.search_text.as_deref().unwrap_or_default()
    }

    /// A one-cell edit changes one row's painted text; under a hidden label
    /// that row's search string is its cells, so re-prepare it.
    fn refresh_find_row(&mut self, row: usize) {
        if self.spec.rows.shown() {
            return;
        }
        if let Some(text) = self.search_text.as_mut()
            && row < text.len()
        {
            text[row] = row_search_text(&self.model, &self.draft, row);
        }
    }

    pub(crate) fn start_fuzzy_find(
        &mut self,
        results: gpui::WeakEntity<geode_shell::fuzzyfind::FuzzyFind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_popup_with_window(window, cx);
        self.fuzzy_find = Some(results.clone());
        if let Some(results) = results.upgrade() {
            cx.observe(&results, |_, results, cx| {
                if !results.read(cx).is_active() {
                    cx.notify();
                }
            })
            .detach();
            cx.observe_release(&results, |_, _, cx| cx.notify())
                .detach();
        }
        cx.notify();
        let delegate = self.table.read(cx).delegate();
        let columns = (0..delegate.columns_count(cx))
            .map(|ix| delegate.column(ix, cx))
            .collect();
        let table = self.table.clone();
        // The result table formats only the rows it shows, as it reports
        // them, against the index and draft `/` opened on; its paint only
        // reads them.
        let find = Rc::new(RefCell::new(crate::delegate::FindCells::new(
            self.model.clone(),
            self.draft.clone(),
        )));
        #[cfg(test)]
        {
            self.find_cells = Some(find.clone());
        }
        let shown = find.clone();
        let header_table = self.table.clone();
        let _ = results.update(cx, |results, cx| {
            results.set_table(
                columns,
                move |col, window, cx| {
                    header_table.update(cx, |table, cx| {
                        table
                            .delegate_mut()
                            .render_th(col, window, cx)
                            .into_any_element()
                    })
                },
                move |row, col, cx| {
                    table.update(cx, |table, cx| {
                        table
                            .delegate()
                            .render_find_cell(&find.borrow(), row, col, cx)
                    })
                },
                move |rows, _| shown.borrow_mut().show(rows),
                window,
                cx,
            )
        });
        let items = self
            .model
            .rows()
            .map(|r| (r.index, r.label.clone()))
            .map(|(_, identity)| {
                let label = identity.to_string();
                let document = self.model.key.clone();
                let tile = cx.entity().downgrade();
                geode_shell::fuzzyfind::FindItem::new(
                    format!("{document:?}:{identity}"),
                    label,
                    "",
                    move |query, _, cx| {
                        tile.update(cx, |tile, cx| {
                            if tile.model.key != document {
                                return Err("The document changed. Search again.".to_string());
                            }
                            let row = tile.model.row_of(&identity).ok_or_else(|| {
                                "This row is no longer available. Search again.".to_string()
                            })?;
                            let origin = tile.cursor;
                            tile.set_cursor_row(row);
                            tile.clamp_cursor();
                            tile.find = Some(FindState {
                                origin,
                                committed: (!query.is_empty()).then(|| query.to_string()),
                            });
                            tile.sync_cursor(cx);
                            cx.notify();
                            Ok(())
                        })
                        .map_err(|_| "The tile is closed".to_string())?
                    },
                )
            })
            .collect();
        let _ = results.update(cx, |results, cx| results.replace_items(items, cx));
    }

    /// Close popups at the find entry point. Find is shell-routed and bypasses the tile
    /// dispatch guard; use the focus-aware closer so a picker can be dismissed without
    /// blurring the shell field now receiving query text.
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
                // Every keystroke searches from the ORIGIN, not from
                // wherever the previous one landed: that is what makes a
                // lengthening query walk forward and a shortened one walk
                // back (vim's incsearch).
                let hit = find_match(self.search_text(), origin, FindDirection::Forward, &query);
                if let Some(row) = hit {
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
        // Find only moves the cursor. Notify without reformatting header chips on every
        // query character.
        cx.notify();
    }

    /// `n`/`N`, counted — the committed query stepped from the cursor,
    /// wrapping, exactly as the blotter's own repeat does.
    fn repeat_find(&mut self, dir: FindDirection, count: Option<u32>) {
        let Some(query) = self.find.as_ref().and_then(|f| f.committed.clone()) else {
            return;
        };
        let len = self.search_text().len();
        if len == 0 {
            return;
        }
        let mut at = self.cursor_row();
        for _ in 0..count.unwrap_or(1).max(1) {
            let start = match dir {
                FindDirection::Forward => (at + 1) % len,
                FindDirection::Backward => (at + len - 1) % len,
            };
            match find_match(self.search_text(), start, dir, &query) {
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
        // Request catalog refresh when handling a key/underlying command, even if one
        // is already held. Completions are read-only and cannot submit that refresh; a
        // held catalog proves availability, not freshness. Visibility requests only
        // need to establish that a catalog exists.
        if matches!(line.split_whitespace().next(), Some("underlying" | "key")) {
            self.request_catalog(cx);
        }
        let command = commands::parse(line)?;
        // After successful parsing, close popups for every command except Menu, which
        // must inspect the current popup to toggle it. Parse failures leave state
        // alone. This explicit close covers shell command routing outside tile
        // dispatch.
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
            Command::Upload(target) => self.arm_upload(target, window, cx),
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
            Command::Autosize { reset } => self
                .autosize_columns(reset, window, cx)
                .map_err(str::to_string),
        }
    }

    /// Fit every column to its header and the prepared text of the rows in
    /// the window — what the table last showed (`reset`: drop the fitted
    /// widths), then refresh so the table
    /// re-reads `column()`. The one route behind both `:autosize` and the
    /// shell's `tile::autosize_columns`; measured on the UI thread at the
    /// window's current rem, never in render. The widths persist in the
    /// session record; a column a later document lacks is ignored and a new
    /// one gets the default width.
    ///
    /// With no rows (no document yet, or an empty one) a fit refuses with
    /// [`NOTHING_TO_FIT`] and the widths already held stay; a reset always
    /// runs.
    pub fn autosize_columns(
        &mut self,
        reset: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), &'static str> {
        let fitted = if reset {
            FittedWidths::new()
        } else {
            let metrics = FitMetrics::xsmall_mono(window.rem_size());
            self.table
                .read(cx)
                .delegate()
                .fit_columns(&metrics)
                .ok_or(NOTHING_TO_FIT)?
        };
        self.table.update(cx, |t, cx| {
            t.delegate_mut().fitted = fitted;
            t.refresh(cx);
        });
        cx.notify();
        Ok(())
    }

    /// Set the policy used by a future new generation. Existing Behind state and
    /// current pixels remain unchanged; notify for session persistence. Command, menu,
    /// and palette routes close an existing popup before reaching this setter.
    fn set_policy(&mut self, policy: UpdatePolicy, cx: &mut Context<Self>) {
        self.policy = policy;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn policy(&self) -> UpdatePolicy {
        self.policy
    }

    /// Read or set an attribute using the editor's declared-type parsing and guards.
    /// Without a value, return the current value through the command notice/error
    /// channel because no mutation occurred.
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
                .map(|a| a.column.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("no attribute '{attr}' ({names})")
        };
        match value {
            None => {
                // Without a document, report missing data before looking for an
                // attribute value.
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
                if let Some(refusal) = self.held_refusal() {
                    return Err(refusal.to_string());
                }
                let header_attr = self
                    .spec
                    .header
                    .iter()
                    .find(|a| a.column == attr)
                    .ok_or_else(unknown)?;
                let parsed = parse_attr(&value, header_attr.ty)?;
                let base = self.attr_edit_base()?;
                self.draft.set_attr(&header_attr.column, parsed, &base);
                self.rebuild_model(cx);
                self.changed(cx);
                Ok(())
            }
        }
    }

    /// Switch document keys by parking the outgoing draft as portable label-pair TOML
    /// and restoring any incoming draft through the session-resolution path. Its first
    /// usable delivery follows Hold, even under an automatic update policy.
    ///
    /// Cancel uncommitted editor text before parking. Otherwise a same-label target
    /// could commit that text into the next key's draft. Drafts without an outgoing key
    /// remain available for the first named key to claim.
    fn set_key(&mut self, key: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.key.as_deref() == Some(key.as_slice()) {
            return;
        }
        if self.editor.is_some() {
            self.close_editor(window, cx);
        }
        // The next underlying's rows can carry the same labels; a
        // selection must never carry over to another document.
        self.clear_selection();
        // A question about the outgoing document must not stand over the
        // incoming one: withdrawn unanswered, as a delivery withdraws it.
        let _ = confirm::withdraw(self, cx);
        // A parked draft restores as Editing and stops comparing the outgoing upload's
        // echo. Clear its submitted payload and error state; retain an in-flight
        // request so its eventual outcome can be reported by key.
        self.sent = None;
        self.submitted = None;
        self.upload_error = None;
        // Park under the outgoing key, retaining a keyless draft for the first key to
        // claim. Capture group sizes before resetting snapshots, and only when the
        // outgoing painted snapshot is the true draft base.
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
        self.title = Self::compute_title(&self.spec, self.key.as_deref());
        // What an echo said belongs to the outgoing underlying's upload.
        self.echo = None;
        self.snapshot = None;
        self.base_snapshot = None;
        // A different document is a different question: drop what was held
        // for the old key (a key change bumps no frame counter, so a later
        // flip would otherwise promote it under the new key's header),
        // forget what was asked, and advance the tag even while hidden so
        // the old key's late delivery cannot enter the restored draft.
        self.following.reset();
        self.publication = None;
        self.cursor = Cursor::Cell { row: 0, col: 0 };
        self.last_grid_col = 0;
        self.rebuild_model(cx);
        if self.visible {
            self.requery(cx);
        } else {
            self.changed(cx);
        }
    }

    /// Prepare picker marks from parked draft count phrases once per open. The current
    /// draft is excluded because the header already identifies its edits.
    fn parked_marks(&self) -> BTreeMap<String, String> {
        self.parked
            .iter()
            .map(|(key, table)| (display_key(key), Draft::from_toml(table).count_phrase()))
            .collect()
    }

    pub fn completions(&self, line: &str, cursor: usize, cx: &App) -> Vec<String> {
        let targets: Vec<String> = self.egress_targets.iter().map(|t| t.to_string()).collect();
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
            // Offer Sent rebase only when a differing echo supplies a newer target.
            self.draft.is_behind()
                || (self.draft.is_sent() && matches!(self.echo, Some(Echo::Differs { .. }))),
            &attrs,
            &targets,
        )
    }

    /// Catalog document keys decoded from dataset partition IDs into typeable key text.
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

    /// Whether the held catalog contains this dataset, independent of freshness.
    /// Visibility uses this availability check; key commands request refresh even when
    /// the answer is already present.
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

    pub fn serialize(&self, cx: &App) -> toml::Table {
        let mut t = toml::Table::new();
        if let Some(key) = &self.key {
            t.insert(
                "underlying".into(),
                toml::Value::Array(key.iter().map(|s| toml::Value::String(s.clone())).collect()),
            );
        }
        // Serialize current and parked unsent work as per-key label-pair tables. Dotted
        // display keys are quoted by TOML. Preserve the legacy bare draft only for
        // nonempty work without an underlying key to file it under.
        let mut drafts = toml::Table::new();
        if !self.draft.is_empty() {
            // Capture actual-base group sizes on a draft clone for the session record.
            // serialize takes &self, so live state stays unchanged while restored work
            // keeps the group metadata needed to detect changed same-day ordinals.
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
        if let Some(w) = widths_to_toml(&self.table.read(cx).delegate().fitted) {
            t.insert(SESSION_KEY.into(), w);
        }
        t
    }

    // ---- test accessors ---------------------------------------------

    #[cfg(test)]
    pub(crate) fn model(&self) -> &MatrixIndex {
        &self.model
    }

    /// One cell as a reader sees it, through the installed index and draft.
    #[cfg(test)]
    pub(crate) fn cell_at(&self, row: usize, col: usize) -> crate::core::Cell {
        self.model
            .cell(&self.draft, row, col)
            .expect("a cell in range")
    }

    #[cfg(test)]
    pub(crate) fn draft(&self) -> &Draft {
        &self.draft
    }

    /// State of a model row, or None beyond the grid.
    #[cfg(test)]
    pub(crate) fn row_state_at(&self, row: usize) -> Option<RowState> {
        self.model.state(row)
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Whether this panel considers itself to have an outstanding
    /// question — `false` is what makes the next frame change a real
    /// retry (the refusal rule).
    #[cfg(test)]
    pub(crate) fn acted_is_none(&self) -> bool {
        self.following.acted().is_none()
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

    /// Whether either editor form targets a provisional row label.
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

    /// Test access to whether a choice-cell popup is open.
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
            Some(Popup::Menu(m)) => m.highlighted(),
            _ => None,
        }
    }

    /// The open menu's action rows as `(title, checked)`, in order — what
    /// a policy test reads to say which row carries the tick.
    #[cfg(test)]
    pub(crate) fn menu_checks(&self) -> Vec<(String, Option<bool>)> {
        match &self.popup {
            Some(Popup::Menu(m)) => m
                .rows()
                .iter()
                .filter_map(|r| r.action().map(|a| (a.title().to_string(), a.tick())))
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
    pub(crate) fn upload_prompt(&self) -> Option<&str> {
        self.pending_upload
            .as_ref()
            .map(|c| c.prompt_text().as_ref())
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

    /// Header text at an injected time, allowing deterministic stale-threshold checks.
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

    /// The header's health chip, if a source feeding the panel's dataset
    /// is unhealthy.
    #[cfg(test)]
    pub(crate) fn health_chip(&self) -> Option<&geode_tile::header::HealthChip> {
        self.health.chip()
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

/// Whether two drafts hold the same unsent work — cell edits, attribute
/// edits and row inserts/deletes — whatever their state or base. What
/// "the next edit" means to the upload error. Not a test of "the same
/// document": the confirm and the `Ok` outcome compare whole drafts,
/// since a rebase moves `base` and nothing else.
fn same_edits(a: &Draft, b: &Draft) -> bool {
    a.edits == b.edits && a.attrs == b.attrs && a.rows == b.rows
}

/// A historical as-of in the trader's clock, the way the as-of chip
/// spells one: `HH:MM` today, `YYYY-MM-DD HH:MM` on any other day.
fn as_of_text(
    at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    clock: geode_core::clock::Clock,
) -> String {
    let local = clock.local(at);
    if local.date_naive() == clock.local(now).date_naive() {
        clock.hm(at)
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
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

/// One row's painted cells joined by spaces: a hidden-label panel's search string.
fn row_search_text(model: &MatrixIndex, draft: &Draft, row: usize) -> String {
    (0..model.columns.len())
        .map(|col| model.format_cell(draft, row, col))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Declared type for Number cells, shared by commit and nudge. Other CellKinds return
/// None. Flat columns use their own ValueColumn type; pivot columns use the panel value
/// type, with slice values handled separately. Explicit model/spec arguments permit
/// access while the editor is mutably borrowed.
fn declared_type(spec: &PanelSpec, model: &MatrixIndex, col: usize) -> Option<ColumnType> {
    match model.kind_of(col)? {
        CellKind::Number(_) => Some(match &spec.columns {
            Columns::Axis(_) => spec.value_type,
            Columns::Values(cols) => cols.get(col).map_or(spec.value_type, |vc| vc.ty),
        }),
        CellKind::Date | CellKind::Text | CellKind::Choice(_) => None,
    }
}

impl MenuHost for MarketDataTile {
    /// A disabled row's reason becomes the notice and the menu stays; an
    /// enabled row closes the menu and dispatches through the key's route.
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Popup::Menu(m)) = &self.popup else {
            return;
        };
        match m.pick(index) {
            Some(Err(reason)) => {
                self.notice = Some(reason);
                self.rebuild_chrome();
                cx.notify();
            }
            Some(Ok(id)) => {
                self.close_popup_with_window(window, cx);
                self.dispatch(&id, None, window, cx);
            }
            None => {}
        }
    }

    /// Change-only: gpui fires this on every pointer move over a row.
    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(Popup::Menu(m)) = &mut self.popup
            && m.highlight(index)
        {
            cx.notify();
        }
    }
}

impl ConfirmHost for MarketDataTile {
    type Payload = PendingUpload;

    fn confirm_slot(&mut self) -> &mut Option<Confirm<PendingUpload>> {
        &mut self.pending_upload
    }

    fn confirmed(&mut self, pending: PendingUpload, _: &mut Window, cx: &mut Context<Self>) {
        self.submit_upload(pending, cx);
    }

    fn cancelled(&mut self, _: PendingUpload, _: &mut Window, cx: &mut Context<Self>) {
        self.notice = Some(UPLOAD_CANCELLED.into());
        self.changed(cx);
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
            self.pending_upload.as_ref(),
            menu_open,
            theme,
            &tones,
            &tile,
            self.id.0,
            self.menu_selector.clone(),
            self.menu_tip_selector.clone(),
            self.state_tip_selector.clone(),
            self.stack.as_ref(),
            self.health.chip(),
            geode_tile::header::Mode::from_key_mode(self.mode()),
        );
        // Anchor the popup at the header's right edge using a positioned sibling and
        // the wrapper's relative coordinate system.
        let header =
            div()
                .relative()
                .w_full()
                .child(header)
                .when_some(self.popup.as_ref(), |el, p| {
                    let popup_el = match p {
                        Popup::Menu(m) => geode_tile::menu::render_menu(
                            m,
                            &self.menu_ids,
                            gpui::Anchor::TopRight,
                            &tile,
                            |t: &mut MarketDataTile, window, cx| {
                                t.close_popup_with_window(window, cx)
                            },
                            cx,
                        )
                        .into_any_element(),
                        Popup::Picker(p) => {
                            render_picker(p, &tile, self.id.0, cx).into_any_element()
                        }
                        // Painted by the delegate, under its own cell
                        // (`MatrixDelegate::render_td`), never off the
                        // header.
                        Popup::Choice(_) => return el,
                    };
                    // Anchored just under the header strip, whose height
                    // this follows (`geode_tile::header::HEADER_HEIGHT`).
                    el.child(
                        div()
                            .absolute()
                            .right_0()
                            .top(scale::design(geode_tile::header::HEADER_HEIGHT))
                            .child(popup_el),
                    )
                });

        // Render the prepared delegate through DataTable. min_h_0 lets the body shrink
        // inside flex layout so scrolling does not push the header out of view.
        let search = self
            .fuzzy_find
            .as_ref()
            .and_then(|r| r.upgrade())
            .filter(|r| r.read(cx).is_active());
        let body = div().flex_1().min_h_0().w_full().child(match &search {
            Some(results) => results.clone().into_any_element(),
            None => DataTable::new(&self.table)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false)
                .into_any_element(),
        });

        // A pointer press anywhere on the tile cancels an armed `:upload`
        // confirm (the door's capture-phase press; the press still does
        // what it would have done).
        let root = v_flex()
            .size_full()
            .debug_selector(|| format!("tile-content-{}", self.id.0));
        confirm::cancel_on_press(root, self.pending_upload.is_some(), &tile)
            .child(header)
            .child(body)
            .when(search.is_some(), |el| {
                el.pb(scale::design(geode_shell::fuzzyfind::FOOTER_HEIGHT))
            })
            // The extent readout, only while a selection is live — the
            // strip's own `aggregate-extent` element, with no totals: a
            // vol or forward ladder does not add up.
            .when_some(
                self.selection_extent.as_ref().filter(|_| search.is_none()),
                |el, extent| {
                    el.child(
                        h_flex()
                            .w_full()
                            .h(scale::design(FOOTER_HEIGHT))
                            .items_center()
                            .px_2()
                            .text_xs()
                            .border_t_1()
                            .border_color(theme.border)
                            .child(aggregates::strip(Some(extent), &[], &[], theme)),
                    )
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use crate::content::MarketDataFactory;
    use crate::core::DraftState;
    use crate::core::draft::RowEdit;
    use crate::core::spec::{RowAxis, RowIdentity, RowLabel, ValueColumn};
    use crate::core::test_fixtures;
    use crate::core::test_fixtures::{CVI, DIVIDEND, at};
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
    use geode_shell::diagnostics::{Diagnostics, Health, SourceSummary};
    use geode_shell::frame::{FLIP_DEADLINE, Frame, FrameRef, Publish};
    use geode_shell::module::{Delivery, FindEvent, ModuleFactory, TileContent};
    use geode_shell::shell::chip;
    use geode_shell::tiling::TileId;
    use geode_shell::tiling::WorkspaceIx;
    use gpui::{Entity, Window};

    /// Header tones derived by this tile must meet 3:1 contrast on every bundled
    /// window background. Plain and quiet-time text use the theme's unmodified
    /// muted foreground and are outside this test's contrast floor.
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
                for tone in [Tone::Key, Tone::Warn] {
                    let colour = tone_colour(tone, theme, &floored);
                    let ratio = contrast_ratio(to_rgb(colour), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {tone:?} at {ratio:.2}:1"));
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

    /// The memo re-derives only when one of its inputs moves: the same
    /// theme leaves it untouched, a theme switch replaces it.
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
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, LazyLock};
    use std::time::{Duration, Instant, SystemTime};

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
            summable: false,
            mixed_flag: None,
        }
    }

    fn provenance(as_of: &str) -> Provenance {
        provenance_gen(as_of, 7)
    }

    fn provenance_gen(as_of: &str, generation: i64) -> Provenance {
        Provenance {
            datasets: vec![Freshness {
                dataset: "cvi_params".into(),
                as_of: Some(as_of.into()),
                generation: Some(generation),
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
        document_with(terms, nodes, provenance(as_of))
    }

    /// [`document_of`] over a caller-supplied provenance.
    fn document_with(terms: &[&str], nodes: &[f64], provenance: Provenance) -> Snapshot {
        document_forward(terms, nodes, provenance, 4500.0)
    }

    /// [`document_with`] with the first term's `forward` at `forward`
    /// (later terms keep their `+ 10` per term step from it).
    fn document_forward(
        terms: &[&str],
        nodes: &[f64],
        provenance: Provenance,
        forward: f64,
    ) -> Snapshot {
        let mut cells: Vec<(String, f64, f64)> = Vec::new();
        let mut slices: Vec<(f64, f64, f64)> = Vec::new();
        for (t, term) in terms.iter().enumerate() {
            for node in nodes {
                let i = cells.len() + 1;
                cells.push(((*term).to_string(), *node, i as f64 / 10.0));
                slices.push((
                    forward + 10.0 * t as f64,
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
            provenance,
        )
    }

    fn cvi(as_of: &str) -> Snapshot {
        document_of(&TERMS, &NODES, as_of)
    }

    /// [`cvi`] with the first term's forward at `forward`.
    fn cvi_with_forward(as_of: &str, forward: f64) -> Snapshot {
        document_forward(&TERMS, &NODES, provenance(as_of), forward)
    }

    /// [`cvi`] as the data tier delivers it for a HISTORICAL request:
    /// `as_of_request` carries the requested instant, as
    /// `query::read::provenance` records it for `AsOf::At`.
    fn cvi_requested_at(as_of: &str, at: chrono::DateTime<chrono::Utc>) -> Snapshot {
        let mut p = provenance(as_of);
        p.as_of_request = Some(at.to_rfc3339());
        document_with(&TERMS, &NODES, p)
    }

    /// Test host rendering one tile beneath Root. Its bubble-phase mouse counter stands
    /// in for shell click-to-focus and drag listeners, verifying that capture handlers
    /// preserve propagation where ordinary tile clicks require it.
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

    /// Flat schedule fixture mixing Date, Number, and Choice columns. Exercises typed
    /// editing and bump guards through the tile rather than only the matrix core.
    fn open_flat(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_spec(cx, &test_fixtures::SCHEDULE, None)
    }

    fn open_spec(
        cx: &mut gpui::TestAppContext,
        spec: &Arc<PanelSpec>,
        restored: Option<toml::Table>,
    ) -> (Harness, gpui::VisualTestContext) {
        open_spec_with_egress(cx, spec, restored, Vec::new())
    }

    /// [`open_spec`] through a factory that knows `egress` — `(target,
    /// documents)` pairs in `egress.toml` order, narrowed to the panel's
    /// own document by the factory exactly as `geode-app` wires it.
    fn open_spec_with_egress(
        cx: &mut gpui::TestAppContext,
        spec: &Arc<PanelSpec>,
        restored: Option<toml::Table>,
        egress: Vec<(String, Vec<String>)>,
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
        let factory =
            MarketDataFactory::new(data.clone(), Arc::clone(spec), Duration::from_secs(15 * 60))
                .with_egress(Arc::new(egress));
        let slot: Rc<RefCell<Option<Built>>> = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let slot = slot.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    // The tile sits in unpinned workspace 1, so the shared lane
                    // is its lane and tests address it as `f.shared()` /
                    // `f.shared_mut()`. A test that pins must reach the tile's
                    // lane through its `FrameRef` instead.
                    let frame =
                        cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                    let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                    let occupant = factory.create(
                        TileId(TILE),
                        restored.as_ref(),
                        FrameRef::new(frame.clone(), WorkspaceIx::FIRST),
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

    /// The production route: `MarketDataFactory::create` narrows
    /// its shared `egress` list to THIS panel's own document
    /// (`targets_for`, pinned in isolation by `geode_marketdata::content`'s
    /// own test) and the tile stores exactly that narrowed list — not the
    /// whole resolved list, and not the other document's targets — the
    /// list `:upload` resolves its target against.
    #[gpui::test]
    fn the_tile_stores_the_targets_its_factory_resolves_for_its_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (data, _rx) = DataHandle::for_tests();
        let egress: Arc<Vec<(String, Vec<String>)>> = Arc::new(vec![
            (
                "sophis".to_string(),
                vec!["cvi_params".to_string(), "dividend_schedule".to_string()],
            ),
            ("bbg".to_string(), vec!["dividend_schedule".to_string()]),
        ]);
        let factory = MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(60))
            .with_egress(egress);
        let slot: Rc<RefCell<Option<Entity<MarketDataTile>>>> = Rc::new(RefCell::new(None));
        let out = slot.clone();
        cx.update(|cx| {
            cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                let frame =
                    cx.new(|_| Frame::new(GroupingSlots::default(), SavedScopes::new(), None));
                let diagnostics = cx.new(|_| Diagnostics::new(LogLevels::default()));
                let occupant = factory.create(
                    TileId(TILE),
                    None,
                    FrameRef::new(frame, WorkspaceIx::FIRST),
                    diagnostics,
                    window,
                    cx,
                );
                let tile = occupant.view.clone().downcast::<MarketDataTile>().unwrap();
                *out.borrow_mut() = Some(tile.clone());
                let host = cx.new(|_| Host {
                    tile,
                    clicks: Rc::new(StdCell::new(0)),
                    moves: Rc::new(StdCell::new(0)),
                    keys: Rc::new(StdCell::new(0)),
                });
                cx.new(|cx| gpui_component::Root::new(host, window, cx))
            })
        })
        .unwrap();

        let tile = slot.borrow_mut().take().expect("the factory built one");
        cx.update(|cx| {
            assert_eq!(
                tile.read(cx).egress_targets,
                vec![SharedString::from("sophis")],
                "CVI's own document ('cvi_params') narrows the shared list to \
                 the one target that accepts it, dropping 'bbg' (dividend only)"
            );
        });
    }

    impl Harness {
        fn command(&self, vcx: &mut gpui::VisualTestContext, line: &str) -> Result<(), String> {
            vcx.update(|window, cx| self.content.command(line, window, cx))
        }
        fn visible(&self, vcx: &mut gpui::VisualTestContext, visible: bool) {
            vcx.update(|_window, cx| self.content.set_visible(visible, cx));
        }
        /// The shell's `launched` call, through the trait door.
        fn launched(&self, vcx: &mut gpui::VisualTestContext) {
            vcx.update(|window, cx| self.content.launched(window, cx));
            vcx.run_until_parked();
        }
        fn dispatch(&self, vcx: &mut gpui::VisualTestContext, verb: &str, count: Option<u32>) {
            let id = ActionId(format!("marketdata::{verb}"));
            vcx.update(|window, cx| self.content.dispatch(&id, count, window, cx));
        }
        /// Dispatch a shared `motion::{id}`, the id the shell's keys send.
        fn motion(&self, vcx: &mut gpui::VisualTestContext, id: &str, count: Option<u32>) {
            let id = ActionId(format!("motion::{id}"));
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
        /// The next DOCUMENT request, skipping any `Cancel` (a close puts
        /// one on the same channel); a test that asserts on a cancel reads
        /// `raw_requests`.
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
        /// Everything on the channel since the last drain, `Cancel` included.
        fn raw_requests(&self) -> Vec<Request> {
            self.rx.try_iter().collect()
        }
        fn versions(&self, vcx: &gpui::VisualTestContext) -> geode_shell::frame::FrameVersions {
            self.frame.read_with(vcx, |f, _| f.shared().versions())
        }
        fn barrier_open(&self, vcx: &gpui::VisualTestContext) -> bool {
            self.frame.read_with(vcx, |f, _| f.barrier_open())
        }
        fn rows(&self, vcx: &gpui::VisualTestContext) -> usize {
            self.tile.read_with(vcx, |t, _| t.model().len())
        }
        /// Keymap context exposed through TileContent, including insert state for open
        /// editors, picker/choice fields, and confirmation.
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
        /// One cell as a reader sees it: its text and whether it reads as
        /// an edit. Read on demand, and checked against the painted window
        /// whenever the cell is in it.
        fn cell(&self, vcx: &gpui::VisualTestContext, row: usize, col: usize) -> (String, bool) {
            self.tile.read_with(vcx, |t, cx| {
                assert_window_paints(t, cx, row, col);
                let c = t.cell_at(row, col);
                (c.text.to_string(), c.edited)
            })
        }
        /// Every cell's text along one row, and down one column, each
        /// checked against the window as [`Self::cell`] is.
        fn row_texts(&self, vcx: &gpui::VisualTestContext, row: usize) -> Vec<String> {
            self.tile.read_with(vcx, |t, cx| {
                (0..t.model().columns.len())
                    .map(|c| {
                        assert_window_paints(t, cx, row, c);
                        t.model().format_cell(t.draft(), row, c).to_string()
                    })
                    .collect()
            })
        }
        fn col_texts(&self, vcx: &gpui::VisualTestContext, col: usize) -> Vec<String> {
            self.tile.read_with(vcx, |t, cx| {
                (0..t.model().len())
                    .map(|r| {
                        assert_window_paints(t, cx, r, col);
                        t.model().format_cell(t.draft(), r, col).to_string()
                    })
                    .collect()
            })
        }
        /// The window cell the table paints at (`row`, `col`); `None` off screen.
        fn painted(&self, vcx: &gpui::VisualTestContext, row: usize, col: usize) -> Option<String> {
            self.tile.read_with(vcx, |t, cx| {
                t.table()
                    .read(cx)
                    .delegate()
                    .window
                    .get(row, col)
                    .map(|c| c.text.to_string())
            })
        }
        fn painted_cell(
            &self,
            vcx: &gpui::VisualTestContext,
            row: usize,
            col: usize,
        ) -> Option<crate::core::MdCell> {
            self.tile.read_with(vcx, |t, cx| {
                t.table().read(cx).delegate().window.get(row, col).cloned()
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

    /// The window paints what a reader sees: inside the window's rows the
    /// prepared cell equals the one prepared on demand from the live index
    /// and draft. Rows off screen are not prepared and not checked.
    fn assert_window_paints(t: &MarketDataTile, cx: &App, row: usize, col: usize) {
        let window = &t.table().read(cx).delegate().window;
        if !window.window().contains(&row) {
            return;
        }
        assert_eq!(
            window.get(row, col),
            t.model().md_cell(t.draft(), row, col).as_ref(),
            "the painted cell ({row}, {col}) is stale"
        );
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

    /// Model replacement must refresh DataTable's cached column groups. The second
    /// delivery changes columns, and painted headers must follow the new model as well
    /// as the delegate's live column count.
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
        h.motion(&mut vcx, "right", Some(2));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 2 }
        );
        h.motion(&mut vcx, "line_start", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
        h.motion(&mut vcx, "left", None);
        assert_eq!(
            h.selection(&vcx),
            (Some(0), Some(1)),
            "`h` at the first value column stays on it — table column 1, never 0"
        );

        h.motion(&mut vcx, "right", Some(2));
        let at = centre_of(&mut vcx, "marketdata-cell-1-0");
        click_at(&mut vcx, at, 1);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 2 },
            "a click on a row label moves the row and leaves the column where it was"
        );
        assert_eq!(h.selection(&vcx), (Some(1), Some(3)));
    }

    /// Double-click selects a value cell, opens its seeded editor, and focuses it. The
    /// field remains focused after drawing while the context reports insert.
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
        h.motion(&mut vcx, "right", Some(2));

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
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
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

    /// Clicking another cell cancels typed editor text before moving selection; it does
    /// not commit and leaves no editor attached to the previous cell.
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

    /// Delegate cursor/editor mirrors supply cell border and editor rendering. Test the
    /// mirror values; actual border appearance requires a display check.
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

        h.motion(&mut vcx, "down", None);
        h.motion(&mut vcx, "right", Some(2));
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

    /// The focused editor is drawn in its target cell so GPUI installs its text-input
    /// handler and accepts typing.
    #[gpui::test]
    fn the_editor_is_painted_in_the_cursor_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "down", None);
        h.motion(&mut vcx, "right", Some(2));
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

    /// Opening the editor leaves a value where it stood: the cell's text
    /// is right-aligned, so the editor's must end at the same right edge,
    /// not start at the left behind the `Input`'s own padding.
    #[gpui::test]
    fn the_editor_keeps_the_value_right_aligned(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "down", None);
        h.motion(&mut vcx, "right", Some(2));
        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        let slot = vcx
            .debug_bounds("marketdata-editor-1-3")
            .expect("the editor paints in the cursor cell");
        let text = h.tile.read_with(&vcx, |t, cx| {
            let Some(Editing {
                state: EditorState::Text(input),
                ..
            }) = &t.editor
            else {
                panic!("a value cell opens a text editor");
            };
            let input = input.read(cx);
            assert!(!input.value().is_empty(), "the edited cell holds a value");
            input
                .range_to_bounds(&(0..input.value().len()))
                .expect("the value is laid out")
        });
        assert!(
            (slot.right() - text.right()).abs() <= gpui::px(1.),
            "the value ends at the cell's right edge ({:?}), as the painted \
             cell's does, not at {:?}",
            slot.right(),
            text.right()
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
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(
                chrono::Utc::now() - chrono::Duration::seconds(secs as i64),
            ));
            f.shared_mut().open_flip(keys, Instant::now());
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
            (t.model().len(), t.model().columns.len(), t.header_texts())
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

    /// A user-layer panel over `cvi_params`, read as the application reads
    /// it (builtin panels beneath, merged, the full structural reader),
    /// paints its own title and formats: the ladder at the user's six
    /// places and `fwd` at three, where the builtin CVI paints four and two.
    #[gpui::test]
    fn a_user_layer_panel_paints_its_own_title_and_formats(cx: &mut gpui::TestAppContext) {
        use crate::core::spec::{BUILTIN_PANELS, builtin_kind_actions};
        use geode_core::config::{Layer, LayerDoc, merge_docs};
        use geode_core::panel::{PANELS_DOC, read_panels};
        const WIDE: &str = r#"config_version = 1

[cvi_wide]
title = "CVI (wide)"
dataset = "cvi_params"
document = "cvi_params"
actions = ["marketdata::cvi_reanchor"]

[cvi_wide.value]
type = "f64"
format = { precision = 6 }

[cvi_wide.rows]
column = "term"
identity = "date"
label = "shown"

[cvi_wide.columns]
axis = "node"

[[cvi_wide.header]]
column = "anchor_date"
label = "anchor"
type = "date"

[[cvi_wide.header]]
column = "spot_ref"
label = "spot"
type = "f64"

[[cvi_wide.slice]]
column = "forward"
label = "fwd"
format = { precision = 3 }

[[cvi_wide.slice]]
column = "atm"
label = "atm"

[[cvi_wide.slice]]
column = "skew"
label = "skew"
"#;
        let builtin = LayerDoc::builtin(PANELS_DOC, BUILTIN_PANELS).unwrap();
        let user = LayerDoc {
            layer: Layer::User,
            file: "panels.toml".into(),
            ..LayerDoc::builtin(PANELS_DOC, WIDE).unwrap()
        };
        let (panels, diags) = read_panels(
            &merge_docs(PANELS_DOC, &[builtin, user]),
            &builtin_kind_actions(),
        );
        assert!(diags.is_empty(), "{diags:?}");
        let spec = panels
            .into_iter()
            .find(|p| p.kind == "cvi_wide")
            .expect("the user panel reads");

        let (h, mut vcx) = open_spec(cx, &Arc::new(spec), None);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));

        let (chips, title) = h.tile.read_with(&vcx, |t, _| (t.header_texts(), t.title()));
        assert!(chips.iter().any(|c| c == "CVI (wide)"), "{chips:?}");
        assert_eq!(title.as_ref(), "CVI (wide) · SPX.Z");
        assert_eq!(
            h.row_texts(&vcx, 0),
            vec![
                "4500.000",
                "0.180000",
                "-1.000000",
                "0.100000",
                "0.200000",
                "0.300000"
            ],
            "fwd at the slice's three places; atm, skew and the ladder at the panel's six"
        );
    }

    /// Stack markers appear first in the header only for a stack with multiple members.
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

    /// The cached title follows key changes, including from an initially unset key.
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

    /// The time chip becomes stale only after the painted source time exceeds the
    /// configured fifteen-minute threshold. The injected clock exercises both sides of
    /// the boundary.
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

    /// AppClock determines the header's source-time zone at construction and after
    /// global changes. BASE is 14:00 UTC, so the hand-written expectations are 23:00 in
    /// Tokyo and 14:00 in UTC.
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
            (
                t.model().len(),
                t.model().base.as_ref().map(|b| b.as_of.clone()),
            )
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
            (t.model().len(), t.notice().map(str::to_string))
        });
        assert_eq!(rows, 2, "last good stays on screen");
        assert_eq!(notice.as_deref(), Some("the document select failed"));
    }

    /// Cursor motion notifies without preparing the header again. Actions that write or
    /// clear a notice rebuild it, including a refused commit and the action that clears
    /// its refusal.
    #[gpui::test]
    fn a_notice_reaches_the_header_and_escape_clears_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);

        let chips = |vcx: &gpui::VisualTestContext| h.tile.read_with(vcx, |t, _| t.header_texts());
        let before = chips(&vcx);
        h.motion(&mut vcx, "down", None);
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

    /// A delivery stays staged while the barrier still waits for this tile. Grid,
    /// header, and source-time chip promote together with the other visible tiles.
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

    /// A key change clears a staged answer even though it bumps no frame counter.
    /// Promotion can run while hidden, so retaining the old key's answer could paint it
    /// under the new key's header.
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

    /// A flip promotes before the visibility check: a panel hidden after it
    /// staged still lands its answer when the barrier releases, rather than
    /// holding a stage nobody promotes until it is shown again.
    #[gpui::test]
    fn a_stage_held_when_the_panel_hides_promotes_on_the_flip(cx: &mut gpui::TestAppContext) {
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
        assert_eq!(h.rows(&vcx), 2, "staged behind the other tile");

        h.visible(&mut vcx, false);
        let now = h.versions(&vcx);
        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.arrived(other, now), "the other tile's answer releases it");
            cx.notify();
        });
        assert_eq!(
            h.rows(&vcx),
            5,
            "a hidden panel still promotes what it staged on the flip"
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

    /// Hiding a panel cancels nothing and forgets nothing: the outstanding
    /// request finishes, its reply paints while the panel is hidden, and
    /// showing it again asks nothing because nothing it follows moved.
    #[gpui::test]
    fn a_panel_hidden_mid_flight_paints_the_reply_and_asks_nothing_on_reshow(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        h.visible(&mut vcx, false);
        assert!(
            !h.raw_requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { .. })),
            "a hide is not a close: nothing is cancelled"
        );
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        assert_eq!(
            h.rows(&vcx),
            2,
            "the reply applies while the panel is hidden"
        );
        h.visible(&mut vcx, true);
        assert!(
            h.document_request().is_none(),
            "nothing it follows moved, so nothing is asked"
        );
        assert_eq!(h.rows(&vcx), 2);
    }

    /// The reply to a question asked before a followed change can land while
    /// the panel is hidden; reshow must still ask again.
    #[gpui::test]
    fn a_followed_change_while_hidden_requeries_on_reshow(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().expect("the first request");
        h.visible(&mut vcx, false);
        let at = chrono::DateTime::parse_from_rfc3339(BASE)
            .unwrap()
            .with_timezone(&chrono::Utc);
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
            cx.notify();
        });
        assert!(
            h.document_request().is_none(),
            "a hidden panel asks nothing"
        );
        h.deliver(&mut vcx, first.tag, Arc::new(cvi(BASE)));
        assert_eq!(
            h.rows(&vcx),
            0,
            "the answer to the as-of it left behind is not applied"
        );
        h.visible(&mut vcx, true);
        let second = h
            .document_request()
            .expect("the as-of moved while hidden: reshow asks again");
        assert!(second.tag > first.tag);
        assert_eq!(second.as_of, geode_core::query::AsOf::At(at));
    }

    /// A reply asked under an as-of the panel moved past while hidden is
    /// dropped, not applied: applying it would run the draft policy (here
    /// `:auto replace`, which throws the edits away) against a document
    /// nobody asked for any more. The reshow asks under the new as-of.
    #[gpui::test]
    fn a_reply_to_an_as_of_left_behind_while_hidden_runs_no_draft_policy(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto replace").unwrap();
        h.with_document_tagged(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.draft().len()), 1);

        // An as-of change while shown: the panel asks again at once.
        let first_at = chrono::Utc::now() - chrono::Duration::days(1);
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut()
                .set_as_of(geode_core::query::AsOf::At(first_at));
            cx.notify();
        });
        let asked = h.document_request().expect("an as-of change requeries");
        // Hidden with that question out, then the as-of moves again.
        h.visible(&mut vcx, false);
        let second_at = chrono::Utc::now() - chrono::Duration::days(2);
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut()
                .set_as_of(geode_core::query::AsOf::At(second_at));
            cx.notify();
        });
        h.deliver(
            &mut vcx,
            asked.tag,
            Arc::new(document_of(&["2026-11-20"], &NODES, NEWER)),
        );

        let (edits, state, source, rows, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().len(),
                t.draft().state.clone(),
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.model().len(),
                t.notice().map(str::to_string),
            )
        });
        assert_eq!(
            source.as_deref(),
            Some(BASE),
            "the old reply is not painted"
        );
        assert_eq!(rows, 2);
        assert_eq!(edits, 1, "and the replace policy never ran");
        assert_eq!(state, DraftState::Editing);
        assert!(
            !notice.as_deref().is_some_and(|n| n.contains("replaced")),
            "nothing was replaced: {notice:?}"
        );

        h.visible(&mut vcx, true);
        let again = h
            .document_request()
            .expect("the as-of moved while hidden: reshow asks again");
        assert!(again.tag > asked.tag);
        assert_eq!(again.as_of, geode_core::query::AsOf::At(second_at));
    }

    /// A panel hidden while enrolled in an open barrier still answers it with
    /// its reply, and promotes on the flip while hidden, so a tab switch in
    /// the middle of a flip never holds the other tiles to the deadline.
    #[gpui::test]
    fn a_panel_hidden_mid_flip_still_answers_the_barrier(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let other = QueryKey(TILE + 1);
        open_barrier_on_as_of(&h, &mut vcx, &[QueryKey(TILE), other], 60);
        let second = h.document_request().unwrap().tag;
        h.visible(&mut vcx, false);
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        let now = h.versions(&vcx);
        assert!(
            !h.frame
                .read_with(&vcx, |f, _| f.barrier_wants(QueryKey(TILE), now)),
            "the reply answered the barrier it was enrolled in, hidden or not"
        );
        assert_eq!(h.rows(&vcx), 2, "held behind the other tile");
        h.frame.update(&mut vcx, |f, cx| {
            assert!(f.arrived(other, now));
            cx.notify();
        });
        assert_eq!(h.rows(&vcx), 5, "and promoted on the flip while hidden");
    }

    /// Closing a panel cancels its request by key and answers the barrier
    /// before its deadline, so the other tiles do not wait it out; a late
    /// reply paints nothing.
    #[gpui::test]
    fn closing_a_panel_mid_flip_cancels_its_request_and_releases_the_barrier(
        cx: &mut gpui::TestAppContext,
    ) {
        // The shell's recorder test pins that removal reaches `closed`;
        // this test pins what `closed` does.
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let first = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, first, Arc::new(cvi(BASE)));
        let opened = Instant::now();
        let at = chrono::Utc::now() - chrono::Duration::seconds(60);
        h.frame.update(&mut vcx, |f, cx| {
            let mut lane = f.shared_mut();
            lane.set_as_of(geode_core::query::AsOf::At(at));
            lane.open_flip([QueryKey(TILE)], opened);
            cx.notify();
        });
        let second = h.document_request().expect("an as-of change requeries").tag;
        h.frame.update(&mut vcx, |f, _| {
            assert!(
                !f.sweep(opened + FLIP_DEADLINE / 2),
                "halfway to the deadline, time alone releases nothing"
            );
        });
        assert!(h.barrier_open(&vcx), "waiting on this panel's reply");
        vcx.update(|_, cx| h.content.closed(cx));
        assert!(
            h.raw_requests()
                .iter()
                .any(|r| matches!(r, Request::Cancel { key } if *key == QueryKey(TILE))),
            "a close cancels the request by key"
        );
        assert!(
            !h.barrier_open(&vcx),
            "the close answered the barrier before its deadline"
        );
        h.deliver(
            &mut vcx,
            second,
            Arc::new(document_of(&["t0", "t1", "t2", "t3", "t4"], &NODES, BASE)),
        );
        assert_eq!(
            h.rows(&vcx),
            2,
            "a late reply to a closed panel paints nothing"
        );
    }

    #[gpui::test]
    fn a_busy_document_refusal_says_busy(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.data.fill_for_tests();
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("document request refused: the data service is busy".to_string())
        );
    }

    /// A refused submission has no future outcome. It answers the barrier immediately
    /// and clears acted so the next frame change retries.
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
            Some("document request refused: the data service has stopped".to_string())
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

    /// A visible tile acknowledges scope/grouping barriers without querying because its
    /// document does not follow those counters. Register the shell-like observer first
    /// so mutation and open_flip precede the tile's observer.
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
            f.shared_mut().set_scope(geode_core::scope::Scope {
                text: Some("spx".into()),
                ..Default::default()
            });
            f.shared_mut().open_flip([QueryKey(TILE)], Instant::now());
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
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
            f.shared_mut().open_flip([QueryKey(TILE)], Instant::now());
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
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
            f.shared_mut().open_flip([QueryKey(TILE)], Instant::now());
            cx.notify();
        });
        let second = h.document_request().unwrap().tag;
        h.deliver_err(&mut vcx, second, "the document select failed");
        assert!(
            !h.frame.read_with(&vcx, |f, _| f.barrier_open()),
            "a failure arrives as surely as a snapshot does"
        );
    }

    /// Switching keys parks pending edits under their underlying and requests the new
    /// document. The header's dirty marker reflects only the current draft; the parked
    /// draft remains available to the picker and session.
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
        let (dirty, len, parked, key) = h.tile.read_with(&vcx, |t, cx| {
            (
                t.header_dirty(),
                t.draft().len(),
                t.parked(),
                t.serialize(cx)["underlying"][0]
                    .as_str()
                    .map(str::to_string),
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

    /// Returning to a parked draft whose document advanced yields Behind with edits
    /// intact and an update time. Restore holds the first usable delivery even under
    /// auto replace; policy acts on the next new generation.
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
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
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
        h.motion(&mut vcx, "right", None);
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

    /// Sessions store the current and parked drafts in separate drafts.<underlying>
    /// entries. Restore installs the selected underlying's draft and keeps the others
    /// parked.
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

        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
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
            h.tile
                .read_with(&vcx, |t, cx| t.serialize(cx)["underlying"][0]
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
        h.motion(&mut vcx, "bottom", None);
        h.motion(&mut vcx, "line_end", None);
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

        h.motion(&mut vcx, "down", Some(3));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 3, col: 0 }
        );
        h.motion(&mut vcx, "right", Some(2));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 3, col: 2 }
        );
        h.motion(&mut vcx, "down", Some(9));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 4, col: 2 },
            "a count past the end clamps"
        );
        h.motion(&mut vcx, "top", None);
        h.motion(&mut vcx, "line_start", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
        h.motion(&mut vcx, "line_end", None);
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
        h.motion(&mut vcx, "bottom", None);
        assert_eq!(
            h.selection(&vcx),
            (Some(terms.len() - 1), Some(LABEL_COL + 1 + SLICE + 2)),
            "G moves the table's selected row, which is what scrolls it into view"
        );
    }

    /// Bare j wraps, counted j clamps, full-page motions move ten rows, and h/l never
    /// wrap.
    #[gpui::test]
    fn a_bare_row_step_wraps_and_the_full_page_keys_move_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        let terms: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        h.deliver(&mut vcx, tag, Arc::new(document_of(&terms, &NODES, BASE)));

        // With attributes present, k on row zero enters the strip. Exercise wrapping
        // through j at the bottom; the cursor unit tests cover k wrapping without
        // attributes.
        let row = |vcx: &gpui::VisualTestContext| match h.tile.read_with(vcx, |t, _| t.cursor()) {
            Cursor::Cell { row, .. } => row,
            Cursor::Attr(_) => panic!("expected a grid cursor"),
        };
        let col = |vcx: &gpui::VisualTestContext| match h.tile.read_with(vcx, |t, _| t.cursor()) {
            Cursor::Cell { col, .. } => col,
            Cursor::Attr(_) => panic!("expected a grid cursor"),
        };
        h.motion(&mut vcx, "bottom", None);
        assert_eq!(row(&vcx), 11);
        h.motion(&mut vcx, "down", None);
        assert_eq!(row(&vcx), 0, "a bare j at the bottom wraps to row 0");
        h.motion(&mut vcx, "up", None);
        assert!(
            matches!(h.tile.read_with(&vcx, |t, _| t.cursor()), Cursor::Attr(0)),
            "a bare k at the top enters the strip, never wraps, while attributes exist"
        );
        h.motion(&mut vcx, "down", None);
        assert_eq!(row(&vcx), 0, "and j returns to the top row");
        h.motion(&mut vcx, "down", Some(20));
        assert_eq!(row(&vcx), 11, "a counted step clamps");
        h.motion(&mut vcx, "page_up", None);
        assert_eq!(row(&vcx), 1, "ctrl+b moves ten");
        h.motion(&mut vcx, "page_down", None);
        assert_eq!(row(&vcx), 11);
        h.motion(&mut vcx, "page_down", None);
        assert_eq!(row(&vcx), 11, "and clamps at the end");
        h.motion(&mut vcx, "line_end", None);
        let last = col(&vcx);
        h.motion(&mut vcx, "right", None);
        assert_eq!(col(&vcx), last, "columns clamp: l at the last column stays");
    }

    /// The panel publishes `grid` (the binding predicate confines the shared
    /// motions to normal and visual modes), and 5G jumps to row 5.
    #[gpui::test]
    fn the_panel_publishes_grid_and_a_counted_g_jumps_to_that_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        let terms: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
        let terms: Vec<&str> = terms.iter().map(String::as_str).collect();
        h.deliver(&mut vcx, tag, Arc::new(document_of(&terms, &NODES, BASE)));
        let ctx = h.tile.read_with(&vcx, |t, _| t.key_context());
        assert!(ctx.has_flag(geode_shell::keymap::GRID));
        h.motion(&mut vcx, "bottom", Some(5));
        assert!(matches!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 4, .. }
        ));
        h.motion(&mut vcx, "top", Some(2));
        assert!(matches!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, .. }
        ));
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
            commands::completions("", 0, &[], false, &[], &[]),
            "the verb position is the pure core's vocabulary"
        );

        // A key command requests a fresh catalog even when one is held. A cached
        // catalog proves availability, not freshness, and new keys can arrive while the
        // tile remains open.
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
        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert_eq!(
            written, restored,
            "the underlying and the draft survive a restart, labels and all"
        );
    }

    /// The legacy bare draft field restores as the current underlying's draft and is
    /// serialized under drafts.<underlying>.
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
            .read_with(&vcx, |t, cx| (t.draft().len(), t.serialize(cx)));
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
        let cell = h.tile.read_with(&vcx, |t, _| t.cell_at(1, SLICE + 1));
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
        h.motion(&mut vcx, "right", Some(SLICE as u32));
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

    #[gpui::test]
    fn fzf_picks_a_row_without_incremental_cursor_movement(cx: &mut gpui::TestAppContext) {
        use geode_shell::fuzzyfind::FuzzyFind;
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let results = vcx.new(|_| FuzzyFind::default());
        assert!(vcx.update(|window, cx| h.content.start_fuzzy_find(
            results.downgrade(),
            window,
            cx
        )));
        results.update(&mut vcx, |results, cx| results.set_query("1120".into(), cx));
        vcx.run_until_parked();
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 0 }
        );
        let item = results.read_with(&vcx, |results, _| results.selected_item().unwrap());
        vcx.update(|window, cx| item.reveal("1120", window, cx))
            .unwrap();
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, col: 0 }
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "search never edits the document"
        );
    }
    /// Display positions the `/` result table painted, in order.
    fn find_positions(vcx: &mut gpui::VisualTestContext, rows: usize) -> Vec<usize> {
        draw(vcx);
        (0..rows)
            .filter(|i| {
                let selector: &'static str = Box::leak(format!("find-result-{i}").into_boxed_str());
                vcx.debug_bounds(selector).is_some()
            })
            .collect()
    }

    /// A 300-row flat schedule, `D000`..`D299`, every amount distinct.
    fn find_schedule(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        let ids: Vec<String> = (0..300).map(|i| format!("D{i:03}")).collect();
        let rows: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), "2026-12-18", i as f64 / 8.0, "declared"))
            .collect();
        h.with_flat_document_with(vcx, test_fixtures::schedule_snapshot(&rows));
    }

    /// Every painted value cell of `rows` carries the grid formatter's
    /// text for its document row.
    fn assert_find_paints_the_formatter(
        h: &Harness,
        vcx: &gpui::VisualTestContext,
        find: &RefCell<crate::delegate::FindCells>,
        rows: &[usize],
    ) {
        let find = find.borrow();
        let painted = find.painted.borrow();
        h.tile.read_with(vcx, |t, _| {
            for &row in rows {
                for c in 0..t.model().columns.len() {
                    // A shown row label sits at table column 0.
                    assert_eq!(
                        painted.get(&(row, c + 1)).map(String::as_str),
                        Some(t.model().format_cell(t.draft(), row, c).as_ref()),
                        "find cell ({row}, {c})"
                    );
                }
            }
        });
    }

    /// `/` formats the rows it paints and no others: the open, a query
    /// that brings new rows into view, a real wheel scroll and a
    /// narrowing to one match each fill exactly the rows entering view
    /// and drop the rest, and every painted cell is the grid formatter's.
    #[gpui::test]
    fn fzf_formats_only_the_rows_it_paints(cx: &mut gpui::TestAppContext) {
        use geode_shell::fuzzyfind::FuzzyFind;
        let (h, mut vcx) = open_flat(cx);
        find_schedule(&h, &mut vcx);
        let cols = h.tile.read_with(&vcx, |t, _| t.model().columns.len());
        let results = vcx.new(|_| FuzzyFind::default());
        assert!(vcx.update(|window, cx| h.content.start_fuzzy_find(
            results.downgrade(),
            window,
            cx
        )));
        vcx.run_until_parked();
        let find = h.tile.read_with(&vcx, |t, _| t.find_cells.clone().unwrap());
        let held = |find: &RefCell<crate::delegate::FindCells>| {
            let find = find.borrow();
            let mut rows: Vec<_> = (0..300).filter(|&r| find.cells().contains(r)).collect();
            rows.sort();
            rows
        };

        // The open: the empty query shows document order.
        let shown = find_positions(&mut vcx, 300);
        assert!(
            shown.len() > 1 && shown.len() < 100,
            "one screenful: {shown:?}"
        );
        assert_eq!(held(&find), shown, "the cache holds the painted rows");
        let opened = find.borrow().fills;
        assert!(
            opened <= geode_tile::grid::FIRST_WINDOW * cols,
            "the open formats at most a first window, not 300 rows: {opened}"
        );
        assert_find_paints_the_formatter(&h, &vcx, &find, &shown);

        // A query that brings rows from far down the document into view.
        results.update(&mut vcx, |results, cx| results.set_query("D29".into(), cx));
        vcx.run_until_parked();
        let positions = find_positions(&mut vcx, 300);
        let rows = held(&find);
        assert_eq!(
            rows.len(),
            positions.len(),
            "rows no longer shown are dropped: {rows:?}"
        );
        assert!(rows.contains(&290) && rows.contains(&299), "{rows:?}");
        let entered = rows.iter().filter(|r| !shown.contains(r)).count();
        assert_eq!(
            find.borrow().fills - opened,
            entered * cols,
            "exactly the entering rows are formatted"
        );
        assert_find_paints_the_formatter(&h, &vcx, &find, &rows);

        // Everything again, then a real wheel scroll down the table.
        results.update(&mut vcx, |results, cx| results.set_query(String::new(), cx));
        vcx.run_until_parked();
        let top = find_positions(&mut vcx, 300);
        assert_eq!(held(&find), top);
        let before = find.borrow().fills;
        let bounds = vcx.debug_bounds("fuzzy-find").expect("the table paints");
        vcx.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-600.))),
            modifiers: gpui::Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        let scrolled = find_positions(&mut vcx, 300);
        assert!(scrolled.first() > top.first(), "scrolled: {scrolled:?}");
        assert_eq!(held(&find), scrolled, "rows scrolled out are dropped");
        let scrolled_in = scrolled.iter().filter(|r| !top.contains(r)).count();
        assert_eq!(
            find.borrow().fills - before,
            scrolled_in * cols,
            "only the rows scrolling in are formatted"
        );
        assert_find_paints_the_formatter(&h, &vcx, &find, &scrolled);

        // One match: the table never reports a one-row range.
        results.update(&mut vcx, |results, cx| results.set_query("D137".into(), cx));
        vcx.run_until_parked();
        assert_eq!(find_positions(&mut vcx, 300), vec![0]);
        assert_eq!(held(&find), vec![137], "the lone match is formatted");
        assert_find_paints_the_formatter(&h, &vcx, &find, &[137]);
    }

    // ---- Cell editing ------------------------------------------------

    /// Editing seeds the field from the cell, commit parses the column's declared type,
    /// and the draft/header record the edit. Assert the typed value as well as markers
    /// so a raw-text write cannot pass.
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
        let (len, base) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().len(),
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
            )
        });
        assert_eq!(len, 1);
        assert_eq!(
            base.as_deref(),
            Some(BASE),
            "recorded against the generation on screen, which is what makes it Behind-able"
        );
    }

    /// Closing a focused Input blurs it before dropping the editor. Root retains the
    /// focused InputState, so removing it from the tree alone leaves a stale focus
    /// handle and prevents the shell's missing-focus recovery. The tile releases focus;
    /// the shell chooses the next target.
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

    /// The painted, focused cell Input accepts characters, exercising the tile's side
    /// of the shell's insert-mode keystroke route.
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
        h.motion(&mut vcx, "right", Some(SLICE as u32));
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

    /// A slice row bump changes the term's value ladder while preserving its
    /// fwd/atm/skew cells. A column bump on fwd changes every term's forward.
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

    /// A row bump on the flat SCHEDULE panel changes only its Number column, amount.
    /// The Date column ex and Choice column status remain unedited.
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

    /// When amount is NULL, a SCHEDULE row bump writes nothing and reports the two
    /// columns skipped for their nonnumeric kinds. The refusal distinguishes kind skips
    /// from the generic no-values case.
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

    /// A column bump on the Choice column status refuses the column outright.
    #[gpui::test]
    fn a_flat_panels_column_bump_on_a_non_numeric_column_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.motion(&mut vcx, "right", Some(2));

        assert_eq!(
            h.command(&mut vcx, "bump 1 col"),
            Err("not a numeric column".to_string())
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "nothing was written"
        );
    }

    /// The shipped DIVIDEND declaration maps amount at index three to F64. A row bump
    /// changes that column alone, skipping the three Date columns and the Choice status
    /// column.
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
    /// not a blanket `Value::F64`: `amt` is
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

    /// A fractional row bump across inserted F64 and I64 cells refuses atomically.
    /// Every candidate value is validated before any write, leaving both cells
    /// unchanged when the I64 value rejects the delta.
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

    /// An integer cell preserves the typed `i64` through commit, whole-number
    /// bump, and upload assembly, including values above exact `f64` precision.
    #[gpui::test]
    fn a_typed_integer_above_2_pow_53_reaches_the_draft_exactly(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::MIXED, None);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::mixed_snapshot(&[("M1", 1.0, 2)])),
        );

        h.tile.update(&mut vcx, |t, cx| t.cursor_to(0, Some(1), cx));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9007199254740993");
        h.dispatch(&mut vcx, "commit", None);

        let values: Vec<Value> = h
            .tile
            .read_with(&vcx, |t, _| t.draft().edits.values().cloned().collect());
        assert_eq!(
            values,
            vec![Value::I64(9007199254740993)],
            "the typed integer, not its f64 rounding"
        );

        // And a whole-number bump of it stays exact.
        h.command(&mut vcx, "bump 1 col").expect("a whole delta");
        let values: Vec<Value> = h
            .tile
            .read_with(&vcx, |t, _| t.draft().edits.values().cloned().collect());
        assert_eq!(values, vec![Value::I64(9007199254740994)]);
    }

    /// Restore resolves labels against the clean document grid before splicing draft
    /// rows. An inserted row, a deleted row, and a D2 cell edit keep their identities
    /// despite shifted painted indices. Chained inserts retain their anchors and order.
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
                .rows()
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

    /// An inserted-row commit writes RowEdit.cells by column label and patches the
    /// painted cell immediately, including its incomplete marker. A subsequent bump
    /// composes with that value.
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

    /// Deleted rows refuse editing, choice steps, and row bumps without opening an
    /// editor. Column bumps skip deleted rows.
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

    /// Text commits trimmed text; Date opens a segmented field in the cell. Both write
    /// typed values and use the column's renderer. The text fixture uses a note column
    /// because SCHEDULE's status opens a Choice popup.
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

    /// The commit action completes a date cell's pending segment digits before
    /// committing, just like the field's Enter handler. Cancel blurs and drops the
    /// field.
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

    /// Required free-text fixture for text-editor tests. SCHEDULE's status is a Choice
    /// popup; this panel instead matches SCHEDULE_OPTIONAL_NOTE with a required value.
    static SCHEDULE_REQUIRED_NOTE: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
        Arc::new(PanelSpec {
            kind: "sched_note_req".into(),
            title: "Dividends (note)".into(),
            dataset: "div_schedule_note".into(),
            document: "div_schedule_note".into(),
            rows: RowAxis {
                column: "dividend_id".into(),
                identity: RowIdentity::Minted,
                label: RowLabel::Shown,
            },
            columns: Columns::Values(vec![ValueColumn {
                column: "note".into(),
                label: "note".into(),
                ty: ColumnType::Utf8,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: true,
            }]),
            header: Vec::new(),
            slice_values: Vec::new(),
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            actions: Vec::new(),
        })
    });

    /// A one-column flat panel whose value is optional free text — what
    /// tells "empty is refused" (a required column) from "empty is a
    /// value" (an optional one).
    static SCHEDULE_OPTIONAL_NOTE: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
        Arc::new(PanelSpec {
            kind: "sched_note".into(),
            title: "Dividends (note)".into(),
            dataset: "div_schedule_note".into(),
            document: "div_schedule_note".into(),
            rows: RowAxis {
                column: "dividend_id".into(),
                identity: RowIdentity::Minted,
                label: RowLabel::Shown,
            },
            columns: Columns::Values(vec![ValueColumn {
                column: "note".into(),
                label: "note".into(),
                ty: ColumnType::Utf8,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: false,
            }]),
            header: Vec::new(),
            slice_values: Vec::new(),
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            actions: Vec::new(),
        })
    });

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

    /// The session tick serializes the draft, capturing its groups from the
    /// installed index: no index build.
    #[gpui::test]
    fn serialize_captures_groups_without_building(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::DIVIDEND, None);
        h.command(&mut vcx, "key SPX.Z").unwrap();
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
        h.command(&mut vcx, "bump 0.5").unwrap();
        let before = crate::core::matrix::builds();
        let t = vcx.update(|_, cx| h.content.serialize(cx));
        assert_eq!(
            crate::core::matrix::builds(),
            before,
            "the tick built nothing"
        );
        let groups = &t["drafts"]["SPX.Z"]["groups"];
        assert_eq!(groups["2026-09-18"].as_integer(), Some(2), "{t:?}");
    }

    /// A committed cell refills its window cell; the index is not rebuilt.
    #[gpui::test]
    fn a_cell_commit_patches_the_model_in_place(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let index = h
            .tile
            .read_with(&vcx, |t, _| t.model() as *const MatrixIndex);
        let before = crate::core::matrix::builds();
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            crate::core::matrix::builds(),
            before,
            "no rebuild for one cell"
        );
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.model() as *const MatrixIndex),
            index
        );
        let painted = h.painted_cell(&vcx, 0, 0).expect("on screen");
        assert_eq!((painted.text.as_ref(), painted.edited), ("4505.50", true));
        assert_eq!(h.painted(&vcx, 0, 1), Some("0.1800".to_string()));
    }

    /// Leaving Sent through a one-cell commit repaints every window cell unsent.
    #[gpui::test]
    fn a_cell_commit_on_a_sent_draft_repaints_every_cell_unsent(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.upload_ok(&mut vcx);
        assert!(
            h.painted_cell(&vcx, 0, 0).is_some_and(|c| c.sent),
            "sent after the upload"
        );
        h.motion(&mut vcx, "down", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "4600");
        h.dispatch(&mut vcx, "commit", None);
        let cols = h.tile.read_with(&vcx, |t, _| t.model().columns.len());
        for row in 0..h.rows(&vcx) {
            for col in 0..cols {
                assert!(
                    !h.painted_cell(&vcx, row, col).is_some_and(|c| c.sent),
                    "({row}, {col})"
                );
            }
        }
    }

    /// An accepted upload marks every edited window cell sent.
    #[gpui::test]
    fn an_accepted_upload_marks_the_painted_cells_sent(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        assert!(!h.painted_cell(&vcx, 0, 0).unwrap().sent);
        h.upload_ok(&mut vcx);
        assert!(h.painted_cell(&vcx, 0, 0).unwrap().sent);
    }

    /// `/` prepares its text once per index build: keystrokes and a cancel
    /// reuse it; an insert (a rebuild) prepares it again.
    #[gpui::test]
    fn find_builds_its_text_once_per_index_build(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        for q in ["2", "20", "202"] {
            vcx.update(|window, cx| h.content.find(FindEvent::Changed(q.into()), window, cx));
        }
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("11".into()), window, cx));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.search_builds), 1);
        h.dispatch(&mut vcx, "insert_below", None);
        h.dispatch(&mut vcx, "cancel", None);
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("11".into()), window, cx));
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.search_builds), 2);
    }

    /// Under a hidden label `/` searches cell text; a value typed into a
    /// cell is found without a rebuild.
    #[gpui::test]
    fn find_after_a_cell_edit_finds_the_new_value_under_a_hidden_label(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::HIDDEN_SCHEDULE, None);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, "declared"),
            ]),
        );
        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("1.25".into()), window, cx)
        });
        vcx.update(|window, cx| h.content.find(FindEvent::Cancelled, window, cx));
        h.motion(&mut vcx, "down", None);
        let amount = h
            .headers(&vcx)
            .iter()
            .position(|n| n == "amount")
            .expect("amount") as u32;
        h.motion(&mut vcx, "right", Some(amount));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "9.75");
        h.dispatch(&mut vcx, "commit", None);
        h.motion(&mut vcx, "up", None);
        let before = crate::core::matrix::builds();
        vcx.update(|window, cx| {
            h.content
                .find(FindEvent::Changed("9.75".into()), window, cx)
        });
        assert_eq!(crate::core::matrix::builds(), before, "no rebuild");
        assert!(matches!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 1, .. }
        ));
    }

    /// `y c` copies every row of the column, on screen or not.
    #[gpui::test]
    fn yank_col_includes_rows_off_screen(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        let rows: Vec<(String, String)> = (0..200)
            .map(|i| {
                (
                    format!("D{i}"),
                    format!("{:04}-01-{:02}", 2027 + i / 28, i % 28 + 1),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str, f64, &str)> = rows
            .iter()
            .enumerate()
            .map(|(i, (l, d))| (l.as_str(), d.as_str(), i as f64, "declared"))
            .collect();
        h.with_flat_document_with(&mut vcx, test_fixtures::schedule_snapshot(&borrowed));
        let amount = h
            .headers(&vcx)
            .iter()
            .position(|n| n == "amount")
            .expect("amount") as u32;
        // The painted label column leads the headers; the cursor starts on
        // model column 0.
        let amount = amount - 1;
        h.motion(&mut vcx, "right", Some(amount));
        h.dispatch(&mut vcx, "yank_col", None);
        let text = clipboard(&mut vcx).expect("yanked");
        assert_eq!(text.lines().count(), 200);
        assert_eq!(text.lines().last(), Some("199.0000"));
    }

    /// A grid date editor ends at the cell's right edge and occupies the width
    /// of its plain date text, without the header field's padding or border.
    /// This verifies relative alignment; actual font metrics still determine
    /// whether the date fits the fixed cell width.
    #[gpui::test]
    fn a_date_cells_field_is_right_aligned_inside_its_cell(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-editor-0-1").is_some(),
            "the field paints in the cursor cell (row 0, table column 1)"
        );
        // The cell's own bounds, not the editor slot's: a slot grows to
        // fit what it holds, so an oversized field widens the slot with it.
        let cell = vcx
            .debug_bounds("marketdata-cell-0-1")
            .expect("the cell is painted");
        let segment = |vcx: &mut gpui::VisualTestContext, i: usize| {
            vcx.debug_bounds(Box::leak(
                format!("marketdata-date-seg-{TILE}-{i}").into_boxed_str(),
            ))
            .expect("every segment is painted")
        };
        let (year, day) = (segment(&mut vcx, 0), segment(&mut vcx, 2));
        assert!(
            (cell.right() - day.right()).abs() <= gpui::px(1.),
            "the day segment ends at the cell's right edge ({:?}), not at {:?}",
            cell.right(),
            day.right()
        );
        // The data face is monospaced, so the date as plain text is ten
        // cells of the year's quarter-width; a padded segment widens it.
        let glyph = year.size.width / 4.;
        assert!(
            ((day.right() - year.left()) - glyph * 10.).abs() <= gpui::px(1.),
            "the field is as wide as the date as plain text ({:?}), not {:?}",
            glyph * 10.,
            day.right() - year.left()
        );
    }

    /// `[ui] line_numbers` reaches a live panel through the shell's
    /// `UiSettings` global: off paints no gutter; publishing `rel` paints
    /// one per row on the next draw, numbered from the cursor with the
    /// cursor row showing its absolute number, and widens the pinned
    /// column by exactly the gutter — BESIDE the row-label cell, which
    /// keeps its own width, so the cursor border and a row's fill never
    /// reach the number. A cursor move re-derives the offsets; with the
    /// cursor in the header strip there is no row to measure from, so
    /// `rel` numbers absolutely. Off gives the width back.
    #[gpui::test]
    fn the_line_numbers_global_paints_a_gutter_beside_the_row_label(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-gutter-0").is_none(),
            "no gutter while the setting is off (the default with no global set)"
        );
        let bounds = |vcx: &mut gpui::VisualTestContext, sel: &'static str| {
            vcx.debug_bounds(sel).expect("painted")
        };
        let label = bounds(&mut vcx, "marketdata-cell-0-0");
        let value = bounds(&mut vcx, "marketdata-cell-0-1");

        vcx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::Relative,
            })
        });
        draw(&mut vcx);
        let gutter = bounds(&mut vcx, "marketdata-gutter-0");
        let width = h
            .tile
            .read_with(&vcx, |t, cx| t.table().read(cx).delegate().gutter_px());
        assert!(width > 0.0, "sanity: a live gutter has width");
        let label_on = bounds(&mut vcx, "marketdata-cell-0-0");
        let value_on = bounds(&mut vcx, "marketdata-cell-0-1");
        assert!(
            (f32::from(value_on.left() - value.left()) - width).abs() < 0.5,
            "the pinned column widened by the gutter ({width}); `on_ui_settings` \
             must `refresh` the table, which caches `column()`'s width"
        );
        assert!(
            (label_on.size.width - label.size.width).abs() < gpui::px(0.5),
            "the label cell keeps its own width"
        );
        assert!(
            gutter.right() <= label_on.left(),
            "the gutter sits beside the label cell, not inside it: \
             {gutter:?} then {label_on:?}"
        );

        let texts = |vcx: &mut gpui::VisualTestContext| -> Vec<String> {
            h.tile.update(vcx, |t, cx| {
                t.table().update(cx, |t, _| {
                    let d = t.delegate_mut();
                    (0..2)
                        .map(|r| d.gutter_text(r).map(|s| s.to_string()).unwrap_or_default())
                        .collect()
                })
            })
        };
        assert_eq!(
            texts(&mut vcx),
            vec!["1", "1"],
            "cursor on row 0: its absolute number, then distances"
        );
        h.motion(&mut vcx, "down", None);
        assert_eq!(texts(&mut vcx), vec!["1", "2"], "cursor on row 1");
        h.tile.update(&mut vcx, |t, cx| {
            t.table().update(cx, |t, _| t.delegate_mut().cursor = None)
        });
        assert_eq!(
            texts(&mut vcx),
            vec!["1", "2"],
            "no grid cursor: `rel` numbers absolutely"
        );

        vcx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::Off,
            })
        });
        draw(&mut vcx);
        assert!(
            vcx.debug_bounds("marketdata-gutter-0").is_none(),
            "off again on the next draw"
        );
        assert!(
            (bounds(&mut vcx, "marketdata-cell-0-1").left() - value.left()).abs() < gpui::px(0.5),
            "and the pinned column gave the width back"
        );
    }

    /// Under a hidden row label the first VALUE column is the pinned one,
    /// so the gutter rides there — beside the value cell, which keeps its
    /// width and its right-aligned value.
    #[gpui::test]
    fn a_hidden_label_panel_paints_its_gutter_beside_the_first_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open_spec(cx, &test_fixtures::HIDDEN_SCHEDULE, None);
        h.with_flat_document(&mut vcx);
        draw(&mut vcx);
        let first = vcx.debug_bounds("marketdata-cell-0-0").expect("painted");
        vcx.update(|_, cx| {
            cx.set_global(UiSettings {
                line_numbers: LineNumbers::On,
            })
        });
        draw(&mut vcx);
        let gutter = vcx
            .debug_bounds("marketdata-gutter-0")
            .expect("the gutter paints in the first value column");
        let first_on = vcx.debug_bounds("marketdata-cell-0-0").expect("painted");
        assert!(
            gutter.right() <= first_on.left(),
            "{gutter:?} then {first_on:?}"
        );
        assert!(
            (first_on.size.width - first.size.width).abs() < gpui::px(0.5),
            "the value cell keeps its own width"
        );
        assert!(
            vcx.debug_bounds("marketdata-gutter-1").is_some(),
            "one gutter per row"
        );
    }

    /// The delegate mirrors a date CELL's field exactly as it mirrors the
    /// text editor: the cell, and the field's own paint and focus handle,
    /// so `render_td` can paint the segments in the cell.
    #[gpui::test]
    fn the_delegate_mirrors_a_date_cells_field(cx: &mut gpui::TestAppContext) {
        use crate::delegate::DelegateEditorPaint;
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.motion(&mut vcx, "down", None);
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

    /// Nudging a Text editor refuses and preserves typed text. Choice columns use a
    /// popup whose insert_up moves the highlight, so the test uses the required-note
    /// fixture.
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

    /// The flat amount column parses as F64 and renders the committed value through its
    /// own four-place format.
    #[gpui::test]
    fn a_flat_panels_commit_on_amount_parses_as_f64(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.motion(&mut vcx, "right", Some(1));

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
        h.motion(&mut vcx, "right", Some(1));

        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("1.2500"));
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("1.2501"));
        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_empty()),
            "a nudge commits nothing"
        );
    }

    // ---- Choice cells ------------------------------------------------

    /// Choice stepping follows declared option order, wraps at both ends, and starts
    /// from the draft's current value. Other cell kinds refuse without writing.
    #[gpui::test]
    fn space_steps_a_choice_cell_and_refuses_elsewhere(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.motion(&mut vcx, "right", Some(2)); // status = declared
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

        h.motion(&mut vcx, "left", None); // amount
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
        h.motion(&mut vcx, "right", Some(2));
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
        h.motion(&mut vcx, "up", None); // Attr(0)
        h.dispatch(&mut vcx, "step", None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("step needs a grid cell — the cursor is in the header".to_string())
        );
        h.motion(&mut vcx, "down", None);
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
        h.motion(&mut vcx, "right", Some(2));
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
        h.motion(&mut vcx, "right", Some(2));
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
            "a bare step wraps"
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
        h.motion(&mut vcx, "right", Some(2));
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

    /// Double-click and i open the same Choice popup. Both obey the editor refusal
    /// gate, so Behind opens nothing.
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
        h.motion(&mut vcx, "right", Some(2));
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
    static SCHEDULE_I64: LazyLock<Arc<PanelSpec>> = LazyLock::new(|| {
        Arc::new(PanelSpec {
            kind: "sched_i64".into(),
            title: "Dividends (i64)".into(),
            dataset: "div_schedule_i64".into(),
            document: "div_schedule_i64".into(),
            rows: RowAxis {
                column: "dividend_id".into(),
                identity: RowIdentity::Minted,
                label: RowLabel::Shown,
            },
            columns: Columns::Values(vec![ValueColumn {
                column: "units".into(),
                label: "units".into(),
                ty: ColumnType::I64,
                format: ColumnFormat::MEASURE,
                choices: None,
                required: true,
            }]),
            header: Vec::new(),
            slice_values: Vec::new(),
            value_type: ColumnType::F64,
            format: ColumnFormat::MEASURE,
            actions: Vec::new(),
        })
    });

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

    /// Reverting a Behind draft clears edits and paints the newest document rather than
    /// the retained base.
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
            (t.draft().state.clone(), t.model().len(), t.cell_at(0, 0))
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

    /// Without a document, edit and bump refuse: there is no cell identity or source
    /// generation against which to record an edit.
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
        h.motion(&mut vcx, "bottom", None);
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

    /// A delivery with the same request tag but a newer source time moves an edited
    /// draft Behind. The retained base stays painted under the edit.
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
            (t.draft().state.clone(), t.model().len(), t.cell_at(0, 0))
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
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

    /// A same-time republish with a changed generation puts an edited draft
    /// Behind and retains its base grid. Removing a term in the new document
    /// must not move a position-keyed edit to the term now at that index.
    #[gpui::test]
    fn a_republish_at_the_same_source_time_holds_the_draft_instead_of_repointing_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &NODES, provenance_gen(BASE, 7))),
        );

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        // The SAME source time, a new generation, and a document whose ROWS
        // have moved: the first term is gone, so the edited cell's row index
        // now belongs to the term below it.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(
                &["2026-11-20"],
                &NODES,
                provenance_gen(BASE, 8),
            )),
        );

        let (state, rows, cell, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().len(),
                t.cell_at(0, 0),
                t.notice().map(str::to_string),
            )
        });
        assert!(
            matches!(
                state,
                DraftState::Behind { ref newer }
                    if newer.as_of == BASE && newer.generation == Some(8)
            ),
            "a same-time republish must be Behind, got {state:?}"
        );
        assert_eq!(rows, 2, "still painting the base generation's two terms");
        assert_eq!(cell.text.to_string(), "0.50", "the edit is still its own");
        assert!(cell.edited);
        assert!(
            notice.is_some_and(|n| n.contains("republished")),
            "the badge's time is the base's own, so the notice must say what moved"
        );
    }

    /// A same-time republish that inserts a node before the existing ladder
    /// must not shift a held edit onto the inserted node. Axis columns follow
    /// document order, while the edit retains its original grid position.
    #[gpui::test]
    fn a_republish_that_reorders_the_nodes_keeps_the_edit_on_its_own_node(
        cx: &mut gpui::TestAppContext,
    ) {
        // The base ladder with a node inserted AHEAD of it, so every node a
        // held edit could be keyed by sits one column further right.
        const WIDER: [f64; 4] = [-30.0, -20.0, -1.0, 3.5];
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &NODES, provenance_gen(BASE, 7))),
        );

        // Onto the first NODE column, past the slice values: a cell whose
        // column identity is a ladder position, which is what moves.
        h.motion(&mut vcx, "right", Some(SLICE as u32));
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.9");
        h.dispatch(&mut vcx, "commit", None);
        let node = h
            .tile
            .read_with(&vcx, |t, _| t.model().columns[SLICE].clone());

        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &WIDER, provenance_gen(BASE, 8))),
        );

        let (state, columns, cells) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.model().columns.clone(),
                (0..t.model().columns.len())
                    .map(|c| t.cell_at(0, c))
                    .collect::<Vec<_>>(),
            )
        });
        assert!(
            matches!(
                state,
                DraftState::Behind { ref newer }
                    if newer.as_of == BASE && newer.generation == Some(8)
            ),
            "a same-time republish must be Behind, got {state:?}"
        );
        assert_eq!(
            columns.len(),
            SLICE + NODES.len(),
            "still the base ladder, not the republished one: {columns:?}"
        );
        let edited: Vec<SharedString> = columns
            .iter()
            .zip(&cells)
            .filter(|(_, cell)| cell.edited)
            .map(|(label, _)| label.clone())
            .collect();
        assert_eq!(
            edited,
            vec![node],
            "the painted edit is still on the node it was typed into: {columns:?}"
        );
        assert_eq!(cells[SLICE].text.to_string(), "0.9000", "its own value");
    }

    /// Automatic rebase onto a same-time republish reports that the edits moved.
    /// The source-time chip is unchanged and no Behind badge remains, so the
    /// notice supplies the update feedback without offering a pending action.
    #[gpui::test]
    fn auto_rebase_still_discloses_a_same_time_republish(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.command(&mut vcx, "auto rebase").unwrap();
        h.command(&mut vcx, "key SPX.Z").unwrap();
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &NODES, provenance_gen(BASE, 7))),
        );

        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        // The same terms and nodes, so every label resolves and nothing is
        // dropped: no `dropped_notice` stands in for the disclosure.
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_with(&TERMS, &NODES, provenance_gen(BASE, 8))),
        );

        let (state, base, notice, chips) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().base.clone(),
                t.notice().map(str::to_string),
                t.header_texts(),
            )
        });
        assert_eq!(state, DraftState::Editing, "rebased, never left Behind");
        assert_eq!(
            base.and_then(|b| b.generation),
            Some(8),
            "and it stands on the republished generation"
        );
        assert!(
            !chips.iter().any(|c| c.starts_with("update ")),
            "no badge moves on a same-time republish: {chips:?}"
        );
        let notice = notice.expect("the republish must be disclosed somewhere");
        assert!(notice.contains("republished"), "{notice}");
        assert!(
            !notice.contains(":rebase") && !notice.contains(":revert"),
            "nothing is pending, so offer no keys: {notice}"
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
            (t.draft().state.clone(), t.model().len(), t.cell_at(0, 0))
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWEST),
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
        h.motion(&mut vcx, "right", None);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.6");
        h.dispatch(&mut vcx, "commit", None);
        h.motion(&mut vcx, "left", None);
        h.motion(&mut vcx, "down", None);
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
                t.model().len(),
                t.cell_at(0, 0),
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

    /// Rebase drops a same-date ordinal edit when that date's group size changes. A
    /// DIVIDEND group growing from two rows to three invalidates 2026-09-18#2, and the
    /// notice reports the size change.
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
        h.motion(&mut vcx, "down", None);
        h.motion(&mut vcx, "right", Some(3));
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

    /// A restored draft retains same-day group sizes from its true base even when that
    /// generation is never delivered. The newer painted fallback must not overwrite
    /// those sizes, so rebase can still detect a shifted ordinal edit.
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
        // The first delivered generation is newer than BASE, so no true base snapshot
        // can be retained. Its same-date group has grown from the session's captured
        // two rows to three.
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

    /// Both live and restored Behind drafts refuse editing and bumps until the newer
    /// document is resolved.
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

    /// An as-of answer survives replacement of its barrier by scope/grouping changes,
    /// which this tile does not follow. It remains the only answer for the current
    /// request and must promote under the newer barrier.
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
            f.shared_mut().set_text(Some("SPX".into()));
            f.shared_mut()
                .open_flip([QueryKey(TILE), other], Instant::now());
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
    /// promote. Reachable while HIDDEN — a hidden panel keeps a stage
    /// already taken, and does not requery for the as-of change that
    /// follows.
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

    /// An empty first delivery preserves restored edits. Resolving them against an
    /// empty label map would silently drop unsent work, so restoration remains pending
    /// until a usable document arrives.
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
        let cell = h.tile.read_with(&vcx, |t, _| t.cell_at(1, SLICE + 1));
        assert_eq!(cell.text.to_string(), "9.5000");
        assert!(cell.edited, "the restored edit is placed by label at last");
    }

    /// A malformed first delivery with a repeated pivot pair leaves the model empty and
    /// the restored draft unresolved. Build failure must not trigger rebase against an
    /// empty model.
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
        let cell = h.tile.read_with(&vcx, |t, _| t.cell_at(1, SLICE + 1));
        assert_eq!(cell.text.to_string(), "9.5000");
        assert!(cell.edited);
    }

    /// Restoration reports edits dropped because the delivered document lacks their row
    /// or column, just as explicit rebase reports missing targets.
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

    /// If the true base of a restored Behind draft was never delivered, later
    /// deliveries must not retain a newer painted fallback as that base. The fallback
    /// continues to show the newest usable generation.
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

    /// An unbuildable generation changes only the notice. Keep the last usable model,
    /// snapshot, and draft together so rebase never targets a document that could not
    /// be painted.
    #[gpui::test]
    fn a_delivery_that_cannot_be_built_changes_nothing_but_the_notice(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        h.set_editor(&mut vcx, "0.5");
        h.dispatch(&mut vcx, "commit", None);

        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(document_of(&["t0", "t0"], &NODES, NEWER)),
        );

        let (state, rows, chips) = h.tile.read_with(&vcx, |t, _| {
            (t.draft().state.clone(), t.model().len(), t.header_texts())
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

    /// A delivery clears the prior query failure only when its replacement document
    /// paints. Staging a successful answer leaves the failure notice visible until
    /// promotion.
    #[gpui::test]
    fn a_painting_delivery_clears_the_previous_deliverys_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
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

    /// A delivery preserves notices generated by its own apply, including restored-edit
    /// drops. Clear the prior notice when applying the painted delivery, before
    /// producing any new notice.
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

    // ---- Attribute cursor, editing, and set ---------------------------

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
        h.motion(&mut vcx, "right", None);
        h.motion(&mut vcx, "up", None);
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
        h.motion(&mut vcx, "down", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            Cursor::Cell { row: 0, col: 1 }
        );
    }

    /// A refused F64 attribute parse keeps insert mode and typed text intact and writes
    /// no draft value.
    #[gpui::test]
    fn a_bad_number_stays_in_insert_mode_with_the_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "right", None);
        h.motion(&mut vcx, "up", None); // Attr(1) = spot_ref
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

    // ---- Nudging -----------------------------------------------------

    /// `up` in an open cell editor steps the text by one unit of the
    /// column's painted precision — a `param` at four places by `0.0001`
    /// — `shift+up` (`insert_up_big`) by ten units, and the two compose
    /// in the editor's text; `enter` then commits the nudged value, so
    /// the arrows write nothing of their own.
    #[gpui::test]
    fn up_steps_a_cell_one_unit_of_its_precision_and_shift_ten(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "right", Some(3)); // the first node: 0.1000
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
        h.motion(&mut vcx, "up", None); // Attr(0) = anchor_date
        h.dispatch(&mut vcx, "edit", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-12"));
        h.dispatch(&mut vcx, "insert_down_big", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-02"));
        h.dispatch(&mut vcx, "insert_up", None);
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("2026-09-03"));
        h.dispatch(&mut vcx, "cancel", None);

        h.motion(&mut vcx, "right", None); // Attr(1) = spot_ref
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
        h.motion(&mut vcx, "right", Some(3));
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

    /// Set without a value returns the current attribute value as an Err notice and
    /// performs no write.
    #[gpui::test]
    fn set_with_no_value_answers_the_current_value_as_a_notice(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "set spot_ref"),
            Err("spot_ref = 5000".to_string())
        );
    }

    /// A single attribute click selects Attr(i) without opening an editor, matching
    /// grid-cell selection.
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

    /// Double-clicking an attribute opens its seeded editor and focuses it after
    /// drawing. Both presses propagate to the host's tile-level mouse-down handler for
    /// focus restoration.
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

    /// A newer generation moves an attribute-edited draft Behind. Rebase carries the
    /// attribute edit by column name and marks it edited on the new document.
    #[gpui::test]
    fn an_attribute_edit_goes_behind_and_rebase_keeps_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
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

    /// Opening an attribute's editor leaves its value box where it was:
    /// the box is the only frame, so neither the text input's own
    /// border, padding and control height nor a second date frame paints
    /// inside it. The date editor is also as wide as the resting date.
    #[gpui::test]
    fn opening_an_attribute_editor_keeps_its_value_box(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        for (i, keeps_width) in [(0, true), (1, false)] {
            let selector: &'static str =
                Box::leak(format!("marketdata-attr-{TILE}-{i}").into_boxed_str());
            draw(&mut vcx);
            let rest = vcx
                .debug_bounds(selector)
                .expect("the attribute is painted");
            let at = centre_of(&mut vcx, selector);
            click_at(&mut vcx, at, 1);
            click_at(&mut vcx, at, 2);
            assert_eq!(h.mode(&vcx), "insert", "attribute {i} opened its editor");
            draw(&mut vcx);
            let editing = vcx
                .debug_bounds(selector)
                .expect("the attribute is painted");
            assert_eq!(
                (editing.top(), editing.size.height),
                (rest.top(), rest.size.height),
                "attribute {i}'s box keeps its top and height while editing"
            );
            if keeps_width {
                assert!(
                    (editing.size.width - rest.size.width).abs() <= gpui::px(1.),
                    "attribute {i}'s box keeps its width ({:?}), not {:?}",
                    rest.size.width,
                    editing.size.width
                );
            }
            h.dispatch(&mut vcx, "escape", None);
        }
    }

    /// `y`/`yy` in the strip yank the attribute's own value, and its
    /// label plus value tab-separated — the same shape a grid row's `yy`
    /// yanks. `yc` has no column to yank and answers with a notice
    /// instead of silently doing nothing.
    #[gpui::test]
    fn yank_in_the_strip_reads_the_attribute_and_yc_is_inert(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "up", None);
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

    // ---- Actions -----------------------------------------------------

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
        // Upload is greyed on a clean draft, so `j` steps over it; the
        // pointer is the one way the highlight rests there.
        let row = centre_of(&mut vcx, &format!("marketdata-menu-row-{TILE}-1"));
        move_to(&mut vcx, row);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.menu_highlighted()), Some(1));
        h.dispatch(&mut vcx, "menu_pick", None);
        assert_eq!(h.mode(&vcx), "menu");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("nothing to upload".into())
        );
    }

    #[gpui::test]
    fn an_unrelated_action_closes_the_menu_first(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.motion(&mut vcx, "down", None);
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

    /// Install Chords from ACTIONS, the fragment, and `user` as the user
    /// layer, so chord lookups use the keymap the shell would publish.
    fn install_chords(vcx: &mut gpui::VisualTestContext, user: Option<&str>) {
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
        // Kind actions register beside ACTIONS, as the content's registration
        // does, so a user layer may bind them.
        for a in &CVI.actions {
            registry
                .register(geode_shell::actions::ActionDef {
                    id: ActionId(a.id.to_string()),
                    title: a.title.to_string(),
                    category: "Market data".to_string(),
                })
                .expect("no duplicate ids");
        }
        let mut docs = vec![
            geode_shell::keymap::fragments::fragment_doc(&CVI.kind, crate::content::DEFAULT_KEYMAP)
                .expect("the fragment parses"),
        ];
        if let Some(text) = user {
            docs.push(geode_core::config::LayerDoc {
                layer: geode_core::config::Layer::User,
                name: "keymap".into(),
                file: "user/keymap.toml".into(),
                table: text.parse().unwrap(),
            });
        }
        let (keymap, diags) = geode_shell::keymap::build_keymap(
            &docs,
            geode_shell::defaults::default_mod(),
            &registry,
        );
        assert!(diags.is_empty(), "{diags:?}");
        vcx.update(|_window, cx| {
            cx.set_global(geode_shell::tips::Chords(Arc::new(
                keymap.bindings().to_vec(),
            )));
        });
        vcx.run_until_parked();
    }

    fn install_fragment_chords(vcx: &mut gpui::VisualTestContext) {
        install_chords(vcx, None);
    }

    const LOAD_REBOUND: &str = "[[bindings]]\ncontext = \"marketdata && mode == normal\"\n[bindings.keys]\n\"u\" = \"none\"\n\"shift+u\" = \"marketdata::load_underlying\"\n";

    const LOAD_UNBOUND: &str = "[[bindings]]\ncontext = \"marketdata && mode == normal\"\n[bindings.keys]\n\"u\" = \"none\"\n";

    /// The open menu's trailing lane for the row titled `title`: its keys
    /// in keymap spelling, or its text.
    fn menu_lane(h: &Harness, vcx: &gpui::VisualTestContext, title: &str) -> String {
        use geode_tile::menu::{Row, Trailing};
        h.tile.read_with(vcx, |t, _| match &t.popup {
            Some(Popup::Menu(m)) => m
                .rows()
                .iter()
                .find_map(|r| match r {
                    Row::Action(a) if a.title().as_ref() == title => Some(match a.trailing() {
                        Trailing::Keys(k) => geode_shell::palette::render_binding(k),
                        Trailing::Text(t) => t.to_string(),
                        Trailing::None => String::new(),
                    }),
                    _ => None,
                })
                .expect("the row"),
            _ => panic!("the menu is open"),
        })
    }

    /// The menu's hints are the live keymap's: a user rebind shows, and a
    /// disabled row still shows its reason.
    #[gpui::test]
    fn a_menu_hint_follows_a_user_rebind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_chords(&mut vcx, Some(LOAD_REBOUND));
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "shift+u");
        assert_eq!(
            menu_lane(&h, &vcx, "Revert edits"),
            "nothing to revert",
            "a disabled row still shows its reason"
        );
    }

    /// An action the user unbinds entirely never shows its shipped key: a
    /// key-only row's lane is empty; a verb row (`Upload`, bound nowhere by
    /// default) shows its `:` verb.
    #[gpui::test]
    fn an_unbound_action_shows_no_stale_key(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        install_chords(&mut vcx, Some(LOAD_UNBOUND));
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "");
        assert_eq!(menu_lane(&h, &vcx, "Upload"), ":upload");
        assert_eq!(menu_lane(&h, &vcx, "Revert edits"), ":revert");
    }

    const POLICY_AND_KIND_BOUND: &str = "[[bindings]]\ncontext = \"marketdata && mode == normal\"\n[bindings.keys]\n\"z\" = \"marketdata::auto_rebase\"\n\"shift+z\" = \"marketdata::cvi_reanchor\"\n";

    /// A user binding on an update-policy action and on a kind action reaches
    /// the open menu: the policy row trails the chord instead of its `:auto`
    /// verb, and the kind row's lane resolves to the chord (painted once the
    /// action is built; an unbuilt row trails its reason over it). The other
    /// policy rows keep their verbs.
    #[gpui::test]
    fn a_menu_hint_follows_a_policy_and_kind_rebind(cx: &mut gpui::TestAppContext) {
        use geode_tile::menu::{Lane, Row};
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_chords(&mut vcx, Some(POLICY_AND_KIND_BOUND));
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "rebase edits"), "z");
        assert_eq!(menu_lane(&h, &vcx, "hold edits"), ":auto hold");
        assert_eq!(menu_lane(&h, &vcx, "replace edits"), ":auto replace");
        let reanchor = h.tile.read_with(&vcx, |t, _| match &t.popup {
            Some(Popup::Menu(m)) => m
                .rows()
                .iter()
                .find_map(|r| match r {
                    Row::Action(a) if a.title().as_ref() == "Reanchor" => Some(a.lane().clone()),
                    _ => None,
                })
                .expect("the row"),
            _ => panic!("the menu is open"),
        });
        match reanchor {
            Lane::Keys(k) => assert_eq!(geode_shell::palette::render_binding(&k), "shift+z"),
            other => panic!("the kind row resolves its binding, got {other:?}"),
        }
    }

    /// A keymap republished while the menu is open re-resolves its hints
    /// at once, not at the next open.
    #[gpui::test]
    fn an_open_menu_follows_a_keymap_reload(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        install_chords(&mut vcx, None);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "u");
        install_chords(&mut vcx, Some(LOAD_REBOUND));
        assert_eq!(menu_lane(&h, &vcx, "Load underlying…"), "shift+u");
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

    /// The actions button toggles its popup in capture without stopping propagation.
    /// Both opening and closing clicks reach the host's bubble listener, standing in
    /// for shell click-to-focus, drag arming, and focus restoration.
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

    fn note(h: &Harness, vcx: &mut gpui::VisualTestContext, source: &str, health: Health) {
        h.diagnostics.update(vcx, |d, cx| {
            d.note_health(source, health, "why".into(), SystemTime::UNIX_EPOCH);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    fn chip_word(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<String> {
        h.tile
            .read_with(vcx, |t, _| t.health_chip().map(|c| c.word().to_string()))
    }

    fn chip_title(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<String> {
        h.tile
            .read_with(vcx, |t, _| t.health_chip().map(|c| c.title().to_string()))
    }

    /// A real health report on the shared entity reaches this panel's header
    /// through its own observer; another dataset's source never does.
    #[gpui::test]
    fn a_degraded_panel_source_shows_the_chip_and_recovery_clears_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.describe_source("cvi_src", SourceSummary::for_dataset("cvi_params"));
            d.describe_source("div_src", SourceSummary::for_dataset("dividend_schedule"));
            cx.notify();
        });
        note(
            &h,
            &mut vcx,
            "div_src",
            Health::Failed {
                reason: "torn".into(),
            },
        );
        assert_eq!(chip_word(&h, &vcx), None, "another panel's dataset");
        note(
            &h,
            &mut vcx,
            "cvi_src",
            Health::Degraded {
                reason: "late".into(),
            },
        );
        assert_eq!(chip_word(&h, &vcx), Some("degraded".into()));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.health_chip().unwrap().tone()),
            chip::Tone::Warning
        );
        assert_eq!(chip_title(&h, &vcx), Some("cvi_src: late".into()));
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // The harness tile is `TILE` (3).
        assert!(
            vcx.debug_bounds("tile-health-3").is_some(),
            "the chip paints"
        );
        note(&h, &mut vcx, "cvi_src", Health::Ok);
        assert_eq!(chip_word(&h, &vcx), None);
    }

    /// A report that lands before the source is described has no dataset
    /// link yet; the description bumps the sources version and the chip
    /// appears then.
    #[gpui::test]
    fn a_source_described_after_its_failure_reaches_the_chip(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        note(
            &h,
            &mut vcx,
            "cvi_src",
            Health::Failed {
                reason: "torn".into(),
            },
        );
        assert_eq!(chip_word(&h, &vcx), None);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.describe_source("cvi_src", SourceSummary::for_dataset("cvi_params"));
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(chip_word(&h, &vcx), Some("failed".into()));
        assert_eq!(chip_title(&h, &vcx), Some("cvi_src: torn".into()));
    }

    /// A panel created while its source is already failed asks at once:
    /// the chip is there before any further diagnostics notification.
    #[gpui::test]
    fn a_panel_opened_after_its_source_failed_shows_the_chip_at_once(
        cx: &mut gpui::TestAppContext,
    ) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.describe_source("cvi_src", SourceSummary::for_dataset("cvi_params"));
            cx.notify();
        });
        note(
            &h,
            &mut vcx,
            "cvi_src",
            Health::Failed {
                reason: "torn".into(),
            },
        );
        let (data, _rx) = DataHandle::for_tests();
        let factory = MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(15 * 60));
        let (frame, diagnostics) = (h.frame.clone(), h.diagnostics.clone());
        let second = vcx.update(|window, cx| {
            factory.create(
                TileId(TILE + 1),
                None,
                FrameRef::new(frame, WorkspaceIx::FIRST),
                diagnostics,
                window,
                cx,
            )
        });
        let tile = second.view.downcast::<MarketDataTile>().unwrap();
        assert_eq!(
            tile.read_with(&vcx, |t, _| t
                .health_chip()
                .map(|c| (c.word().to_string(), c.title().to_string()))),
            Some(("failed".to_string(), "cvi_src: torn".to_string()))
        );
    }

    /// The popup occludes the grid beneath it. Hovering an action row moves its
    /// highlight without triggering the host's underlying hover listener.
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

    // ---- Segmented date field ----------------------------------------

    /// Opens the field on `anchor_date` (Attr 0) and paints it: the
    /// field's `on_key_down` is a listener on the painted, focused element,
    /// so every test that types into it draws first.
    fn open_date_field(h: &Harness, vcx: &mut gpui::VisualTestContext) {
        h.motion(vcx, "up", None); // Attr(0) = anchor_date
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

    /// Editing a Date attribute opens the segmented field in insert mode with its own
    /// focus handle, the existing date, and the day segment active.
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

    /// Enter completes a pending single digit before committing: typing 2 in the day
    /// writes the second, not the previous twelfth. Exercise both the field's Enter
    /// handler and the commit action.
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

    /// Clicking a segment returns focus to an open date field whose keyboard focus
    /// moved elsewhere.
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
        h.motion(&mut vcx, "right", None);
        h.motion(&mut vcx, "up", None); // Attr(1) = spot_ref
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.tile.read_with(&vcx, |t, _| t.editor_state()).is_some());
        assert!(h.tile.read_with(&vcx, |t, _| t.date_field()).is_none());
        assert_eq!(h.editor_value(&vcx).as_deref(), Some("5000"));
    }

    /// Single-line Input selection actions can bubble into DataTable and move its
    /// selection. The shell reclaims Shift-Up/Down in Input context so the keymap can
    /// route nudging. This tile harness checks that the table selection remains
    /// unchanged.
    #[gpui::test]
    fn shift_up_in_the_editor_no_longer_moves_the_tables_selection(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "down", None);
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

    /// Find is shell-owned and bypasses dispatch, so its first keystroke must close any
    /// open popup itself.
    #[gpui::test]
    fn a_find_keystroke_closes_the_popup(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        vcx.update(|window, cx| h.content.find(FindEvent::Changed("1M".into()), window, cx));
        assert_eq!(h.mode(&vcx), "normal");
    }

    /// Parsed commands close an open popup themselves because the shell command route
    /// bypasses dispatch. Menu instead toggles the popup.
    #[gpui::test]
    fn a_command_line_closes_the_popup(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        assert_eq!(h.mode(&vcx), "menu");
        h.command(&mut vcx, "bump 1").unwrap();
        assert_eq!(h.mode(&vcx), "normal");
    }

    // ---- Underlying picker -------------------------------------------

    /// A panel launched with no underlying opens its picker at once, the
    /// field holding the keyboard, and keeps it through the next frame (the
    /// shell's deferred focus restore spares an insert-mode input).
    #[gpui::test]
    fn a_launched_panel_with_no_underlying_opens_the_picker(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        h.launched(&mut vcx);
        assert_eq!(h.mode(&vcx), "insert");
        vcx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(
            h.tile
                .read_with(&vcx, |t, _| matches!(t.popup, Some(Popup::Picker(_)))),
            "the picker is open"
        );
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "its field holds the keyboard"
        );
    }

    /// A panel launched on an underlying shows it and opens no picker.
    #[gpui::test]
    fn a_launched_panel_on_an_underlying_opens_no_picker(cx: &mut gpui::TestAppContext) {
        let mut state = toml::Table::new();
        state.insert(
            "underlying".into(),
            toml::Value::Array(vec![toml::Value::String("SPX.Z".into())]),
        );
        let (h, mut vcx) = open_with(cx, Some(state));
        h.visible(&mut vcx, true);
        h.launched(&mut vcx);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.popup.is_none()));
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.header_texts())
                .contains(&"SPX.Z".to_string())
        );
    }

    /// Without `launched` (a restore), an empty panel opens no picker.
    #[gpui::test]
    fn an_empty_panel_that_was_not_launched_opens_no_picker(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.visible(&mut vcx, true);
        assert_eq!(h.mode(&vcx), "normal");
        assert!(h.tile.read_with(&vcx, |t, _| t.popup.is_none()));
    }

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

    /// Opening the picker with pending edits is allowed. Selecting a different key
    /// parks the draft under its current underlying.
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

    /// Opening the picker always requests a fresh catalog. A held catalog can omit keys
    /// published since its delivery.
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

    /// Enter selects the highlighted key. Commit defensively refilters current text
    /// because set_value emits no Change, but unchanged text must preserve keyboard
    /// navigation.
    #[gpui::test]
    fn enter_loads_the_highlighted_row_not_the_top_match(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.motion(&mut vcx, "menu_down", None);
        h.motion(&mut vcx, "menu_down", None);
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

    /// Opening the actions menu while the picker is open blurs and drops its focused
    /// Input before installing the menu.
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

    /// Diagnostics with unchanged catalog keys preserve the picker highlight. A changed
    /// catalog reranks matches while keeping the highlighted key by identity.
    #[gpui::test]
    fn diagnostics_catalog_updates_preserve_the_highlight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.diagnostics.update(&mut vcx, |d, cx| {
            d.catalog = Some(catalog(&["AAA.Z", "BBB.Z", "CCC.Z"]));
            cx.notify();
        });
        h.visible(&mut vcx, true);
        h.dispatch(&mut vcx, "load_underlying", None);
        h.motion(&mut vcx, "menu_down", None);
        h.motion(&mut vcx, "menu_down", None);
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

    /// Catalog insertion before the highlighted key shifts its index without changing
    /// selection. Adding AAA.Z before CCC.Z keeps CCC.Z highlighted; removing the
    /// selected key falls back to the first match.
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
        h.motion(&mut vcx, "menu_down", None);
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

    // ---- Popup focus and navigation ----------------------------------

    /// Switching keys cancels an open editor without committing its text into the next
    /// document. The editor can remain open after focus moves to the shell command
    /// field, so this path must close it explicitly.
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

    /// Clicking an attribute cancels an open cell editor before selecting the strip
    /// target. No editor remains attached to the previous cell after the shell restores
    /// tile focus.
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

    /// Selecting the picker action leaves its Input focused. The row click stops
    /// propagation so the host's ordinary tile-click handling does not also run. Shell
    /// insert-focus retention is checked separately.
    ///
    /// Place the cursor in the strip so sync_cursor calls clear_selection, which does
    /// not stop propagation. A grid cursor would call TableState::set_selected_row and
    /// mask whether the popup row itself stopped the click.
    #[gpui::test]
    fn the_menu_row_to_picker_path_leaves_the_pickers_field_focused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.motion(&mut vcx, "up", None);
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
        h.motion(&mut vcx, "up", None);
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

    /// The picker paints at most PICKER_ROWS matches. Navigation beyond that window
    /// scrolls it to keep the highlighted key visible, including the last declared key.
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
        h.motion(&mut vcx, "menu_down", Some(100));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.picker_highlighted_key()),
            Some(keys[19].clone()),
            "the highlight follows to the last declared row, with the \
             window sliding to keep it painted"
        );
    }

    /// Find closes an orphaned picker without blurring the Input that currently owns
    /// focus. A second Input stands in for the shell command field, which must continue
    /// accepting the find query.
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

    // ---- Update policy -----------------------------------------------

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
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.model().len(),
                t.cell_at(0, 0),
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
        h.motion(&mut vcx, "down", None);
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
                t.model().len(),
                t.cell_at(0, 0),
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
        h.motion(&mut vcx, "right", None);
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
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.model().len(),
                t.cell_at(0, 0),
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

    /// Policy changes do not act retroactively. A Behind draft remains held after
    /// switching to rebase and after redelivery of the same generation; only a further
    /// new generation triggers rebase.
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
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.model().len(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
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
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.draft().len(),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
            "a redelivery never acts, got {state:?}"
        );
        assert_eq!(source.as_deref(), Some(BASE));
        assert_eq!(len, 1, "the edit is intact");

        // A further generation under the new policy: onto the NEWEST.
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWEST)));
        let (state, base, source, cell) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().state.clone(),
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.cell_at(0, 0),
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
        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        assert_eq!(written, restored);

        h.command(&mut vcx, "auto hold").unwrap();
        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
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

    /// With the action menu open the panel publishes `tilelist` over
    /// `mode == menu`, so the shared menu steps (j/k, arrows) move the menu
    /// highlight and the grid stays where it was. With no menu open the
    /// flag is gone and a stray menu step moves nothing.
    #[gpui::test]
    fn tilelist_keys_step_the_open_menu_and_leave_the_grid(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
        let ctx = h.tile.read_with(&vcx, |t, _| t.key_context());
        assert!(!ctx.has_flag(geode_shell::keymap::TILELIST));
        h.motion(&mut vcx, "menu_down", None);
        assert_eq!(h.tile.read_with(&vcx, |t, _| t.cursor()), cursor);

        h.dispatch(&mut vcx, "menu", None);
        let ctx = h.tile.read_with(&vcx, |t, _| t.key_context());
        assert!(ctx.has_flag(geode_shell::keymap::TILELIST));
        assert_eq!(ctx.get("mode"), Some("menu"));
        let before = h.tile.read_with(&vcx, |t, _| t.menu_highlighted());
        h.motion(&mut vcx, "menu_down", None);
        let after = h.tile.read_with(&vcx, |t, _| t.menu_highlighted());
        assert_ne!(after, before, "the menu stepped");
        assert_eq!(h.mode(&vcx), "menu", "the step kept the menu open");
        h.motion(&mut vcx, "menu_up", None);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.menu_highlighted()),
            before,
            "the step back returned"
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            cursor,
            "the grid did not move"
        );
        h.dispatch(&mut vcx, "menu_close", None);
        let ctx = h.tile.read_with(&vcx, |t, _| t.key_context());
        assert!(!ctx.has_flag(geode_shell::keymap::TILELIST));
    }

    /// The menu's `On new document` section ticks the policy in force,
    /// and picking another row sets it and closes the menu — through the
    /// ordinary `menu_pick` path, two `j`s down from the first row on a
    /// clean draft (`Load`, then `hold edits` — the greyed `Upload` and
    /// `Revert` are stepped over — then `rebase edits`).
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

        for _ in 0..2 {
            h.motion(&mut vcx, "menu_down", None);
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
        h.motion(&mut vcx, "right", None);
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
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
                (0..2).map(|c| t.cell_at(0, c)).collect::<Vec<_>>(),
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

    /// The first usable delivery after restore holds edits even under replace. A newer
    /// document yields Behind with the restored edits intact and no replaced notice.
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
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
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
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
            "a redelivery never acts, got {state:?}"
        );
        assert_eq!(len, 1);

        // A FURTHER generation: the policy resumes and acts.
        const NEWEST: &str = "2026-09-12T14:15:00Z";
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWEST)));
        let (draft, source, notice) = h.tile.read_with(&vcx, |t, _| {
            (
                t.draft().clone(),
                t.model().base.as_ref().map(|b| b.as_of.clone()),
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
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
                t.draft().len(),
            )
        });
        assert_eq!(policy, UpdatePolicy::Rebase);
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
            "Behind, not rebased — got {state:?}"
        );
        assert_eq!(base.as_deref(), Some(BASE), "the base is the restored one");
        assert_eq!(len, 1);
    }

    /// Automatic rebase against an empty new generation falls back to Hold. The draft
    /// stays Behind with edits intact and its base painted, without an extra notice.
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
                t.model().base.as_ref().map(|b| b.as_of.clone()),
                t.model().len(),
                t.notice().map(str::to_string),
            )
        });
        assert!(
            matches!(state, DraftState::Behind { ref newer } if newer.as_of == NEWER),
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
        h.motion(&mut vcx, "right", None);
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
                t.draft().base.as_ref().map(|b| b.as_of.clone()),
                (0..2).map(|c| t.cell_at(0, c)).collect::<Vec<_>>(),
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

    /// Tile commands leave the frame's scope, grouping, and as-of counters unchanged.
    /// Counters detect mutations even when an operation would leave the selected value
    /// unchanged.
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
            "autosize",
            "autosize reset",
        ];
        for word in crate::commands::VERBS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "no sweep line for `:{word}`"
            );
        }
        let before = h.frame.read_with(&vcx, |f, _| f.shared().versions());
        for line in lines {
            assert!(
                crate::commands::parse(line).is_ok(),
                "`{line}` no longer parses"
            );
            let _ = vcx.update(|window, cx| h.content.command(line, window, cx));
            let after = h.frame.read_with(&vcx, |f, _| f.shared().versions());
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
    /// The width the delegate hands the table for the column headed `name`.
    fn width_of_column(h: &Harness, vcx: &gpui::VisualTestContext, name: &str) -> f32 {
        let ix = h
            .headers(vcx)
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("no column '{name}'"));
        h.tile.read_with(vcx, |t, cx| {
            f32::from(
                gpui_component::table::TableDelegate::column(t.table().read(cx).delegate(), ix, cx)
                    .width,
            )
        })
    }

    const LONG_STATUS: &str = "provisionally estimated by the desk";

    /// `:autosize` through the content's command route fits a column whose
    /// text outgrows the default width; the fit survives the refresh every
    /// model install runs; `:autosize reset` returns to the default.
    #[gpui::test]
    fn autosize_fits_every_row_survives_a_model_install_and_resets(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot(&[
                ("D1", "2026-12-18", 1.25, "declared"),
                ("D2", "2027-03-19", 0.5, LONG_STATUS),
            ]),
        );
        let default = width_of_column(&h, &vcx, "status");
        h.command(&mut vcx, "autosize").unwrap();
        let fitted = width_of_column(&h, &vcx, "status");
        let rem = vcx.update(|window, _| f32::from(window.rem_size()));
        let text = LONG_STATUS.chars().count() as f32 * rem * 0.875 * 0.6;
        assert!(fitted > default, "{fitted} > {default}");
        assert!(fitted >= text, "{fitted} holds the last row's {text}px");

        h.tile.update(&mut vcx, |t, cx| t.install_model(cx));
        assert_eq!(width_of_column(&h, &vcx, "status"), fitted);

        h.command(&mut vcx, "autosize reset").unwrap();
        assert_eq!(width_of_column(&h, &vcx, "status"), default);
    }

    /// Before any document, `:autosize` refuses and keeps the restored
    /// widths; `:autosize reset` still drops them.
    #[gpui::test]
    fn autosize_with_no_document_refuses_and_keeps_the_widths(cx: &mut gpui::TestAppContext) {
        let mut widths = toml::Table::new();
        widths.insert("status".into(), toml::Value::Float(200.0));
        let mut record = toml::Table::new();
        record.insert(
            geode_shell::colfit::SESSION_KEY.into(),
            toml::Value::Table(widths),
        );
        let (h, mut vcx) = open_spec(cx, &test_fixtures::SCHEDULE, Some(record));
        let fitted = |h: &Harness, vcx: &gpui::VisualTestContext| {
            h.tile
                .read_with(vcx, |t, cx| t.table().read(cx).delegate().fitted.clone())
        };
        let before = fitted(&h, &vcx);
        assert_eq!(before.len(), 1);
        assert_eq!(
            h.command(&mut vcx, "autosize"),
            Err(geode_shell::colfit::NOTHING_TO_FIT.to_string())
        );
        assert_eq!(fitted(&h, &vcx), before);
        h.command(&mut vcx, "autosize reset").unwrap();
        assert!(fitted(&h, &vcx).is_empty());
    }

    /// Fitted widths ride the session record. On restore a key the model
    /// still has is used, one it lacks is ignored, and a column with no
    /// entry keeps the default.
    #[gpui::test]
    fn autosize_widths_round_trip_the_session(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot(&[("D1", "2026-12-18", 1.25, LONG_STATUS)]),
        );
        let default_amount = width_of_column(&h, &vcx, "amount");
        h.command(&mut vcx, "autosize").unwrap();
        let fitted = width_of_column(&h, &vcx, "status");
        let mut record = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
        let widths = record
            .get_mut(geode_shell::colfit::SESSION_KEY)
            .and_then(|v| v.as_table_mut())
            .expect("widths persisted");
        widths.remove("amount");
        widths.insert("gone".into(), toml::Value::Float(300.0));

        let (h2, mut vcx2) = open_spec(cx, &test_fixtures::SCHEDULE, Some(record));
        h2.with_flat_document(&mut vcx2);
        assert_eq!(width_of_column(&h2, &vcx2, "status"), fitted);
        assert_eq!(width_of_column(&h2, &vcx2, "amount"), default_amount);
        assert_eq!(h2.headers(&vcx2).len(), h.headers(&vcx).len());
    }

    // ---- The window follows the table -------------------------------

    /// `n` schedule rows with distinct labels `D{i}` and dates, amount `i`.
    fn schedule_rows(n: usize) -> Vec<(&'static str, &'static str, f64, &'static str)> {
        (0..n)
            .map(|i| {
                let label: &'static str = Box::leak(format!("D{i}").into_boxed_str());
                let date: &'static str = Box::leak(
                    format!("{:04}-01-{:02}", 2027 + i / 28, i % 28 + 1).into_boxed_str(),
                );
                (label, date, i as f64, "declared")
            })
            .collect()
    }

    fn schedule_of(n: usize) -> Snapshot {
        test_fixtures::schedule_snapshot(&schedule_rows(n))
    }

    /// A drawn table fills the window for the rows it shows, and only those.
    #[gpui::test]
    fn the_table_report_fills_only_its_range(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(&mut vcx, schedule_of(200));
        draw(&mut vcx);
        let window = h
            .tile
            .read_with(&vcx, |t, cx| t.table().read(cx).delegate().window.window());
        assert!(window.start == 0 && window.end < 200, "{window:?}");
        assert!(h.painted(&vcx, 0, 0).is_some());
        assert_eq!(h.painted(&vcx, 199, 0), None, "off screen is not prepared");
        // Scrolled to the last row, the window is what the table now shows,
        // never the first window the install prepared.
        h.motion(&mut vcx, "bottom", None);
        draw(&mut vcx);
        assert_eq!(
            h.painted(&vcx, 199, 1).as_deref(),
            Some("199.0000"),
            "the last row is prepared"
        );
        assert_eq!(h.painted(&vcx, 0, 0), None, "the first row left the window");
    }

    /// Scrolled to the bottom of a long document, a one-row redelivery
    /// still paints its row: the recorded range lies past the new end, and
    /// the table never reports a one-row range to fill it.
    #[gpui::test]
    fn a_shrink_to_one_row_after_a_scroll_paints_the_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.command(&mut vcx, "key SPX.Z").expect("a valid key");
        h.visible(&mut vcx, true);
        let tag = h.document_request().expect("one request").tag;
        h.deliver(&mut vcx, tag, Arc::new(schedule_of(200)));
        draw(&mut vcx);
        h.motion(&mut vcx, "bottom", None);
        draw(&mut vcx);
        assert!(h.painted(&vcx, 199, 0).is_some(), "scrolled to the end");
        h.deliver(
            &mut vcx,
            tag,
            Arc::new(test_fixtures::schedule_snapshot_at(
                &[("D1", "2026-12-18", 1.25, "declared")],
                NEWER,
            )),
        );
        assert_eq!(h.rows(&vcx), 1);
        assert_eq!(h.painted(&vcx, 0, 0).as_deref(), Some("2026-12-18"));
        draw(&mut vcx);
        assert_eq!(h.painted(&vcx, 0, 0).as_deref(), Some("2026-12-18"));
    }

    /// A redelivery that leaves the reported range unchanged still repaints:
    /// the install refills the recorded range itself.
    #[gpui::test]
    fn a_redelivery_in_an_unchanged_range_repaints_the_window(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        let tag = h.with_document_tagged(&mut vcx);
        draw(&mut vcx);
        let before = h.painted(&vcx, 0, 0);
        h.deliver(&mut vcx, tag, Arc::new(cvi_with_forward(NEWER, 4600.0)));
        assert_ne!(h.painted(&vcx, 0, 0), before);
        assert_eq!(h.painted(&vcx, 0, 0).as_deref(), Some("4600.00"));
    }

    /// One row: the table never reports a range of length one, and the
    /// first window still paints it.
    #[gpui::test]
    fn a_one_row_document_paints_without_a_table_report(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document_with(
            &mut vcx,
            test_fixtures::schedule_snapshot(&[("D1", "2026-12-18", 1.25, "declared")]),
        );
        draw(&mut vcx);
        assert!(h.painted(&vcx, 0, 0).is_some());
    }

    /// An open editor paints in its cell even when the window lacks that
    /// cell. The first draw records the table's range, so the second one
    /// (same range) brings no report that would refill the cleared window.
    #[gpui::test]
    fn the_editor_paints_on_a_cell_the_window_lacks(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.dispatch(&mut vcx, "edit", None);
        draw(&mut vcx);
        h.tile.update(&mut vcx, |t, cx| {
            t.table().update(cx, |t, _| t.delegate_mut().window.clear())
        });
        draw(&mut vcx);
        assert_eq!(h.painted(&vcx, 0, 0), None, "the window lacks the cell");
        assert!(
            vcx.debug_bounds("marketdata-editor-0-1").is_some(),
            "the editor still paints"
        );
    }

    /// `:autosize` measures the rows on screen: a wider value off screen does not widen.
    #[gpui::test]
    fn autosize_measures_the_window_not_the_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        let mut doc = schedule_rows(200);
        doc[199].3 = LONG_STATUS;
        h.with_flat_document_with(&mut vcx, test_fixtures::schedule_snapshot(&doc));
        draw(&mut vcx);
        let default = width_of_column(&h, &vcx, "status");
        h.command(&mut vcx, "autosize").unwrap();
        assert!(
            width_of_column(&h, &vcx, "status") < default + 1.0,
            "the long status is off screen"
        );
    }

    // ---- Row insertion and deletion ----------------------------------

    /// The model's row labels in painted order.
    fn row_labels(h: &Harness, vcx: &gpui::VisualTestContext) -> Vec<String> {
        h.tile.read_with(vcx, |t, _| {
            t.model().rows().map(|r| r.label.to_string()).collect()
        })
    }

    fn notice_of(h: &Harness, vcx: &gpui::VisualTestContext) -> Option<String> {
        h.tile.read_with(vcx, |t, _| t.notice().map(str::to_string))
    }

    /// On a Minted axis, insertion below opens the new row's first cell; insertion
    /// above reanchors the selected inserted row beneath its new predecessor. Deleting
    /// an inserted row removes it, while deleting a document row marks it Deleted and a
    /// repeat refuses.
    ///
    /// With row labels hidden, table column zero is the first value column, yank copies
    /// cells, and find searches painted cells. Inserts still mint draft identities.
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

    /// On a Typed(Date) axis, insertion opens a segmented row-label editor. Committing
    /// a unique term renames the provisional row and opens its first cell. A duplicate
    /// keeps the editor open; cancel drops the provisional row.
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
                .rows()
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

    /// Two inserts after the same document row reanchor the first beneath the second.
    /// Committing the second's typed label updates the follower's anchor, preserving
    /// painted order D1, second, first, D2.
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
        h.motion(&mut vcx, "up", None);
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
        h.motion(&mut vcx, "up", None); // Attr(0)
        for verb in ["insert_below", "insert_above", "delete_row"] {
            h.dispatch(&mut vcx, verb, None);
            assert_eq!(notice_of(&h, &vcx).as_deref(), Some("not a row"), "{verb}");
            assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
            assert!(h.editor_value(&vcx).is_none());
        }
        h.motion(&mut vcx, "down", None);
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
        h.motion(&mut vcx, "right", None); // amount
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

    /// Row edits persist under drafts.<key> and park with other edits on key switches.
    /// Filled inserted cells survive restore, and yank copies their prepared values.
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
        h.motion(&mut vcx, "right", None);
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

        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));
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

    /// Insertion is immediately after its parent, reanchoring an existing follower
    /// beneath the new row. Two inserts after D1 paint D1, new-2, new-1, and renaming
    /// either preserves that order.
    #[gpui::test]
    fn o_rehangs_the_existing_follower_onto_the_new_row(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_flat(cx);
        h.with_flat_document(&mut vcx);
        h.dispatch(&mut vcx, "insert_below", None);
        draw(&mut vcx);
        type_keys(&mut vcx, "escape");
        h.motion(&mut vcx, "up", None); // back on D1
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
            h.frame.read_with(&vcx, |f, _| f
                .barrier_wants(QueryKey(TILE), f.shared().versions())),
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

    // ---- Upload ------------------------------------------------------

    /// CVI's panel with one egress target, `sophis`, that accepts its
    /// document (`cvi_params`) — and a second, `bbg`, that does not.
    fn open_upload(cx: &mut gpui::TestAppContext) -> (Harness, gpui::VisualTestContext) {
        open_spec_with_egress(
            cx,
            &CVI,
            None,
            vec![
                ("sophis".into(), vec!["cvi_params".into()]),
                ("bbg".into(), vec!["dividend_schedule".into()]),
            ],
        )
    }

    impl Harness {
        /// The next UPLOAD request, skipping the document requests and
        /// cancels a test's own setup puts on the same channel.
        fn upload_request(&self) -> Option<geode_data::UploadParams> {
            loop {
                match self.rx.try_recv() {
                    Ok(Request::Upload(params)) => return Some(params),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
        }
        fn deliver_upload(
            &self,
            vcx: &mut gpui::VisualTestContext,
            tag: u64,
            result: Result<(), String>,
        ) {
            let u = geode_shell::module::UploadDelivery {
                key: QueryKey(TILE),
                tag,
                target: "sophis".into(),
                result,
            };
            vcx.update(|window, cx| self.content.deliver(Delivery::Upload(u), window, cx));
        }
        /// One committed cell edit on the cursor cell.
        fn edit_one_cell(&self, vcx: &mut gpui::VisualTestContext) {
            self.dispatch(vcx, "edit", None);
            self.set_editor(vcx, "4505.5");
            self.dispatch(vcx, "commit", None);
        }
        fn upload_prompt(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.upload_prompt().map(str::to_string))
        }
    }

    #[gpui::test]
    fn upload_is_refused_on_a_clean_draft(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err("nothing to upload".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None, "nothing armed");
        assert!(h.upload_request().is_none());
    }

    #[gpui::test]
    fn upload_is_refused_on_a_behind_draft(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err(
                "rebase or revert first: an upload must be of a document you have seen whole"
                    .into()
            )
        );
        assert_eq!(h.upload_prompt(&vcx), None);
        assert!(h.upload_request().is_none());
    }

    #[gpui::test]
    fn upload_is_refused_with_an_incomplete_row(cx: &mut gpui::TestAppContext) {
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
        let (h, mut vcx) = open_spec_with_egress(
            cx,
            &test_fixtures::SCHEDULE,
            Some(restored),
            vec![("sophis".into(), vec!["div_schedule".into()])],
        );
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
            h.command(&mut vcx, "upload"),
            Err("1 row incomplete".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None);
        assert!(h.upload_request().is_none());
    }

    #[gpui::test]
    fn upload_is_refused_with_no_eligible_target(cx: &mut gpui::TestAppContext) {
        // No egress at all: nothing accepts the document.
        let (h, mut vcx) = open(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err("no egress target accepts cvi_params".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None);

        // A named target that does not take this document.
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "upload bbg"),
            Err("bbg does not accept cvi_params".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None);

        // Several eligible and no argument: the trader names one.
        let (h, mut vcx) = open_spec_with_egress(
            cx,
            &CVI,
            None,
            vec![
                ("sophis".into(), vec!["cvi_params".into()]),
                ("bbg".into(), vec!["cvi_params".into()]),
            ],
        );
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err("upload to which target? sophis, bbg".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None);
        assert!(
            h.command(&mut vcx, "upload bbg").is_ok(),
            "a named one arms"
        );
    }

    #[gpui::test]
    fn upload_arms_a_confirm_and_y_submits_the_assembled_document(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let expected = h.tile.read_with(&vcx, |t, _| {
            crate::core::upload::assemble(
                &t.painted_snapshot().unwrap(),
                &t.spec,
                t.model(),
                t.draft(),
            )
            .unwrap()
        });

        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        let prompt = "upload 1 cell, 0 rows added, 0 removed of SPX.Z to sophis? (y/n)";
        assert_eq!(h.upload_prompt(&vcx).as_deref(), Some(prompt));
        assert!(
            h.header_texts(&vcx).contains(&prompt.to_string()),
            "the header paints the question: {:?}",
            h.header_texts(&vcx)
        );
        assert_eq!(h.mode(&vcx), "insert", "the confirm holds the keyboard");
        assert!(
            h.upload_request().is_none(),
            "nothing sent before the answer"
        );

        let keys_before = h.host_keys();
        type_keys(&mut vcx, "y");
        let sent = h.upload_request().expect("y submits");
        assert_eq!(sent.key, QueryKey(TILE));
        assert_eq!(sent.tag, 1);
        assert_eq!(sent.target, "sophis");
        assert_eq!(sent.document, "cvi_params");
        assert_eq!(sent.rows, expected, "assemble's own rows, whole");
        assert_eq!(h.host_keys(), keys_before, "the y was the confirm's alone");
        assert_eq!(h.upload_prompt(&vcx), None, "disarmed");
        assert_eq!(h.mode(&vcx), "normal");
        assert!(
            !h.tile.read_with(&vcx, |t, _| t.draft().is_sent()),
            "Sent waits for the outcome"
        );
    }

    #[gpui::test]
    fn any_other_key_cancels_the_confirm_and_is_consumed(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);

        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        let keys_before = h.host_keys();
        type_keys(&mut vcx, "n");
        assert_eq!(h.upload_prompt(&vcx), None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled".into())
        );
        assert!(h.upload_request().is_none());
        assert_eq!(h.host_keys(), keys_before, "n was consumed");

        let cursor = h.tile.read_with(&vcx, |t, _| t.cursor());
        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        type_keys(&mut vcx, "j");
        assert_eq!(h.upload_prompt(&vcx), None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled".into())
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.cursor()),
            cursor,
            "j did not move"
        );
        assert_eq!(h.host_keys(), keys_before, "j was consumed too");
        assert!(h.upload_request().is_none());
    }

    /// The upload confirm's No button is any other key and its Yes button
    /// is `y`: the confirm door paints both, and a press on either does not
    /// cancel the question before its click lands.
    #[gpui::test]
    fn the_upload_confirms_yes_and_no_buttons_answer_it(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let click = |vcx: &mut gpui::VisualTestContext, selector: &str| {
            let at = centre_of(vcx, selector);
            vcx.simulate_click(at, gpui::Modifiers::default());
            vcx.run_until_parked();
            draw(vcx);
        };

        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        click(&mut vcx, &format!("marketdata-upload-confirm-{TILE}-no"));
        assert_eq!(h.upload_prompt(&vcx), None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(UPLOAD_CANCELLED.into())
        );
        assert!(h.upload_request().is_none(), "No sends nothing");

        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        click(&mut vcx, &format!("marketdata-upload-confirm-{TILE}-yes"));
        let sent = h.upload_request().expect("Yes submits, as y does");
        assert_eq!(sent.target, "sophis");
        assert_eq!(h.upload_prompt(&vcx), None, "disarmed");
        assert_eq!(h.mode(&vcx), "normal");
    }

    #[gpui::test]
    fn focus_leaving_the_tile_cancels_the_confirm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        // gpui diffs focus paths only for an ACTIVE window (the shell's
        // scope-bar tests carry the same activation note).
        vcx.update(|window, _cx| window.activate_window());
        vcx.run_until_parked();
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        assert_eq!(h.command(&mut vcx, "upload"), Ok(()));
        draw(&mut vcx);
        assert!(
            vcx.update(|window, cx| h.content.holds_focus(window, cx)),
            "the prompt holds the keyboard"
        );
        // Focus moves elsewhere — as a tile-focus move's root restore, the
        // palette or a click into the grid would move it.
        vcx.update(|window, cx| window.blur(cx));
        draw(&mut vcx);
        assert_eq!(h.upload_prompt(&vcx), None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled".into())
        );
        type_keys(&mut vcx, "y");
        assert!(h.upload_request().is_none(), "a later y sends nothing");
    }

    #[gpui::test]
    fn an_ok_outcome_enters_sent_and_the_header_reads_sent_hhmm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().unwrap().tag;
        h.deliver_upload(&mut vcx, tag, Ok(()));
        let (at, clock) = h.tile.read_with(&vcx, |t, _| match &t.draft().state {
            DraftState::Sent { at } => (at.clone(), t.clock),
            other => panic!("expected Sent, got {other:?}"),
        });
        assert!(chrono::DateTime::parse_from_rfc3339(&at).is_ok(), "{at}");
        let want = format!("sent {}", crate::core::draft::local_hhmm(&at, clock));
        assert!(
            h.header_texts(&vcx).contains(&want),
            "{:?}",
            h.header_texts(&vcx)
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.sent.is_some()),
            "kept for the echo"
        );
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err("already sent".into()),
            "a Sent draft with no edit since"
        );
    }

    #[gpui::test]
    fn an_err_outcome_keeps_editing_and_shows_the_error(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().unwrap().tag;
        h.deliver_upload(&mut vcx, tag, Err("sophis is down".into()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.sent.is_none()));
        let err = "upload failed: sophis is down".to_string();
        assert!(
            h.header_texts(&vcx).contains(&err),
            "{:?}",
            h.header_texts(&vcx)
        );
        // Escape clears a notice; the failure stays until the next edit.
        h.dispatch(&mut vcx, "escape", None);
        assert!(h.header_texts(&vcx).contains(&err));
        h.motion(&mut vcx, "down", None);
        h.edit_one_cell(&mut vcx);
        assert!(
            !h.header_texts(&vcx).contains(&err),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    #[gpui::test]
    fn a_stale_upload_tag_is_ignored(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().unwrap().tag;
        h.deliver_upload(&mut vcx, tag + 1, Ok(()));
        h.deliver_upload(&mut vcx, tag - 1, Err("old".into()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing
        );
        assert!(
            h.tile.read_with(&vcx, |t, _| t.sent.is_some()),
            "still awaiting its own outcome"
        );
        assert!(
            !h.header_texts(&vcx)
                .iter()
                .any(|t| t.starts_with("upload failed"))
        );
    }

    /// An upload is a whole document: one assembled over a historical
    /// generation would revert every untouched row upstream, so `:upload`
    /// is refused unless the panel follows live — naming the as-of as the
    /// as-of chip would, the time alone today, the date and time otherwise.
    #[gpui::test]
    fn upload_is_refused_under_a_historical_as_of(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let now = chrono::Utc::now();
        for at in [
            now - chrono::Duration::seconds(60),
            now - chrono::Duration::days(3),
        ] {
            h.frame.update(&mut vcx, |f, cx| {
                f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
                cx.notify();
            });
            vcx.run_until_parked();
            let when = as_of_text(at, chrono::Utc::now(), clock);
            assert_eq!(
                h.command(&mut vcx, "upload"),
                Err(format!("upload: the panel shows {when}, not live"))
            );
            assert_eq!(h.upload_prompt(&vcx), None, "nothing armed");
            assert!(h.upload_request().is_none());
        }
        let today = as_of_text(now - chrono::Duration::seconds(60), now, clock);
        assert_eq!(today.len(), "HH:MM".len(), "{today}");
        let older = as_of_text(now - chrono::Duration::days(3), now, clock);
        assert_eq!(older.len(), "YYYY-MM-DD HH:MM".len(), "{older}");

        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(geode_core::query::AsOf::Live);
            cx.notify();
        });
        vcx.run_until_parked();
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        assert_eq!(h.command(&mut vcx, "upload"), Ok(()), "live again: armed");
    }

    /// The frame goes live, but the generation on screen — the one
    /// `:upload` assembles — was delivered for a historical request and
    /// stays painted until a live one is applied. Harness: frame `At`,
    /// the historical document delivered, frame `Live` with its
    /// requery left unanswered (`answer`: `None`) or answered `Err`
    /// (`Some`). An edit, then `:upload`, is refused naming the painted
    /// as-of, and nothing is sent.
    fn upload_refused_over_a_painted_historical_generation(
        cx: &mut gpui::TestAppContext,
        answer: Option<&str>,
    ) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let at = chrono::Utc::now() - chrono::Duration::seconds(60);
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
            cx.notify();
        });
        vcx.run_until_parked();
        let tag = h.document_request().expect("the historical request").tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi_requested_at(BASE, at)));
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(geode_core::query::AsOf::Live);
            cx.notify();
        });
        vcx.run_until_parked();
        let live = h.document_request().expect("the live request").tag;
        if let Some(error) = answer {
            h.deliver_err(&mut vcx, live, error);
        }
        h.edit_one_cell(&mut vcx);
        let when = as_of_text(at, chrono::Utc::now(), clock);
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err(format!("upload: the panel shows {when}, not live"))
        );
        assert_eq!(h.upload_prompt(&vcx), None, "nothing armed");
        assert!(h.upload_request().is_none(), "nothing sent");

        // Positive control: the live generation applied, `:upload` arms.
        h.deliver(&mut vcx, live, Arc::new(cvi(BASE)));
        assert_eq!(h.command(&mut vcx, "upload"), Ok(()), "live painted: armed");
    }

    #[gpui::test]
    fn upload_is_refused_while_a_historical_generation_is_painted(cx: &mut gpui::TestAppContext) {
        upload_refused_over_a_painted_historical_generation(cx, None);
    }

    #[gpui::test]
    fn upload_is_refused_when_the_live_requery_fails_over_a_historical_generation(
        cx: &mut gpui::TestAppContext,
    ) {
        upload_refused_over_a_painted_historical_generation(cx, Some("disk gone"));
    }

    /// `y` re-checks: a confirm armed live, then the frame moved to a
    /// historical as-of before the answer, sends nothing and says why.
    #[gpui::test]
    fn upload_confirm_rechecks_the_as_of_at_y(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        let clock = h.tile.read_with(&vcx, |t, _| t.clock);
        let at = chrono::Utc::now() - chrono::Duration::seconds(60);
        h.frame.update(&mut vcx, |f, cx| {
            f.shared_mut().set_as_of(geode_core::query::AsOf::At(at));
            cx.notify();
        });
        vcx.run_until_parked();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        assert!(h.upload_request().is_none(), "nothing sent");
        let when = as_of_text(at, chrono::Utc::now(), clock);
        let refusal = format!("upload: the panel shows {when}, not live");
        assert!(
            h.header_texts(&vcx).contains(&refusal),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// A second `:upload` while the first awaits its outcome is refused:
    /// it would race the first's echo.
    #[gpui::test]
    fn upload_is_refused_while_an_upload_is_in_flight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().expect("submitted").tag;
        h.motion(&mut vcx, "down", None);
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Err("an upload of SPX.Z to sophis is in flight".into())
        );
        assert_eq!(h.upload_prompt(&vcx), None);
        assert!(h.upload_request().is_none());
        h.deliver_upload(&mut vcx, tag, Ok(()));
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Ok(()),
            "answered: the next upload arms"
        );
    }

    /// Upload SPX, switch to NDX with an edit of its own, then deliver
    /// SPX's outcome: a notice naming SPX and the target, and nothing else
    /// on NDX. Switching back, SPX's draft has given up its echo check —
    /// it restores `Editing`, never `Sent`.
    fn outcome_after_a_key_switch(
        cx: &mut gpui::TestAppContext,
        result: Result<(), String>,
        notice: &str,
    ) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().expect("submitted").tag;

        h.command(&mut vcx, "key NDX.Z").expect("a valid key");
        let request = h.document_request().expect("NDX requested").tag;
        h.deliver(&mut vcx, request, Arc::new(cvi(BASE)));
        h.edit_one_cell(&mut vcx);
        let before = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert!(
            h.tile.read_with(&vcx, |t, _| t.sent.is_none()),
            "switch drops sent"
        );

        h.deliver_upload(&mut vcx, tag, result);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(notice.to_string())
        );
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().clone()),
            before,
            "NDX's draft is untouched"
        );
        let texts = h.header_texts(&vcx);
        assert!(
            !texts
                .iter()
                .any(|t| t.starts_with("upload failed") || t.starts_with("sent ")),
            "{texts:?}"
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.sent.is_none()));

        h.command(&mut vcx, "key SPX.Z").expect("back to SPX");
        let request = h.document_request().expect("SPX requested").tag;
        h.deliver(&mut vcx, request, Arc::new(cvi(BASE)));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing,
            "a switched-away draft restores unsent"
        );
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Ok(()),
            "and may be sent again"
        );
    }

    #[gpui::test]
    fn an_err_outcome_after_a_key_switch_is_a_notice_naming_the_key(cx: &mut gpui::TestAppContext) {
        outcome_after_a_key_switch(
            cx,
            Err("sophis is down".into()),
            "upload of SPX.Z to sophis failed: sophis is down",
        );
    }

    #[gpui::test]
    fn an_ok_outcome_after_a_key_switch_is_a_notice_naming_the_key(cx: &mut gpui::TestAppContext) {
        outcome_after_a_key_switch(cx, Ok(()), "upload of SPX.Z to sophis sent");
    }

    /// The confirm counts every kind of edit it is about to send, an
    /// attribute included.
    #[gpui::test]
    fn the_confirm_counts_attribute_edits(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "set spot_ref 4520").unwrap();
        h.command(&mut vcx, "upload").unwrap();
        assert_eq!(
            h.upload_prompt(&vcx).as_deref(),
            Some("upload 0 cells, 1 attribute, 0 rows added, 0 removed of SPX.Z to sophis? (y/n)")
        );
    }

    #[gpui::test]
    fn a_refused_submit_says_so_and_sends_nothing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        h.data.shutdown();
        type_keys(&mut vcx, "y");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload refused: the data service has stopped".into())
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.sent.is_none()));
    }

    #[gpui::test]
    fn a_busy_upload_refusal_says_busy_and_clears_in_flight(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        h.data.fill_for_tests();
        type_keys(&mut vcx, "y");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload refused: the data service is busy".into())
        );
        assert!(
            h.tile
                .read_with(&vcx, |t, _| t.sent.is_none() && t.in_flight.is_none())
        );
    }

    #[gpui::test]
    fn the_menus_upload_row_arms_the_same_confirm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.dispatch(&mut vcx, "menu", None);
        h.motion(&mut vcx, "menu_down", None); // Upload
        h.dispatch(&mut vcx, "menu_pick", None);
        draw(&mut vcx);
        assert_eq!(
            h.upload_prompt(&vcx).as_deref(),
            Some("upload 1 cell, 0 rows added, 0 removed of SPX.Z to sophis? (y/n)")
        );
        type_keys(&mut vcx, "y");
        assert!(h.upload_request().is_some());
    }

    #[gpui::test]
    fn upload_completes_the_panels_eligible_targets(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        let c = vcx.update(|_, cx| h.content.completions("upload ", 7, cx));
        assert_eq!(c, vec!["sophis".to_string()]);
    }

    #[gpui::test]
    fn a_delivery_under_the_question_cancels_the_y(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_behind()));
        assert_eq!(h.upload_prompt(&vcx), None, "withdrawn on the delivery");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled: a new document arrived".into())
        );
        type_keys(&mut vcx, "y");
        assert!(
            h.upload_request().is_none(),
            "the prompt's document is no longer the one on screen"
        );
    }

    /// The critical case the whole-draft comparison exists for: under
    /// `:auto rebase` a same-shape newer generation moves only `base` —
    /// edits (keyed by index), attrs, rows and state all compare equal —
    /// so an edits-only check let `y` send rows assembled from the
    /// superseded base while the header named the newer one.
    #[gpui::test]
    fn a_rebase_under_the_question_withdraws_the_confirm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "auto rebase").unwrap();
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        assert!(h.upload_prompt(&vcx).is_some(), "armed");
        let prompt_focus = h.tile.read_with(&vcx, |t, _| {
            t.pending_upload
                .as_ref()
                .expect("armed")
                .focus_handle()
                .clone()
        });
        assert!(vcx.update(|window, _| prompt_focus.is_focused(window)));
        let before = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        let after = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert_eq!(
            after.base.as_ref().map(|b| b.as_of.as_str()),
            Some(NEWER),
            "rebased, not Behind"
        );
        assert!(!after.is_behind());
        assert!(
            same_edits(&before, &after) && before.state == after.state,
            "the premise: only base moved"
        );
        assert_eq!(h.upload_prompt(&vcx), None, "withdrawn at once");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled: a new document arrived".into())
        );
        draw(&mut vcx);
        assert!(
            !vcx.update(|window, _| prompt_focus.is_focused(window)),
            "the prompt's focus was blurred, not dropped while focused"
        );
        type_keys(&mut vcx, "y");
        assert!(
            h.upload_request().is_none(),
            "y must not send the superseded base's rows"
        );
    }

    /// `y`'s own re-check, the second line behind the withdrawal on
    /// delivery: no production route moves the draft under a standing
    /// question today (every key and press answers it, every delivery
    /// withdraws it), so the draft is moved here directly — a `base`-only
    /// move, the rebase shape an edits-only comparison misses.
    #[gpui::test]
    fn y_rechecks_the_whole_draft_base_included(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        h.tile.update(&mut vcx, |t, _| {
            t.draft.base = Some(at(NEWER));
        });
        type_keys(&mut vcx, "y");
        assert!(h.upload_request().is_none(), "a moved base sends nothing");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled: the draft changed under the question".into())
        );
    }

    #[gpui::test]
    fn an_ok_after_a_rebase_in_flight_does_not_enter_sent(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.command(&mut vcx, "auto rebase").unwrap();
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let upload_tag = h.upload_request().expect("submitted").tag;
        let tag = h.tile.read_with(&vcx, |t, _| t.following.tag());
        h.deliver(&mut vcx, tag, Arc::new(cvi(NEWER)));
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t
                    .draft()
                    .base
                    .as_ref()
                    .map(|b| b.as_of.clone()))
                .as_deref(),
            Some(NEWER),
            "rebased while in flight"
        );
        h.deliver_upload(&mut vcx, upload_tag, Ok(()));
        let state = h.tile.read_with(&vcx, |t, _| t.draft().state.clone());
        assert_eq!(state, DraftState::Editing, "the newer base was never sent");
    }

    #[gpui::test]
    fn a_pointer_press_on_the_tile_cancels_the_confirm(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        let at = centre_of(&mut vcx, &format!("marketdata-header-{TILE}"));
        click_at(&mut vcx, at, 1);
        assert_eq!(h.upload_prompt(&vcx), None);
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some("upload cancelled".into())
        );
        assert!(h.upload_request().is_none());
    }

    // ---- Upload echo -------------------------------------------------

    impl Harness {
        /// `:upload`, `y` and an `Ok` outcome over the draft as it stands:
        /// a `Sent` draft, answering the rows that went out.
        fn upload_ok(&self, vcx: &mut gpui::VisualTestContext) -> DocumentRows {
            self.command(vcx, "upload").expect("armed");
            draw(vcx);
            type_keys(vcx, "y");
            let req = self.upload_request().expect("y submits");
            self.deliver_upload(vcx, req.tag, Ok(()));
            assert!(
                self.tile.read_with(vcx, |t, _| t.draft().is_sent()),
                "the premise: Sent"
            );
            req.rows
        }
        /// A further generation answering the panel's own latest request —
        /// what the upstream's publish of the sent document arrives as.
        fn echo(&self, vcx: &mut gpui::VisualTestContext, snapshot: Snapshot) {
            let tag = self.tile.read_with(vcx, |t, _| t.following.tag());
            self.deliver(vcx, tag, Arc::new(snapshot));
        }
        fn sent_at(&self, vcx: &gpui::VisualTestContext) -> String {
            self.tile.read_with(vcx, |t, _| match &t.draft().state {
                DraftState::Sent { at } => local_hhmm(at, t.clock),
                other => panic!("expected Sent, got {other:?}"),
            })
        }
        fn painted_as_of(&self, vcx: &gpui::VisualTestContext) -> Option<String> {
            self.tile
                .read_with(vcx, |t, _| t.model().base.as_ref().map(|b| b.as_of.clone()))
        }
        fn sent_rows(&self, vcx: &gpui::VisualTestContext) -> Option<DocumentRows> {
            self.tile.read_with(vcx, |t, _| t.sent.clone())
        }
    }

    /// `rows` with one CVI `param` moved: the upstream answered something
    /// other than what was sent, in exactly one row of the long form.
    fn one_param_moved(rows: &DocumentRows) -> DocumentRows {
        let mut echo = rows.clone();
        let (_, column) = echo
            .values
            .iter_mut()
            .find(|(name, _)| name == "param")
            .expect("CVI carries param");
        match column {
            geode_core::document::Column::F64(v) => v[0] += 0.5,
            other => panic!("param is F64, got {other:?}"),
        }
        echo
    }

    #[gpui::test]
    fn a_matching_echo_clears_the_draft_and_says_confirmed(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let (text, edited) = h.cell(&vcx, 0, 0);
        assert!(edited, "the premise: an edit on screen");
        let sent = h.upload_ok(&mut vcx);
        let at = h.sent_at(&vcx);

        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &sent, NEWER));

        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert!(draft.is_empty(), "{draft:?}");
        assert_eq!(draft.state, DraftState::Clean);
        assert!(h.sent_rows(&vcx).is_none(), "the echo consumed it");
        assert_eq!(
            h.painted_as_of(&vcx).as_deref(),
            Some(NEWER),
            "the panel follows the echo"
        );
        assert_eq!(
            h.cell(&vcx, 0, 0),
            (text, false),
            "the sent value, now the document's own"
        );
        let confirmed = format!("sent {at}, confirmed ");
        let has_confirmed = |h: &Harness, vcx: &gpui::VisualTestContext| {
            h.header_texts(vcx)
                .iter()
                .any(|t| t.starts_with(&confirmed))
        };
        assert!(has_confirmed(&h, &vcx), "{:?}", h.header_texts(&vcx));

        // A redelivery of the same generation (any publish anywhere bumps
        // the frame) does not take the line down; the next edit does.
        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &sent, NEWER));
        assert!(has_confirmed(&h, &vcx), "{:?}", h.header_texts(&vcx));
        h.motion(&mut vcx, "down", None);
        h.edit_one_cell(&mut vcx);
        assert!(!has_confirmed(&h, &vcx), "{:?}", h.header_texts(&vcx));
    }

    #[gpui::test]
    fn a_differing_echo_keeps_sent_and_counts_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);

        let echo = one_param_moved(&sent);
        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &echo, NEWER));

        let state = h.tile.read_with(&vcx, |t, _| t.draft().state.clone());
        assert!(matches!(state, DraftState::Sent { .. }), "{state:?}");
        assert!(
            h.header_texts(&vcx)
                .contains(&"echo differs (1 rows)".to_string()),
            "{:?}",
            h.header_texts(&vcx)
        );
        assert_eq!(
            h.painted_as_of(&vcx).as_deref(),
            Some(BASE),
            "the base stays painted under the edits"
        );
        assert!(h.cell(&vcx, 0, 0).1, "the edit is still painted");
        assert!(h.sent_rows(&vcx).is_some(), "kept for :rebase/:revert");

        // The same generation redelivered changes nothing.
        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &echo, NEWER));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_sent()));
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(BASE));
        assert!(
            h.header_texts(&vcx)
                .contains(&"echo differs (1 rows)".to_string()),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// A later generation that DOES match what was sent still confirms:
    /// the comparison is against `sent` every time a new generation
    /// arrives, not only the first.
    #[gpui::test]
    fn a_matching_echo_after_a_differing_one_still_confirms(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);
        h.echo(
            &mut vcx,
            test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), NEWER),
        );
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_sent()));
        let later = "2026-09-12T14:09:00Z";
        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &sent, later));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_empty()));
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(later));
        assert!(
            !h.header_texts(&vcx)
                .iter()
                .any(|t| t.starts_with("echo differs")),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// Redelivery of the draft's base while Sent does not trigger echo comparison.
    /// The base check uses source time and known generation IDs before inspecting
    /// submitted rows. Deliberately different contents prove the short-circuit:
    /// if compared, this fixture would report a differing echo.
    #[gpui::test]
    fn a_redelivery_of_the_base_while_sent_is_not_read_as_the_echo(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);

        // Same generation as `draft.base`, with content that would read as
        // differing if it were ever compared.
        h.echo(
            &mut vcx,
            test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), BASE),
        );

        assert!(
            h.tile.read_with(&vcx, |t, _| t.draft().is_sent()),
            "still Sent"
        );
        assert!(
            !h.header_texts(&vcx)
                .iter()
                .any(|t| t.starts_with("echo differs") || t.starts_with("echo not comparable")),
            "{:?}",
            h.header_texts(&vcx)
        );
        assert_eq!(
            h.painted_as_of(&vcx).as_deref(),
            Some(BASE),
            "still painting the base"
        );
        assert!(h.cell(&vcx, 0, 0).1, "the edit is still painted");
        assert!(h.sent_rows(&vcx).is_some(), "kept for the real echo later");
    }

    /// Auto policy governs Behind transitions. A Sent draft with a differing echo
    /// remains Sent under replace or rebase policy.
    #[gpui::test]
    fn the_update_policy_does_not_apply_to_a_sent_draft(cx: &mut gpui::TestAppContext) {
        for policy in ["replace", "rebase"] {
            let (h, mut vcx) = open_upload(cx);
            h.with_document(&mut vcx);
            h.command(&mut vcx, &format!("auto {policy}")).unwrap();
            h.edit_one_cell(&mut vcx);
            let sent = h.upload_ok(&mut vcx);
            h.echo(
                &mut vcx,
                test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), NEWER),
            );
            let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
            assert!(draft.is_sent(), "{policy}: {:?}", draft.state);
            assert_eq!(draft.len(), 1, "{policy}: the edit is kept");
            assert_eq!(
                draft.base.as_ref().map(|b| b.as_of.as_str()),
                Some(BASE),
                "{policy}: not moved"
            );
            assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(BASE), "{policy}");
        }
    }

    /// `:rebase` from `Sent` only has somewhere to go once a differing
    /// echo is held — this holds one first (`one_param_moved`), which is
    /// also the premise `rebase_from_sent_without_a_held_echo_is_refused`
    /// tests the absence of.
    #[gpui::test]
    fn rebase_from_sent_yields_editing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);
        h.echo(
            &mut vcx,
            test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), NEWER),
        );

        assert_eq!(h.command(&mut vcx, "rebase"), Ok(()));
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.base.as_ref().map(|b| b.as_of.as_str()), Some(NEWER));
        assert_eq!(draft.len(), 1, "the edit moved onto the echo");
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(NEWER));
        assert!(h.cell(&vcx, 0, 0).1, "painted as an edit again");
        assert!(h.sent_rows(&vcx).is_none());
        assert!(
            !h.header_texts(&vcx)
                .iter()
                .any(|t| t.starts_with("echo differs")),
            "{:?}",
            h.header_texts(&vcx)
        );
        assert_eq!(
            h.command(&mut vcx, "upload"),
            Ok(()),
            "an Editing draft may be sent again"
        );
    }

    /// Rebase refuses a Sent draft without a differing echo and is absent from
    /// completions. Rebasing onto the submitted generation would make the same edits
    /// uploadable again while the first request awaits its echo.
    #[gpui::test]
    fn rebase_from_sent_without_a_held_echo_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.upload_ok(&mut vcx);

        assert!(
            !h.tile
                .read_with(&vcx, |t, cx| t.completions("", 0, cx))
                .contains(&"rebase".to_string()),
            "no echo held: rebase is not offered"
        );
        assert_eq!(
            h.command(&mut vcx, "rebase"),
            Err(
                "nothing newer to rebase onto — the upload is awaiting its echo; :revert to drop it"
                    .to_string()
            )
        );
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert!(draft.is_sent(), "{:?}", draft.state);
        assert_eq!(
            draft.base.as_ref().map(|b| b.as_of.as_str()),
            Some(BASE),
            "unchanged"
        );
        assert_eq!(draft.len(), 1, "the edit is unchanged");
    }

    #[gpui::test]
    fn revert_from_sent_follows_the_echo(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);
        h.echo(
            &mut vcx,
            test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), NEWER),
        );

        assert_eq!(h.command(&mut vcx, "revert"), Ok(()));
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert_eq!(draft.state, DraftState::Clean);
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(NEWER));
        assert!(h.sent_rows(&vcx).is_none());
        assert!(
            !h.header_texts(&vcx)
                .iter()
                .any(|t| t.starts_with("echo differs")),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// Under a differing echo the panel paints the BASE while a newer
    /// generation is held — `Behind`'s own situation — so an edit there is
    /// refused the same way: `:rebase` or `:revert` first.
    #[gpui::test]
    fn an_edit_under_a_differing_echo_is_refused(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);
        h.echo(
            &mut vcx,
            test_fixtures::snapshot_of_at(&CVI, &one_param_moved(&sent), NEWER),
        );
        h.dispatch(&mut vcx, "edit", None);
        assert!(h.editor_value(&vcx).is_none(), "no editor opened");
        assert_eq!(
            h.tile
                .read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(ECHO_REFUSED.to_string())
        );
        assert_eq!(h.command(&mut vcx, "bump 1"), Err(ECHO_REFUSED.to_string()));
        assert!(h.tile.read_with(&vcx, |t, _| t.draft().is_sent()));
    }

    /// An edit made while the upload was in flight keeps the draft
    /// `Editing` on `Ok` — and the rows kept for an echo go with it: no
    /// `Sent` draft will ever compare against them.
    #[gpui::test]
    fn an_edit_in_flight_drops_the_kept_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.command(&mut vcx, "upload").unwrap();
        draw(&mut vcx);
        type_keys(&mut vcx, "y");
        let tag = h.upload_request().unwrap().tag;
        assert!(h.sent_rows(&vcx).is_some(), "kept while in flight");
        h.motion(&mut vcx, "down", None);
        h.edit_one_cell(&mut vcx);
        h.deliver_upload(&mut vcx, tag, Ok(()));
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing
        );
        assert!(h.sent_rows(&vcx).is_none());
    }

    /// A further edit takes a `Sent` draft back to `Editing` (the draft's
    /// own rule) and the sent rows with it.
    #[gpui::test]
    fn an_edit_after_sent_drops_the_kept_rows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.upload_ok(&mut vcx);
        h.motion(&mut vcx, "down", None);
        h.edit_one_cell(&mut vcx);
        assert_eq!(
            h.tile.read_with(&vcx, |t, _| t.draft().state.clone()),
            DraftState::Editing
        );
        assert!(h.sent_rows(&vcx).is_none());
    }

    /// The minted id is Geode's, not the wire's: an inserted `new-1` goes
    /// out under that label (the kind writes no id) and comes back under
    /// the upstream's own date-derived id — still a match.
    #[gpui::test]
    fn a_dividend_echo_matches_despite_reminted_labels(cx: &mut gpui::TestAppContext) {
        let restored: toml::Table = format!(
            r#"
key = ["SPX.Z"]
[draft]
base = "{BASE}"
edits = []
[draft.rows.new-1]
after = "D2"
cells = {{ ex = {{ type = "date", value = "2027-06-18" }}, amount = 0.75, status = {{ type = "text", value = "estimated" }} }}
"#
        )
        .parse()
        .unwrap();
        let (h, mut vcx) = open_spec_with_egress(
            cx,
            &test_fixtures::SCHEDULE,
            Some(restored),
            vec![("sophis".into(), vec!["div_schedule".into()])],
        );
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
        let sent = h.upload_ok(&mut vcx);
        let (_, ids) = &sent.axes[0];
        assert_eq!(
            ids,
            &geode_core::document::Column::Utf8(vec!["D1".into(), "D2".into(), "new-1".into()]),
            "the premise: the painted label went out"
        );

        h.echo(
            &mut vcx,
            test_fixtures::schedule_snapshot_at(
                &[
                    ("D1", "2026-12-18", 1.25, "declared"),
                    ("D2", "2027-03-19", 0.5, "estimated"),
                    ("2027-06-18#1", "2027-06-18", 0.75, "estimated"),
                ],
                NEWER,
            ),
        );
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert_eq!(draft.state, DraftState::Clean, "{draft:?}");
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(NEWER));
        assert!(
            h.header_texts(&vcx)
                .iter()
                .any(|t| t.contains(", confirmed ")),
            "{:?}",
            h.header_texts(&vcx)
        );
    }

    /// Sessions persist Sent edits as ordinary draft edits. Restore returns them to
    /// Editing, where they can be uploaded again.
    #[gpui::test]
    fn a_sent_draft_restores_as_editing(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        h.upload_ok(&mut vcx);
        let written = h.tile.read_with(&vcx, |t, cx| t.serialize(cx));

        let (h, mut vcx) = open_spec_with_egress(
            cx,
            &CVI,
            Some(written),
            vec![("sophis".into(), vec!["cvi_params".into()])],
        );
        h.visible(&mut vcx, true);
        let tag = h.document_request().unwrap().tag;
        h.deliver(&mut vcx, tag, Arc::new(cvi(BASE)));
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert_eq!(draft.state, DraftState::Editing);
        assert_eq!(draft.len(), 1);
        assert_eq!(h.command(&mut vcx, "upload"), Ok(()), "sendable again");
    }

    /// Defensive: a `Sent` draft with no rows to compare against (no
    /// production route leaves one — `sent` is dropped only once the draft
    /// has left `Sent`) must not claim a confirmation it cannot check; it
    /// goes `Behind`, the ordinary disclosure of a newer generation.
    #[gpui::test]
    fn a_sent_draft_with_nothing_to_compare_goes_behind(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx) = open_upload(cx);
        h.with_document(&mut vcx);
        h.edit_one_cell(&mut vcx);
        let sent = h.upload_ok(&mut vcx);
        h.tile.update(&mut vcx, |t, _| t.sent = None);
        h.echo(&mut vcx, test_fixtures::snapshot_of_at(&CVI, &sent, NEWER));
        let draft = h.tile.read_with(&vcx, |t, _| t.draft().clone());
        assert!(draft.is_behind(), "{:?}", draft.state);
        assert_eq!(h.painted_as_of(&vcx).as_deref(), Some(BASE));
    }

    // ---- the confirm's end hands the keyboard back (shell-hosted) ------

    /// Forwards every trait method to the real factory and keeps the tile
    /// it creates — the one way a shell-hosted test can read the panel it
    /// is typing at (the shell hands back only `&dyn ModuleFactory`).
    struct Capturing {
        inner: MarketDataFactory,
        tile: Rc<RefCell<Option<Entity<MarketDataTile>>>>,
    }

    impl ModuleFactory for Capturing {
        fn kind(&self) -> &'static str {
            self.inner.kind()
        }
        fn register_actions(&self, registry: &mut geode_shell::actions::ActionRegistry) {
            self.inner.register_actions(registry)
        }
        fn contexts(&self) -> Vec<&'static str> {
            self.inner.contexts()
        }
        fn default_keymap(&self) -> Option<&'static str> {
            self.inner.default_keymap()
        }
        fn create(
            &self,
            tile: TileId,
            restored: Option<&toml::Table>,
            frame: FrameRef,
            diagnostics: Entity<Diagnostics>,
            window: &mut Window,
            cx: &mut gpui::App,
        ) -> geode_shell::module::TileOccupant {
            let occupant = self
                .inner
                .create(tile, restored, frame, diagnostics, window, cx);
            *self.tile.borrow_mut() = occupant.view.clone().downcast::<MarketDataTile>().ok();
            occupant
        }
    }

    /// The real shell over one CVI panel (tile 1) restored with a
    /// one-cell draft and one egress target, its keymap built from the
    /// shell's own builtin layer and the panel's own fragment exactly as
    /// `main.rs` splices them — so every key below travels the production
    /// route: gpui's dispatch, the shell root's listener, the keymap, the
    /// tile's `dispatch`/command line.
    fn open_in_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::VisualTestContext,
        Entity<geode_shell::shell::ShellView>,
        Entity<MarketDataTile>,
        Receiver<Request>,
    ) {
        use geode_core::config::{ConfigSources, LayerDoc};
        use geode_shell::defaults::{
            BUILTIN_KEYMAP, default_mod, register_add_actions, register_builtin_actions,
        };
        use geode_shell::shell::{ShellServices, ShellView};

        cx.update(gpui_component::init);
        cx.update(geode_shell::shell::dialog::init_reclaimed_keybindings);
        cx.update(crate::init);
        let (data, rx) = DataHandle::for_tests();
        let captured = Rc::new(RefCell::new(None));
        let factory = Capturing {
            inner: MarketDataFactory::new(data, Arc::clone(&CVI), Duration::from_secs(15 * 60))
                .with_egress(Arc::new(vec![(
                    "sophis".to_string(),
                    vec!["cvi_params".to_string()],
                )])),
            tile: captured.clone(),
        };
        let (config, builtin) = ShellServices::config_and_builtin(ConfigSources::default());
        let mut registry = geode_shell::actions::ActionRegistry::default();
        register_builtin_actions(&mut registry);
        register_add_actions(&mut registry, &["cvi"]);
        let mut roster = geode_shell::module::ModuleRoster::new();
        roster.add(Box::new(factory));
        roster.register_actions(&mut registry);
        let (fragments, fragment_diags) = roster.keymap_fragments();
        assert!(fragment_diags.is_empty(), "{fragment_diags:?}");
        let spliced = geode_shell::keymap::fragments::splice(
            &[LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()],
            &fragments,
        );
        let (keymap, diags) = geode_shell::keymap::build_keymap(&spliced, default_mod(), &registry);
        assert!(diags.is_empty(), "{diags:?}");
        let (theme, warnings) = geode_shell::theme::load_bundled();
        assert!(warnings.is_empty(), "{warnings:?}");

        let mut session = geode_shell::session::to_toml(
            &geode_shell::tiling::Workspaces::new(),
            &geode_shell::session::TileRecords::new(),
            None,
            &geode_shell::session::PinnedRecords::new(),
            &geode_shell::palette_usage::PaletteUsage::new(),
            &geode_shell::session::PageRecords::new(),
        );
        let ws1: toml::Table = format!(
            r#"
focused = 1
[node]
kind = "leaf"
id = 1
[tiles.1]
module = "{}"
[tiles.1.state]
underlying = ["SPX.Z"]
[tiles.1.state.drafts."SPX.Z"]
base = "{BASE}"
edits = [["2026-11-20", "-1", 9.5]]
"#,
            CVI.kind
        )
        .parse()
        .unwrap();
        if let Some(toml::Value::Table(ws)) = session.get_mut("workspaces") {
            ws.insert("1".to_string(), toml::Value::Table(ws1));
        }
        let restored = geode_shell::session::from_toml(&session).unwrap();
        assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);

        let services = ShellServices {
            config,
            builtin,
            registry,
            keymap,
            mod_alias: default_mod(),
            workspaces: restored.workspaces,
            theme,
            session_path: None,
            roster,
            restored_tiles: restored.tiles,
            restored_frame: None,
            restored_pinned: Default::default(),
            restored_palette_usage: geode_shell::palette_usage::PaletteUsage::new(),
            log: None,
            action_tail: Arc::new(std::sync::Mutex::new(
                geode_shell::diagnostics::ActionTail::new(),
            )),
            keymap_diagnostics: Vec::new(),
            keymap_fragments: fragments,
            keymap_fragment_diagnostics: Vec::new(),
            composition_diagnostics: Vec::new(),
            pages: geode_shell::module::PageRoster::new(),
            restored_pages: std::collections::BTreeMap::new(),
        };
        let shell_slot = Rc::new(RefCell::new(None));
        let window = cx
            .update(|cx| {
                let shell_slot = shell_slot.clone();
                cx.open_window(gpui::WindowOptions::default(), |window, cx| {
                    let view = cx.new(|cx| ShellView::new(services, None, None, window, cx));
                    *shell_slot.borrow_mut() = Some(view.clone());
                    cx.new(|cx| gpui_component::Root::new(view, window, cx))
                })
            })
            .unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        // gpui diffs focus paths only for an ACTIVE window.
        vcx.update(|window, _| window.activate_window());
        draw(&mut vcx);
        vcx.run_until_parked();
        let shell = shell_slot.borrow_mut().take().expect("the shell view");
        let tile = captured
            .borrow()
            .clone()
            .expect("the shell created the panel");
        (vcx, shell, tile, rx)
    }

    #[gpui::test]
    fn edit_keys_place_the_caret_at_the_requested_end(cx: &mut gpui::TestAppContext) {
        let (mut vcx, shell, tile, _rx) = open_in_shell(cx);
        let tag = tile.read_with(&vcx, |t, _| t.following.tag());
        let outcome = QueryOutcome {
            key: QueryKey(1),
            tag,
            snapshot: Ok(Arc::new(cvi(BASE))),
            submitted: Instant::now(),
        };
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.deliver(Delivery::Query(outcome), window, cx))
        });
        draw(&mut vcx);
        for selection in ["", "v", "shift-v"] {
            // Row selections exclude the leading slice columns.
            type_keys(&mut vcx, "home 3 l");
            if !selection.is_empty() {
                type_keys(&mut vcx, selection);
            }
            for (key, at_start) in [("shift-i", true), ("i", false), ("enter", false)] {
                type_keys(&mut vcx, key);
                let input = tile.read_with(&vcx, |t, _| match &t.editor {
                    Some(Editing {
                        state: EditorState::Text(input),
                        ..
                    }) => input.clone(),
                    _ => panic!("{selection} {key}: expected a text editor"),
                });
                let before = input.read_with(&vcx, |s, _| s.value().to_string());
                assert!(!before.is_empty());
                assert_eq!(
                    input.read_with(&vcx, |s, _| s.cursor()),
                    if at_start { 0 } else { before.len() },
                    "{selection} {key}"
                );
                vcx.simulate_input("7");
                assert_eq!(
                    input.read_with(&vcx, |s, _| s.value().to_string()),
                    if at_start {
                        format!("7{before}")
                    } else {
                        format!("{before}7")
                    },
                    "typing must insert without replacing the cell text"
                );
                type_keys(&mut vcx, "escape");
            }
            if !selection.is_empty() {
                type_keys(&mut vcx, "escape");
            }
        }
    }

    /// Every way the upload confirm ends — `y`, a cancelling key — blurs
    /// its prompt, leaving NO element focused. The tile must still answer
    /// the very next keystroke with no click in between: `j` moves the
    /// cursor through the shell's own key route.
    #[gpui::test]
    fn the_tile_answers_keys_after_the_upload_confirm_ends(cx: &mut gpui::TestAppContext) {
        for answer in ["y", "n", "escape"] {
            let (mut vcx, shell, tile, rx) = open_in_shell(cx);
            // The panel's LATEST question: the shell's first frames can
            // ask more than once (visibility, then the frame settling).
            let mut asked = None;
            while let Ok(request) = rx.try_recv() {
                if let Request::Document(params) = request {
                    asked = Some(params.tag);
                }
            }
            let tag = asked.expect("the visible panel asked for its document");
            assert_eq!(
                tag,
                tile.read_with(&vcx, |t, _| t.following.tag()),
                "{answer}"
            );
            let outcome = QueryOutcome {
                // The session's own tile id, not the harness's `TILE`.
                key: QueryKey(1),
                tag,
                snapshot: Ok(Arc::new(cvi(BASE))),
                submitted: Instant::now(),
            };
            vcx.update(|window, cx| {
                shell.update(cx, |s, cx| s.deliver(Delivery::Query(outcome), window, cx))
            });
            draw(&mut vcx);
            assert_eq!(
                tile.read_with(&vcx, |t, _| t.draft().len()),
                1,
                "{answer}: the premise, a restored one-cell draft"
            );

            type_keys(&mut vcx, ":");
            vcx.simulate_input("upload");
            type_keys(&mut vcx, "enter");
            assert!(
                tile.read_with(&vcx, |t, _| t.upload_prompt().is_some()),
                "{answer}: :upload armed the confirm; notice {:?}, header {:?}",
                tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)),
                tile.read_with(&vcx, |t, _| t.header_texts()),
            );

            type_keys(&mut vcx, answer);
            assert!(
                tile.read_with(&vcx, |t, _| t.upload_prompt().is_none()),
                "{answer}: the confirm ended"
            );
            vcx.run_until_parked();
            draw(&mut vcx);

            let row = |vcx: &gpui::VisualTestContext| {
                tile.read_with(vcx, |t, _| match t.cursor() {
                    Cursor::Cell { row, .. } => row,
                    Cursor::Attr(_) => usize::MAX,
                })
            };
            let before = row(&vcx);
            type_keys(&mut vcx, "j");
            assert_eq!(
                row(&vcx),
                before + 1,
                "{answer}: j reached the tile with no click after the confirm"
            );
        }
    }

    /// A press on the upload prompt itself answers "no" and takes no focus
    /// (its own press-to-focus would hand the keyboard back to a question
    /// that is gone). The keyboard returns to the tile through the shell's
    /// restoration path: the very next `j` moves the cursor.
    #[gpui::test]
    fn the_tile_answers_keys_after_a_press_on_the_upload_prompt(cx: &mut gpui::TestAppContext) {
        let (mut vcx, shell, tile, rx) = open_in_shell(cx);
        let mut asked = None;
        while let Ok(request) = rx.try_recv() {
            if let Request::Document(params) = request {
                asked = Some(params.tag);
            }
        }
        let tag = asked.expect("the visible panel asked for its document");
        let outcome = QueryOutcome {
            key: QueryKey(1),
            tag,
            snapshot: Ok(Arc::new(cvi(BASE))),
            submitted: Instant::now(),
        };
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.deliver(Delivery::Query(outcome), window, cx))
        });
        draw(&mut vcx);
        type_keys(&mut vcx, ":");
        vcx.simulate_input("upload");
        type_keys(&mut vcx, "enter");
        draw(&mut vcx);
        assert!(
            tile.read_with(&vcx, |t, _| t.upload_prompt().is_some()),
            "fixture: :upload armed the confirm"
        );

        let at = vcx
            .debug_bounds("marketdata-upload-confirm-1")
            .expect("the prompt is painted")
            .center();
        vcx.simulate_click(at, gpui::Modifiers::default());
        vcx.run_until_parked();
        draw(&mut vcx);
        assert!(
            tile.read_with(&vcx, |t, _| t.upload_prompt().is_none()),
            "the press cancelled the confirm"
        );
        assert_eq!(
            tile.read_with(&vcx, |t, _| t.notice().map(str::to_string)),
            Some(UPLOAD_CANCELLED.into()),
            "a press is a no, not a y"
        );

        let row = |vcx: &gpui::VisualTestContext| {
            tile.read_with(vcx, |t, _| match t.cursor() {
                Cursor::Cell { row, .. } => row,
                Cursor::Attr(_) => usize::MAX,
            })
        };
        let before = row(&vcx);
        type_keys(&mut vcx, "j");
        assert_eq!(
            row(&vcx),
            before + 1,
            "j reached the tile with no other click after the prompt press"
        );
    }

    /// The shared motion keys reach the panel through the shell's builtin
    /// bindings: `down` moves (the fragment never bound the arrows), a
    /// counted `G` is that row, and `k` on row 0 still enters the strip.
    #[gpui::test]
    fn the_shared_motion_keys_move_the_panel_through_the_shell(cx: &mut gpui::TestAppContext) {
        let (mut vcx, shell, tile, rx) = open_in_shell(cx);
        let mut asked = None;
        while let Ok(request) = rx.try_recv() {
            if let Request::Document(params) = request {
                asked = Some(params.tag);
            }
        }
        let tag = asked.expect("the visible panel asked for its document");
        let outcome = QueryOutcome {
            key: QueryKey(1),
            tag,
            snapshot: Ok(Arc::new(cvi(BASE))),
            submitted: Instant::now(),
        };
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.deliver(Delivery::Query(outcome), window, cx))
        });
        draw(&mut vcx);
        let cursor = |vcx: &gpui::VisualTestContext| tile.read_with(vcx, |t, _| t.cursor());
        type_keys(&mut vcx, "g g");
        assert!(matches!(cursor(&vcx), Cursor::Cell { row: 0, .. }));
        type_keys(&mut vcx, "down");
        assert!(
            matches!(cursor(&vcx), Cursor::Cell { row: 1, .. }),
            "the arrow moves"
        );
        type_keys(&mut vcx, "1 shift-g");
        assert!(
            matches!(cursor(&vcx), Cursor::Cell { row: 0, .. }),
            "1G is row 1"
        );
        type_keys(&mut vcx, "k");
        assert!(
            matches!(cursor(&vcx), Cursor::Attr(_)),
            "k on row 0 enters the strip"
        );
        type_keys(&mut vcx, "shift-g");
        assert!(
            matches!(cursor(&vcx), Cursor::Cell { row: 1, .. }),
            "G leaves the strip"
        );
    }

    mod selection;
}
