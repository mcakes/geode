//! The config dialogs' shared scaffold: browse a config domain's named
//! objects, and edit one
//! (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md`).
//!
//! Phase 4c replaced an earlier design that would have put a TOML text
//! editor in a tile. Instead every config domain — views, groupings,
//! scopes, sources, the read-only schema — gets a purpose-built dialog
//! on this one scaffold, so a trader edits *objects with fields* rather
//! than text, and so the layer a change lands in is a property of the
//! scaffold rather than a thing each dialog remembers to get right.
//! Both stages were first built over one adapter, [`Domain::Views`] — the
//! hardest shape first, deliberately (spec §14), so the vocabulary was
//! settled before the thinner adapters depended on it. Views is the only
//! domain whose fields split across two destinations, and
//! [`Destination`] is what makes that split mechanical rather than a
//! special case inside one adapter.
//!
//! [`Domain::Groupings`] is the first of those thinner adapters, and it
//! needed one more thing from the scaffold that Views alone never
//! exercised: its object's own value is a bare array (`3 = ["book",
//! "lhu"]`), not a table, so [`Domain::to_table`] renders a
//! `toml_edit::Item` rather than a `toml_edit::Table` — see
//! `groupings.rs`'s module doc for the full story.
//!
//! ## Layout
//!
//! Split the way `keybindings_view` and `picker` are, one directory
//! further out because a domain adapter is a file of its own (spec §4):
//!
//! - this module — the pure core: [`Stage`], [`ObjectDialogState`],
//!   [`Draft`] and its field vocabulary ([`Field`], [`FieldKind`],
//!   [`ListItem`], [`Destination`]), [`ObjectRow`], [`Domain`], and the
//!   one derivation of `layer` and `overridden` every domain shares;
//! - [`views`] — the `Domain::Views` adapter: the doc it reads, the
//!   one-line summary a view row shows, the fields a view has and which
//!   file each one is written to, and nothing else;
//! - [`groupings`] — the `Domain::Groupings` adapter: the nine grouping
//!   slots, one dimension chain each;
//! - [`render`] — the gpui shell: `open`, the [`dialog::ModalKeyHandler`]
//!   both stages come through, and the painted list, fields and action
//!   bar.
//!
//! No `gpui` type appears in this file, so every transition and every
//! marker below is unit-testable without a window — the same split, for
//! the same reason, that keeps `KeybindingsState` free of the scroll
//! handle that sits beside it on `ShellView`.
//!
//! ## Browse rows are derived; the edit stage paints its draft
//!
//! [`Domain::objects`] runs fresh on every render and every keystroke.
//! That is the contract `keybindings_view::derive_rows` and
//! `settings_view::derive_rows` both hold, and the reason those dialogs
//! cannot show a stale value: a config reload lands in
//! `ShellView::services.config` with no notification to any dialog, so
//! anything cached here would be wrong from the next 500 ms watcher tick
//! onward.
//!
//! The **edit stage** is the deliberate exception: its rows come from the
//! stored [`Draft`], not from `Config`. That is not a cache, and it is
//! what makes a config edit instant. A keystroke records its change on a
//! pending batch that is merged and applied 250 ms later ([`apply`]), so
//! `services.config` is knowingly up to one debounce behind — a row
//! derived from it would show the trader their own keystroke a quarter of
//! a second late, which is the lag this whole design exists to remove.
//! The draft is the same value the flush is about to merge, rendered
//! through the same [`Domain::to_table`], and a failed write rebuilds it
//! from the reverted config, so the two cannot drift apart.

pub mod apply;
mod colours;
mod dataset_columns;
mod groupings;
pub mod render;
mod schema;
mod scopes;
mod sources;
mod views;

/// `ShellView::deliver_distinct` routes a `SCOPES_KEY` outcome here — the
/// one door onto the Values stage's own delivery, kept `pub(in crate::
/// shell)` rather than fully `pub` like [`render::open`], since nothing
/// outside this crate's shell needs it.
pub(in crate::shell) use render::deliver_values;

use std::collections::{BTreeMap, BTreeSet};

use geode_core::config::{Config, Diagnostic, Layer, Severity};
use geode_core::view::ColumnPresentation;

use crate::dialogmode::DialogMode;

/// The one notice every mutating verb on a [`Domain::writable`] `false`
/// domain shows — `render.rs`'s browse `n` gate, `handle_edit_key`'s
/// verb gate, and `render::edit_commit_notice` all read this same
/// string, so a trader sees one consistent answer wherever they reach
/// for a key the schema inspector cannot honour.
pub const READ_ONLY_NOTICE: &str =
    "the datasets doc is read-only — open a column (enter) to set how it paints";

/// Which config domain a dialog is browsing. One variant per adapter
/// module under this directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Views,
    Groupings,
    Scopes,
    /// Read-only (§9, §19.4): the datasets the other adapters build their
    /// choices from.
    Schema,
    /// The ingest feeds, one object per source (§8.3, §19.3).
    Sources,
    /// The shared colour vocabulary a column's `colour` field and a
    /// chart series can name — one object per named colour, a hue (with
    /// its tone) or a theme token (Part 2c §6.1).
    Colours,
}

/// Which stage of the scaffold is on screen.
///
/// `Edit` is what makes the `escape` ladder's
/// [`crate::dialogmode::EscapeStep::PreviousStage`] rung reachable — this
/// dialog is the design's first consumer of that rung, and
/// [`ObjectDialogState::has_previous_stage`] is written against this enum
/// rather than against a literal `false` so constructing the variant is
/// all it took to turn the rung on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Browse,
    /// The browse list with the filter row replaced by a *name* field
    /// (§18.2): `n` enters it, `enter` creates, `escape` returns to
    /// `Browse` with nothing written. A stage rather than a flag on
    /// `Browse` so `has_previous_stage` turns the escape ladder's third
    /// rung on by construction, the same way `Edit` did.
    Naming,
    /// Editing one object's fields. The draft itself lives in
    /// [`ObjectDialogState::draft`] rather than in here, because the
    /// *name* is what identifies the stage (it is what `escape` restores
    /// the browse selection to) while the draft is mutable state that a
    /// `PartialEq` stage comparison has no business walking.
    Edit {
        object: String,
    },
    /// Editing one column's presentation — a **projection** over the same
    /// [`Draft`] (Part 2c §5.2), not a draft of its own: `enter` on a
    /// member row stashes the view's fields and installs the column's
    /// seven, every change folds back onto the item
    /// ([`Draft::fold_column`]), and `escape` restores the view with the
    /// cursor on the column.
    ///
    /// It carries both names for the same reason [`Stage::Edit`] carries
    /// one: the pair is what `escape` steps back through and what the
    /// crumb reads (`tree › npv`). Everything mutable about the column
    /// lives on the draft, as it does for `Edit`.
    Column {
        object: String,
        column: String,
    },
    /// Ticking one dimension's values for a saved scope (scopes-editing
    /// spec §4) — a projection over the same [`Draft`] in
    /// [`Stage::Column`]'s mould: `enter` on a `dimensions` row stashes
    /// the scope's fields and installs one list of the column's distinct
    /// values; `escape` restores the scope with the cursor on the column.
    Values {
        object: String,
        column: String,
    },
}

/// What `enter` in [`Stage::Naming`] creates (scopes-editing spec §6):
/// the domain's empty object (`n`), or a verbatim copy of a named one
/// (`c`). Recorded by NAME when armed — the browse cursor is an index,
/// and a reload can re-rank the list under it (the same reason
/// `confirm_target` records one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSeed {
    Empty,
    CopyOf(String),
}

/// One named object as the browse list shows it.
///
/// `layer`, `overridden` and `drifted` are the three provenance markers
/// spec §5 defines; they exist because whole-object override is invisible
/// otherwise — a user who overrode a desk view sees their copy and no
/// hint that a desk version exists underneath it, nor that it has since
/// moved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRow {
    pub name: String,
    /// A one-line description of the object, from its own adapter — the
    /// second, muted line of the row, and half of what the filter
    /// matches against.
    pub summary: String,
    /// The layer whose copy takes effect, or `None` when no layer defines
    /// the object at all and it is on the list only because the domain's
    /// roster names it (`Domain::roster` — a grouping slot nothing has
    /// filled). `Option` rather than a `configured: bool` beside a
    /// placeholder `Layer`: every reader of this field decides something
    /// destructive or forking on it (`arm_delete`, `would_fork`), and a
    /// placeholder value would answer those questions with a lie.
    pub layer: Option<Layer>,
    /// The user layer defines this object **and so does an earlier
    /// layer**. Both halves matter: the edit stage offers `Revert to
    /// desk` on an overridden row, and revert deletes the user's copy —
    /// on a view only the user layer defines, that would delete the view
    /// outright rather than restore anything. See [`derive_rows`].
    pub overridden: bool,
    /// The layer this row overrides has changed since it was forked
    /// (spec §5.2, §19.6). `overrides.toml` (`OVERRIDES_DOC`) records the
    /// shadowed layer's canonical text at fork time, keyed by
    /// [`override_key`]; `derive_rows` compares that text against the
    /// shadow's CURRENT text. `false` whenever there is no recorded
    /// entry — an override that predates this sidecar, or a stale entry
    /// (see [`stale_override_keys`]) — never a guess made by comparing
    /// the user's copy against the desk's current one, which would mark
    /// every deliberate customisation as drifted (exactly backwards).
    pub drifted: bool,
    /// A grouping key painted before the name, dimmed (§19.3): the
    /// dataset a source feeds. `Some` only on Sources; the primary sort
    /// key when present, part of `searchable_text`, never the identity —
    /// the doc key is still `name`, so a dataset with two sources is two
    /// rows and every click handler and selector stays keyed by `name`.
    pub prefix: Option<String>,
}

impl ObjectRow {
    /// What the browse row paints as its label (§19.3): `"<prefix> ·
    /// <name>"` for a prefixed row, the bare name otherwise. The one
    /// spelling of that join, shared by the painted label
    /// (`render.rs`'s browse painter) and [`searchable_text`], so a hit
    /// inside the prefix ranks and highlights against the exact text on
    /// screen.
    pub fn display_name(&self) -> String {
        match &self.prefix {
            Some(p) => format!("{p} · {}", self.name),
            None => self.name.clone(),
        }
    }
}

impl Domain {
    /// The config doc this domain's objects live in — the file stem, as
    /// `Config::layered_docs` keys them. The one spelling of that fact:
    /// [`objects`](Self::objects) reads the doc name from here rather
    /// than naming the adapter's own constant a second time, so the two
    /// cannot drift as adapters are added.
    pub fn doc(self) -> &'static str {
        match self {
            Domain::Views => views::DOC,
            Domain::Groupings => groupings::DOC,
            Domain::Scopes => scopes::DOC,
            Domain::Schema => schema::DOC,
            Domain::Sources => sources::DOC,
            Domain::Colours => colours::DOC,
        }
    }

    /// The dialog's title, and the word the footer uses for one object.
    pub fn title(self) -> &'static str {
        match self {
            Domain::Views => "Views",
            Domain::Groupings => "Groupings",
            Domain::Scopes => "Scopes",
            Domain::Schema => "Schema",
            Domain::Sources => "Sources",
            Domain::Colours => "Colours",
        }
    }

    /// The plural noun the browse crumb counts (`2 views`, `9 slots`,
    /// `3 saved`) — not `title()`, whose "Groupings" would count the
    /// wrong thing.
    pub fn crumb_noun(self) -> &'static str {
        match self {
            Domain::Views => "views",
            Domain::Groupings => "slots",
            Domain::Scopes => "saved",
            Domain::Schema => "datasets",
            Domain::Sources => "sources",
            Domain::Colours => "colours",
        }
    }

    /// How this domain describes one object on its browse row — the only
    /// genuinely domain-specific part of a row, and therefore the only
    /// part an adapter gets to supply.
    fn summary_fn(self) -> fn(&toml::Value) -> String {
        match self {
            Domain::Views => views::summary,
            Domain::Groupings => groupings::summary,
            Domain::Scopes => scopes::summary,
            Domain::Schema => schema::summary,
            Domain::Sources => sources::summary,
            Domain::Colours => colours::summary,
        }
    }

    /// The presentation doc this domain's objects can be personalised
    /// through without forking (spec §4.1), if it has one at all.
    ///
    /// `None` for a domain none of whose fields carry
    /// [`Destination::Presentation`] — Groupings, whose `dimensions` is
    /// entirely [`Destination::Doc`] (`groupings.rs`'s module doc has the
    /// reasoning) — so [`derive_rows`]'s "personalised without
    /// overriding" check, and `render::run_confirmed`'s removal list,
    /// both have either a real doc name to look for or nothing to look
    /// for, rather than a name that could never correspond to a file. Also
    /// `None` for Schema, which has no user-facing overlay of any kind —
    /// [`Domain::writable`] is `false` for it, so there is nothing to
    /// personalise without forking in the first place.
    fn presentation_doc(self) -> Option<&'static str> {
        match self {
            Domain::Views => Some(views::PRESENTATION_DOC),
            // Scopes and Sources have no presentation doc for the same
            // reason Groupings does not: every field either domain has is
            // `Destination::Doc` (`scopes.rs`'s and `sources.rs`'s own
            // module docs). Schema joins them for the reason this
            // method's own doc comment gives. Colours joins for the same
            // "every field is `Destination::Doc`" reason (`colours.rs`'s
            // own module doc) — there is nothing to personalise about a
            // shared colour without forking it.
            Domain::Groupings
            | Domain::Scopes
            | Domain::Schema
            | Domain::Sources
            | Domain::Colours => None,
        }
    }

    /// The names a domain's browse list always shows, configured or not.
    /// `Some` only for Groupings (§18.4): the nine `ctrl+1..9` slots are
    /// a fixed keyboard, so an unfilled slot is a row that reads `empty`
    /// rather than a row that does not exist — and there is no `n`,
    /// because nothing can be created that is not already on the list.
    /// Slot `0` is not here: `ctrl+0` is `frame::slot_clear`, the view's
    /// own grouping, and `GroupingSlots` is nine wide (user ruling
    /// 2026-09-10).
    ///
    /// A separate question from [`Domain::writable`], which follows right
    /// below: this is "can anything be created that is not already
    /// listed", not "can this domain be written to at all" — Schema
    /// answers `None` here (nothing fixes its list; it simply lists
    /// whatever `datasets.toml` declares) and `false` to `writable`.
    pub(super) fn roster(self) -> Option<&'static [&'static str]> {
        match self {
            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
            Domain::Views | Domain::Scopes | Domain::Schema | Domain::Sources | Domain::Colours => {
                None
            }
        }
    }

    /// `false` for [`Domain::Schema`] outside its column stage (§19.4,
    /// dataset-presentation spec §4.2): the create gate, the footer hints
    /// and every mutating verb — `space`, `shift+space`, `i`, `d`, `r`,
    /// `x`, `n`, `o`, `shift+j`/`shift+k`, a tick click, a drop — read
    /// this, so a read-only surface refuses in one place rather than by
    /// each verb forgetting. Schema's ONE writable surface is
    /// [`Stage::Column`], whose fields write the dataset overlay, never
    /// the datasets doc. The Groupings roster gate
    /// (`roster().is_some()`) is a separate question ("can anything be
    /// created that is not already listed") and stays beside it.
    pub fn writable(self, stage: &Stage) -> bool {
        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. })
    }

    /// May `c` copy an object under a new name? Scopes alone for now
    /// (scopes-editing spec §6); the mechanism is generic.
    pub fn duplicable(self) -> bool {
        self == Domain::Scopes
    }

    /// The text painted before an object's name, if this domain groups
    /// its objects (§19.3). `None` on every domain but Sources — a
    /// source's row leads with the dataset it feeds
    /// (`sources::prefix`), painted dimmed ahead of the name and used as
    /// the primary sort key in [`derive_rows`]; every other domain's
    /// objects are already uniquely named with nothing to group them by.
    fn prefix_fn(self) -> Option<fn(&toml::Value) -> Option<String>> {
        match self {
            Domain::Sources => Some(sources::prefix),
            Domain::Views
            | Domain::Groupings
            | Domain::Scopes
            | Domain::Schema
            | Domain::Colours => None,
        }
    }

    /// Every named object in this domain, with its provenance markers.
    ///
    /// **Deliberately not a `match`.** The `layer`/`overridden`
    /// derivation is the dangerous computation on this whole surface
    /// (see [`ObjectRow::overridden`]: a wrong `overridden` makes the
    /// edit stage offer a destructive `Revert to desk`), so every domain
    /// must share the one tested walk. A per-domain `match` here would
    /// merely *discourage* an adapter from doing its own walk and
    /// diverging; an unconditional call makes that unrepresentable — the
    /// only things a domain decides are its doc name, its summary line,
    /// its optional row prefix and (optionally) its presentation doc, and
    /// all four arrive through the small matches above. Part 2 adds three
    /// more adapters onto this exact seam, which is why the hole stays
    /// closed as they arrive.
    pub fn objects(self, config: &Config) -> Vec<ObjectRow> {
        derive_rows(
            config,
            self.doc(),
            self.presentation_doc(),
            self.roster(),
            self.summary_fn(),
            self.prefix_fn(),
        )
    }

    /// Is `name` already spoken for in this domain (spec §18.2's "a name
    /// any layer already holds is refused")?
    ///
    /// **Wider than [`Domain::objects`] on purpose, and that is the
    /// whole reason it exists.** The rows are what the browse list can
    /// show — `doc`'s layered keys plus the roster — but a name can also
    /// be held by the user's presentation overlay alone
    /// ([`personalised_names`]): the desk dropped a view the trader had
    /// hidden a column on, so `view_presentation.toml` still names it
    /// while no layer of `views.toml` does. Creating over that name made
    /// a fresh user view that silently inherited the orphaned overlay's
    /// `hidden`/`order`/`width`, and — being user-only — `r` then
    /// refused it, so no dialog verb could clear it. A row-based check
    /// cannot see that name at all, which is why the refusal reads this
    /// union rather than the list.
    ///
    /// The names this domain refuses outright, reserved by a grammar
    /// outside its own doc (Part 2c §6.1) — `Colours` alone: a column's
    /// `colour` field already spells `none` and `sign` itself, so a
    /// named colour object by either name would be unreachable through
    /// that field and confusing everywhere else. Empty for every other
    /// domain, which has no such collision.
    pub fn reserved_names(self) -> &'static [&'static str] {
        match self {
            Domain::Colours => &geode_core::colour::RESERVED_NAMES,
            Domain::Views
            | Domain::Groupings
            | Domain::Scopes
            | Domain::Schema
            | Domain::Sources => &[],
        }
    }

    /// `config_version` never reaches here: `check_object_name` refuses
    /// it before the caller asks.
    pub fn name_taken(self, config: &Config, name: &str) -> bool {
        if self.reserved_names().contains(&name) {
            return true;
        }
        if self.roster().is_some_and(|roster| roster.contains(&name)) {
            return true;
        }
        if config
            .layered_docs(self.doc())
            .iter()
            .any(|layered| layered.table.contains_key(name))
        {
            return true;
        }
        personalised_names(config, self.presentation_doc()).contains(name)
    }
}

/// The objects a domain's user-layer presentation overlay names — the
/// set [`derive_rows`] reads as "personalised without overriding" and
/// [`Domain::name_taken`] reads as "a name that is spoken for even
/// though nothing lists it". One walk, shared, because the two answers
/// have to agree: a name this set holds and `doc` does not is exactly
/// the case that produces no browse row at all, so nothing on screen
/// could reveal the two walks having drifted apart.
///
/// Empty outright for a domain with no presentation doc
/// (`presentation_doc: None`) — there is nothing to be personalised
/// through. `config_version` is the schema stamp every layered doc
/// carries, not an object.
fn personalised_names<'a>(config: &'a Config, presentation_doc: Option<&str>) -> BTreeSet<&'a str> {
    presentation_doc
        .map(|presentation_doc| {
            config
                .layered_docs(presentation_doc)
                .iter()
                .filter(|layered| layered.layer == Layer::User)
                .flat_map(|layered| layered.table.keys())
                .filter(|name| *name != "config_version")
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default()
}

/// The drift sidecar (spec §5.2, §19.6): `overrides.toml`, user layer
/// only, one entry per forked object keyed `"<doc>.<object>"`, holding
/// the shadowed layer and the shadowed object's canonical TOML text at
/// fork time. A sidecar rather than a key inside the object, because an
/// atomic doc's reader treats an unknown key as a diagnostic.
pub const OVERRIDES_DOC: &str = "overrides";

/// The sidecar's own key for `object` in `doc` — `"<doc>.<object>"`, the
/// join every reader and writer of `OVERRIDES_DOC` uses to name an entry.
pub fn override_key(doc: &str, object: &str) -> String {
    format!("{doc}.{object}")
}

/// The entry recorded when `object` is forked over `shadowed`'s copy.
/// The canonical text, not a hash: `DefaultHasher` is not stable across
/// Rust versions, a crypto dependency is unjustified, and keeping the
/// text makes a real diff free if it is ever wanted (§5.2).
pub fn override_entry(shadowed: Layer, object: &str, value: &toml::Value) -> toml::Value {
    let mut table = toml::Table::new();
    table.insert(
        "shadowed_layer".into(),
        toml::Value::String(shadowed.name().to_string()),
    );
    table.insert(
        "shadowed_text".into(),
        toml::Value::String(object_text(object, toml_value_to_item(value))),
    );
    toml::Value::Table(table)
}

/// The copy a user-layer write of `object` would shadow: the LAST
/// non-user layer defining it, with its value. `None` when no such
/// layer does — a user-only object forks nothing.
pub fn shadow_of(config: &Config, doc: &str, object: &str) -> Option<(Layer, toml::Value)> {
    config
        .layered_docs(doc)
        .iter()
        .filter(|d| d.layer != Layer::User)
        .filter_map(|d| d.table.get(object).map(|v| (d.layer, v.clone())))
        .next_back()
}

/// The overrides sidecar's own entries, user layer only, as `key ->
/// (shadowed_layer, shadowed_text)`. Private: every reader outside this
/// module goes through [`stale_override_keys`] or `derive_rows`'s own
/// drift computation, never the raw map.
fn override_entries(config: &Config) -> BTreeMap<String, (String, String)> {
    config
        .layered_docs(OVERRIDES_DOC)
        .iter()
        .filter(|d| d.layer == Layer::User)
        .flat_map(|d| d.table.iter())
        .filter(|(k, _)| *k != "config_version")
        .filter_map(|(k, v)| {
            let t = v.as_table()?;
            Some((
                k.clone(),
                (
                    t.get("shadowed_layer")?.as_str()?.to_string(),
                    t.get("shadowed_text")?.as_str()?.to_string(),
                ),
            ))
        })
        .collect()
}

/// Whether `doc.object` has a recorded override entry — the gate
/// [`render::removal_edits`] uses to decide whether a delete/revert also
/// touches `overrides.toml`, so a missing sidecar is never created just
/// to remove nothing from it.
pub(super) fn has_override_entry(config: &Config, doc: &str, object: &str) -> bool {
    override_entries(config).contains_key(&override_key(doc, object))
}

/// Entries that describe nothing any more (§19.6): the user layer no
/// longer holds the object, or no layer beneath shadows it. Ignored by
/// `derive_rows` and pruned by the next overrides write.
pub fn stale_override_keys(config: &Config) -> Vec<String> {
    override_entries(config)
        .keys()
        .filter(|key| {
            let Some((doc, object)) = key.split_once('.') else {
                return true;
            };
            let user_has = config
                .layered_docs(doc)
                .iter()
                .any(|d| d.layer == Layer::User && d.table.contains_key(object));
            !user_has || shadow_of(config, doc, object).is_none()
        })
        .cloned()
        .collect()
}

/// The gate [`derive_rows`] applies to compute [`ObjectRow::drifted`]:
/// drift is provable only from the sidecar's recorded text, so no entry
/// means not drifted, never a guess from the shadow's current copy
/// (§19.6).
fn drift_of(entry: Option<&(String, String)>, shadow: Option<&toml::Value>, name: &str) -> bool {
    match (entry, shadow) {
        (Some((_, recorded)), Some(value)) => {
            object_text(name, toml_value_to_item(value)) != *recorded
        }
        _ => false,
    }
}

/// Every object named in `doc`'s layered documents, plus every name
/// `roster` fixes as always-listed (§18.4 — Groupings' nine slots), one
/// row each, sorted by name.
///
/// A rostered name no doc defines gets a row with `layer: None` and
/// `summary: "empty"` — seeded before the layered walk below so a name
/// the walk does touch overwrites that placeholder in place, and a name
/// it never touches is left exactly as seeded. `roster: None` (every
/// domain but Groupings) seeds nothing, so those domains list only what
/// their docs actually define, as before.
///
/// `Config::layered_docs` hands back the per-layer documents in Builtin →
/// Desk → User order, so a single ordered walk answers both markers
/// (spec §5.1 — "no new machinery"):
///
/// - `layer` is the **last** layer whose doc contains the name, which is
///   also the copy that takes effect: every doc these dialogs edit is
///   atomic at depth 1, so a later layer's table replaces the earlier
///   one whole;
/// - `overridden` is the user layer containing it **and** some earlier
///   layer containing it too. The second half is what stops the edit
///   stage offering `Revert to desk` on a view no desk ever had —
///   reverting there would delete the user's own view rather than
///   restore anything (`render::arm_revert` gates on exactly this).
///
/// "The user layer containing it" spans `presentation_doc` as well as
/// `doc`, when the domain has one, and that is not a refinement: §4.1's
/// whole design is that hiding a column writes `view_presentation.toml`
/// and forks nothing, so the commonest user override there leaves
/// `doc`'s user layer empty. Reading the markers off `doc` alone
/// answered `r` with "no user override to revert" while the file `r`
/// would have removed sat on disk. The `some earlier layer` half is
/// still read off `doc` only — that is the half that guarantees
/// reverting leaves an object behind. A domain with no presentation doc
/// at all (`presentation_doc: None`) simply has nothing this half can
/// add.
///
/// Sorted by name by default, kept rather than left in file order: rows
/// come from up to three documents, so "file order" would mean one
/// file's order followed by whatever names the next file added, which is
/// neither the user's nor the desk's order and shifts as soon as
/// anything is overridden. Alphabetical is the one ordering that stays
/// put — and the `BTreeMap` walk below already produces it for free. A
/// domain with a `prefix` (Sources, §19.3) sorts by `(prefix, name)`
/// instead: the dataset a source feeds is the grouping a trader scans
/// by, so its rows cluster by dataset with by-name order only breaking a
/// tie inside one dataset — a second, explicit sort over the by-name
/// output, since the `BTreeMap`'s own key is still the bare name.
///
/// `config_version` is skipped — it is the schema stamp every layered
/// doc carries, not an object.
fn derive_rows(
    config: &Config,
    doc: &str,
    presentation_doc: Option<&str>,
    roster: Option<&'static [&'static str]>,
    summary: fn(&toml::Value) -> String,
    prefix: Option<fn(&toml::Value) -> Option<String>>,
) -> Vec<ObjectRow> {
    // Objects the user layer has personalised without overriding: a
    // `view_presentation.toml` table names the object and forks nothing.
    let personalised = personalised_names(config, presentation_doc);
    // The sidecar's own entries, read once — the drift question below is
    // per-row but the doc is not, and `override_entries` already
    // filters to the user layer.
    let entries = override_entries(config);
    // Accumulated by name — one name can appear in up to three documents
    // and each appearance updates the same row — in a `BTreeMap`, whose
    // key order IS the by-name order described above, so the rows come
    // out sorted without a separate pass. The `Vec<Layer>` beside each
    // row is every layer that defined it, which is what the `overridden`
    // question below needs and the row itself does not carry; the
    // trailing `Option<toml::Value>` is the last NON-USER layer's value —
    // the shadow drift compares the sidecar's recorded text against.
    let mut rows: BTreeMap<String, (Vec<Layer>, ObjectRow, Option<toml::Value>)> = BTreeMap::new();
    // Seeded before the layered walk, not after: a name the walk touches
    // has to update this entry in place (`entry.1.layer = Some(..)`
    // below), and a name it never touches has to survive untouched —
    // `empty`, `layer: None`. Seeding afterward would need its own
    // "don't overwrite what the walk already set" check; seeding first
    // makes the walk's own `or_insert_with`/overwrite the only rule.
    for name in roster.unwrap_or(&[]) {
        rows.insert(
            (*name).to_string(),
            (
                Vec::new(),
                ObjectRow {
                    name: (*name).to_string(),
                    summary: "empty".to_string(),
                    layer: None,
                    overridden: false,
                    drifted: false,
                    prefix: None,
                },
                None,
            ),
        );
    }
    for layered in config.layered_docs(doc) {
        for (name, value) in &layered.table {
            if name == "config_version" {
                continue;
            }
            let entry = rows.entry(name.clone()).or_insert_with(|| {
                (
                    Vec::new(),
                    ObjectRow {
                        name: name.clone(),
                        summary: String::new(),
                        layer: Some(layered.layer),
                        overridden: false,
                        drifted: false,
                        prefix: None,
                    },
                    None,
                )
            });
            entry.0.push(layered.layer);
            // Last writer wins, which is the merge's own rule: both the
            // winning layer and the summary describe the copy that
            // actually takes effect.
            entry.1.layer = Some(layered.layer);
            entry.1.summary = summary(value);
            entry.1.prefix = prefix.and_then(|f| f(value));
            if layered.layer != Layer::User {
                entry.2 = Some(value.clone());
            }
        }
    }
    let mut out: Vec<ObjectRow> = rows
        .into_values()
        .map(|(layers, mut row, shadow)| {
            let mine = layers.contains(&Layer::User) || personalised.contains(row.name.as_str());
            row.overridden = mine && layers.iter().any(|l| *l < Layer::User);
            // §19.6: drift is "the shadowed copy moved since the fork" —
            // provable only from the sidecar's recorded text, so no
            // entry means not drifted, never a guess.
            row.drifted = row.overridden
                && drift_of(
                    entries.get(&override_key(doc, &row.name)),
                    shadow.as_ref(),
                    &row.name,
                );
            row
        })
        .collect();
    if prefix.is_some() {
        out.sort_by(|a, b| (&a.prefix, &a.name).cmp(&(&b.prefix, &b.name)));
    }
    out
}

// ---------------------------------------------------------------------
// The edit stage: fields, destinations, and the draft
// ---------------------------------------------------------------------

/// Which user-layer document one field's value is written to (spec §4.1).
///
/// **This enum is the whole reason the Views dialog is safe to use.**
/// Dragging a column's width is the commonest edit a trader makes, and
/// writing it into `views.toml` would fork the desk's view — a forked
/// view is frozen, so when the desk adds a column next week the trader
/// never sees it. Order, inclusion and width therefore carry
/// [`Destination::Presentation`] and land in `view_presentation.toml`,
/// which `config::load_views` merges *over* the view; only a
/// definitional change (the dataset, the column *set*) carries
/// [`Destination::Doc`] and forks anything.
///
/// It is an enum on the field rather than a rule inside the adapter so
/// the split is mechanical: [`Draft::writes_by_destination`] groups by
/// it, and the flush makes one `config_write::edit` call per group
/// without knowing what either file is for. Exactly two places in the
/// scaffold know `view_presentation.toml` exists — [`Destination::doc`]
/// and [`Domain::presentation_doc`], the same answer asked two ways
/// ("which file does this destination write" and "does this domain have
/// an overlay file at all") — and both route to the Views adapter's own
/// `PRESENTATION_DOC` constant rather than spelling the name again.
///
/// `Ord` because the groups are collected into a `BTreeMap`, so a flush
/// writes its files in a fixed order rather than a hash-random one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    /// The domain's own doc, user layer.
    Doc,
    /// `view_presentation.toml`, user layer.
    Presentation,
    /// `dataset_presentation.toml`, user layer — the Schema dialog's
    /// column stage (dataset-presentation spec §4.1).
    DatasetPresentation,
}

impl Destination {
    /// The config doc (file stem) this destination writes, for `domain`.
    pub fn doc(self, domain: Domain) -> &'static str {
        match (self, domain) {
            (Destination::Doc, domain) => domain.doc(),
            (Destination::Presentation, Domain::Views) => views::PRESENTATION_DOC,
            // Every Groupings field is `Destination::Doc` (spec §8.2 —
            // there is nothing presentational about a dimension chain),
            // so this arm exists only to keep the match exhaustive as
            // domains are added, not because anything can reach it.
            (Destination::Presentation, Domain::Groupings) => {
                unreachable!("Groupings has no Presentation-destined fields")
            }
            // Every Scopes field is `Destination::Doc` too (`scopes.rs`'s
            // module doc: the whole object is a read-only summary), so
            // this arm exists only to keep the match exhaustive.
            (Destination::Presentation, Domain::Scopes) => {
                unreachable!("Scopes has no Presentation-destined fields")
            }
            // Schema's only writable fields are `DatasetPresentation`-
            // destined (its column stage, the arm below); no Schema
            // field carries `Presentation`, since `view_presentation.toml`
            // is keyed by VIEW and this domain browses datasets. So this
            // arm, like the two above, exists only to keep the match
            // exhaustive.
            (Destination::Presentation, Domain::Schema) => {
                unreachable!("Schema has no Presentation-destined fields")
            }
            // Sources joins the same list: `sources.rs`'s module doc has
            // the reasoning (every field is `Destination::Doc`, there is
            // no presentation overlay for a source).
            (Destination::Presentation, Domain::Sources) => {
                unreachable!("Sources has no Presentation-destined fields")
            }
            // Colours joins the same list: `colours.rs`'s module doc has
            // the reasoning (every field is `Destination::Doc`, there is
            // no presentation overlay for a shared colour).
            (Destination::Presentation, Domain::Colours) => {
                unreachable!("Colours has no Presentation-destined fields")
            }
            (Destination::DatasetPresentation, Domain::Schema) => {
                geode_core::view::DATASET_PRESENTATION_DOC
            }
            (Destination::DatasetPresentation, _) => {
                unreachable!("only the Schema door builds DatasetPresentation-destined fields")
            }
        }
    }
}

/// One entry of an [`FieldKind::OrderedList`].
///
/// A deliberately fixed, bounded shape rather than general nesting (spec
/// §3.1): a view's columns and a grouping's dimensions are the only
/// ordered lists in the config model and both fit it, while arbitrary
/// sub-fields would make the edit stage recursive for no reader.
///
/// **Membership is not a field here** (§18.7): which of a field's two
/// lists an item sits in IS its membership, so the same `ListItem` shape
/// describes one of the object's own entries and one of the catalogue
/// entries it may gain. Where the two differ is what
/// [`FieldKind::OrderedList`] documents, and nothing has to keep a flag
/// and a position agreeing with each other.
///
/// `presentation` is the column's presentation as the trader sees it —
/// the kind default with the desk's and the overlay's keys applied, i.e.
/// `ViewSpec::presentation_of` after `load_views` (Part 2c §4.3) — not a
/// bare `width` (the shape this field carried before Part 2c): a
/// picker's `column_summary` needs precision, colour and scale too, and
/// carrying the whole `ColumnPresentation` is what lets it read them off
/// one value rather than growing a field per format key. A domain with
/// no presentation concept at all (Groupings' `dimensions`) carries
/// `ColumnPresentation::default()`, which is indistinguishable from "no
/// override of anything" — exactly what such a domain means to say.
#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub name: String,
    /// Shown rather than hidden. For a view this is the inverse of
    /// `ColumnPresentation::hidden`, and it is presentation, never the
    /// column set: a hidden column stays in `ViewSpec::columns`, so the
    /// compiler still selects it and unhiding costs nothing. `included`
    /// is the one truth from here on — the tick flips it and
    /// `views::presentation_table` reads only it to decide `hidden`;
    /// `presentation.hidden` is merely the seed `views::fields` read out
    /// of the merged overlay when this item was built, never consulted
    /// again.
    pub included: bool,
    pub presentation: ColumnPresentation,
    /// The `[[columns]]` `kind` a first-time write of this item needs —
    /// `"dimension"` or `"measure"` for Views (`views::fields`'s own doc
    /// has the schema-role mapping and why a `key`/`attribute` column
    /// never reaches this list at all), the ViewColumn variant's own kind
    /// for a column already in the view. `None` where a list has no
    /// column-kind concept (Groupings' `dimensions`): read only by
    /// `views::columns_for`, on the one path that writes a brand new
    /// `[[columns]]` entry — a column already in `Draft::source` keeps
    /// its own table, `kind` included, untouched by this field entirely.
    pub kind: Option<String>,
    /// Muted text painted after the name, supplied by the domain
    /// (Scopes: a selection's values on the edit stage, a value's row
    /// count or `not in data` on the Values stage). `None` paints
    /// Views' own `column_summary` as before. Display only — never
    /// written, never filtered on.
    pub note: Option<String>,
}

/// The closed vocabulary an object's fields are built from (spec §3.1).
///
/// Closed on purpose: validation is then by construction — a `Choice`
/// offers only datasets that exist, an `OrderedList` only columns the
/// view has — which is what makes most of the loader's old diagnostics
/// unreachable from these dialogs.
///
/// Two variants have no key in this task and say so rather than
/// pretending: [`FieldKind::Text`] needs in-place editing behind `i`,
/// and [`FieldKind::MultiChoice`] needs a per-option row for `space` to
/// tick. Views uses neither; both arrive with the Part 2 adapter that
/// first needs one, which is also when a test can see them.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text(String),
    /// `step` is the distance one `space` moves; `wrap` makes the range
    /// circular — a hue, say — so a step past `max` lands at
    /// `min + overshoot` rather than pinning at `max` (spec §5.4). Every
    /// `Number` this crate builds today is `step: 1, wrap: false` (the
    /// only one live is Sources' `stable_polls`; Groupings' `slot` is
    /// display-only and a [`FieldKind::Text`], not a `Number`, per
    /// `groupings.rs`'s own doc) — a future field that steps by more than
    /// one, or wraps, is what this pair exists for.
    Number {
        value: i64,
        min: i64,
        max: i64,
        step: i64,
        wrap: bool,
    },
    Bool(bool),
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    MultiChoice {
        options: Vec<String>,
        ticked: BTreeSet<String>,
    },
    /// The object's own ordered list, and — for a list a trader can add
    /// to — the catalogue of what may join it (spec §18.7). `items` is
    /// the only list that is ordered, written, counted or reorderable.
    ///
    /// `available` is `None` where ticking IS membership and there is no
    /// catalogue to promote out of (Groupings' `dimensions`, whose
    /// unticked rows are already in `items`), and `Some` — possibly
    /// EMPTY — where one exists (Views' `columns`, whose catalogue empties
    /// out once the trader has added every column the dataset offers).
    /// The distinction is load-bearing rather than tidy: `x` demotes into
    /// a catalogue that exists and refuses where none does, so reading
    /// "is this list catalogue-less" off `available.is_empty()` would make
    /// `x` go dead on a fully-added Views list — the exact regression
    /// `remove_selected`'s own doc records. `available` is unordered by
    /// construction: nothing writes it and nothing reads its order.
    ///
    /// Two lists rather than one list and a flag, so the
    /// members-before-available rule four mutators used to maintain by
    /// hand is not a rule at all.
    OrderedList {
        items: Vec<ListItem>,
        available: Option<Vec<ListItem>>,
    },
}

/// Which layer's value a column-stage field is showing (dataset-
/// presentation spec §5.3): the trader's own view-level override, their
/// dataset-level setting, or the desk view's own key. `None` when the
/// kind default is in force. Painted as a lowercase chip in the slot the
/// Schema rows' layer badge uses — the two never appear together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Desk,
    Dataset,
    View,
}

impl Provenance {
    pub fn name(self) -> &'static str {
        match self {
            Provenance::Desk => "desk",
            Provenance::Dataset => "dataset",
            Provenance::View => "view",
        }
    }
}

/// Which door opened the column stage (§4.1, §5): the Views dialog's
/// member row (the fields write the view overlay over a desk + dataset
/// baseline) or the Schema dialog's column row (the fields write the
/// dataset overlay over the kind default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDoor {
    View,
    Dataset,
}

/// The two layers BELOW the view overlay under one column, each as the
/// keys that layer itself sets — NOT merged — so provenance and the fold
/// can name a layer (§5.1–§5.3).
///
/// The view overlay itself is deliberately absent. It had exactly one
/// reader, `dataset_columns::provenance_of`'s View arm, and reading it
/// there was the defect: a field stepped BACK to the value the layer
/// below already gives still read `view`, because the captured overlay
/// still held a key the write was about to remove. The chip now asks
/// only whether the field differs from desk + dataset, which is a
/// question the field's own value answers (the final whole-branch
/// review's named risk 4).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnLayers {
    pub desk: ColumnPresentation,
    pub dataset: ColumnPresentation,
}

impl ColumnLayers {
    /// desk with the dataset level merged over — what a cleared VIEW key
    /// falls to, and what the view writer compares against (§5.1).
    pub fn below_view(&self) -> ColumnPresentation {
        let mut p = self.desk.clone();
        p.merge_over(&self.dataset);
        p
    }
}

/// Everything a column stage needs beyond the seven fields, set by the
/// door that opened it and dropped by [`Draft::leave_column`].
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnContext {
    pub door: ColumnDoor,
    pub layers: ColumnLayers,
    /// The Schema door only: the `[<dataset>]` table of
    /// `dataset_presentation.toml` as it stands (empty when absent), so
    /// the writer can render the dataset's OTHER personalised columns
    /// verbatim beside the one being edited (§4.5).
    pub overlay_object: toml::Table,
    /// The Schema door only: the scratch item the fields fold into,
    /// where the Views door folds into its parent list's item.
    pub item: Option<ListItem>,
}

/// One editable property of one object.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The TOML key within the object (`dataset`, `columns`). Also what a
    /// diagnostic's key path is matched against once readers carry one
    /// (spec §8.5 — see [`Draft::diagnostics`]).
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub dest: Destination,
    /// The layer this row's value came from, painted as a badge on the
    /// row when `Some` (§19.4). Filled by the Schema adapter from
    /// `Config::explain`; every writable domain leaves it `None`, since
    /// the object-level badge in the header already says whose copy is
    /// on screen and a second badge per row would only repeat it.
    pub layer: Option<Layer>,
}

/// One row of the edit stage: a field, or one item of a field's ordered
/// list.
///
/// Flattened rather than nested so the cursor, `space` and `shift+j` all
/// speak one index — the same shape the browse list has, and the reason
/// `vimnav::apply` works unchanged in both stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditRow {
    Field(usize),
    /// One of the object's own list entries — `field`'s `items`.
    Item {
        field: usize,
        item: usize,
    },
    /// One row of a list's `available` catalogue. A variant of its own so
    /// a consumer cannot treat an available row as one of the object's
    /// own by omission — every match on [`EditRow`] has to say what
    /// `space`, `x`, a click or a label means here.
    Available {
        field: usize,
        item: usize,
    },
}

/// What the row under the cursor answers to — the footer's whole
/// question (user ruling 2026-09-13, "when we're highlighting a row that
/// is text based, show the `press i` helper text; when it's on something
/// we cycle, show the space/shift+space etc").
///
/// Derived from the row rather than from the domain, which is the point:
/// a footer that named `space` on a read-only `Text`, or `i` on a
/// `Choice`, would teach a key that is inert on the row the trader is
/// actually looking at — the defect class this interaction model exists
/// to remove. Answered by [`Draft::selected_vocabulary`], which is the
/// same match [`Draft::step_selected`] and [`super::render::
/// open_text_field`] make, in the same order, so the footer cannot
/// promise a key those two would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowVocabulary {
    /// Nothing on this row changes and nothing types: a display-only
    /// `Text` (Groupings' `slot`, Scopes' two summaries, every Schema
    /// row), an `OrderedList`'s own header row, a `MultiChoice`, or no
    /// row at all under an over-narrow filter.
    Inert,
    /// A value the step keys cycle and `i` cannot open: `Choice`, `Bool`.
    Steps,
    /// Both: a `Number`, which steps by one and takes a typed value.
    StepsAndTypes,
    /// `i` alone: a `Text` row the domain marks editable.
    Types,
    /// One of the object's own list entries — `space` ticks it.
    Item,
    /// A catalogue row — `space` adds it to the object's list.
    Available,
}

/// What a dragged list row carries (4c §18.9.1): the field's key, which
/// block it came from, and the item's NAME — never an index. The keyboard
/// stays live during a drag, so a keystroke can reorder or remove between
/// the grab and the drop; a payload resolved by name at drop time lands on
/// the row the trader picked up, or on nothing, never on whichever column
/// now holds the grabbed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDrag {
    pub field: String,
    /// `true` for an [`EditRow::Item`], `false` for an
    /// [`EditRow::Available`] — §18.7.1's variant distinction carried onto
    /// the wire.
    pub own: bool,
    pub name: String,
}

/// What a keystroke is waiting to have confirmed. Each of the three is
/// unrecoverable in its own direction — deleting the user's copy of an
/// object, throwing away a personal override, or forking an object out of
/// the layer that maintains it — so each takes a second, deliberate
/// keystroke rather than happening under one letter.
///
/// There is no `Discard`. It existed to guard the staged-draft model's
/// unsaved work; now every field edit is recorded the moment it is made,
/// on a batch that outlives the stage and the dialog and reaches disk on
/// its own timer, so leaving abandons nothing and a confirm there would
/// be a question about a state that cannot arise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Delete,
    Revert,
    /// `o` on a saved scope the user layer already owns (`Domain::Scopes`
    /// only): overwrite its contents with whatever the frame currently
    /// holds. Armed only there, because only there is something lost —
    /// with no desk copy underneath, the scope's previous selection is
    /// gone for good. On a desk- or builtin-owned scope `o` writes at
    /// once and says so instead (`render::overwrite_scope`): nothing is
    /// lost, the desk's copy is still there and `r` restores it, and the
    /// fork it makes is announced rather than asked about, exactly as a
    /// field edit's is (user ruling 2026-09-14). There used to be a
    /// `Fork` confirm for those field edits and a `forks` payload here
    /// so one prompt could disclose the fork; both went with that ruling.
    Overwrite,
}

impl Confirm {
    /// The prompt, naming the object and the consequence rather than
    /// asking "are you sure": the interaction model's own copy rule, and
    /// gpui-component's design guide's.
    pub fn prompt(self, name: &str) -> String {
        match self {
            Confirm::Delete => format!("Delete '{name}' from your config?"),
            Confirm::Revert => format!("Throw away your changes to '{name}'?"),
            // User-owned (the only case that arms it): nothing underneath
            // to fall back to, so the scope's previous contents really
            // are gone.
            Confirm::Overwrite => format!(
                "Replace '{name}' with the frame's current scope? Its saved selection is lost."
            ),
        }
    }
}

/// A value field open in the filter row's place (§19.1): the shared
/// `Input` seeded with a row's value, `enter` applying it down the tick's
/// own path and `escape` cancelling. The chain field (§18.8) is the case
/// with `completions: true` — the rows below are then
/// [`groupings::chain_candidates`] rather than the edit rows.
///
/// `row` is an [`EditRow`], not a field index, so a future item-level
/// text (a column's width, Part 2c) is one more arm and not a second
/// mechanism; nothing in this plan opens it on anything but
/// `EditRow::Field`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEntry {
    pub row: EditRow,
    pub completions: bool,
}

/// One object being edited.
///
/// The edit **buffer**, and what the edit stage actually paints: a
/// keystroke changes a field here — so the trader sees it at once — and
/// [`apply::commit_edit`] puts that change on the pending batch the next
/// flush merges, applies and writes (spec §7.1). The buffer also carries
/// the cursor, the confirm and the row structure, and it is what the
/// difference against [`Draft::baseline`] is taken from — that difference
/// is precisely "what this keystroke changed", which is what decides the
/// files a flush touches.
///
/// An earlier build staged here and wrote only on `s`, reasoning that a
/// write per keystroke would fire the 500 ms watcher mid-edit and reload
/// a half-finished object. What actually removes that hazard is that the
/// change never travels through the disk to reach the screen: the flush
/// merges the documents already in hand, and the watcher's reload of our
/// own write is a no-op (see [`apply`]).
#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    /// Created by `n` this session and not yet on the browse list the
    /// config derives — the edit header says `new` beside the layer until
    /// the dialog is left. Nothing else reads it: the write path treats a
    /// new object like any other Doc write.
    pub is_new: bool,
    pub fields: Vec<Field>,
    /// The object exactly as the merged doc holds it, kept so a
    /// [`Destination::Doc`] write can preserve everything the field
    /// vocabulary does not model — a view's `grouping`, `sort`, `joins`,
    /// per-column `format`, `label` and a derived column's `sql`.
    /// Rendering a Doc override from the fields alone would silently
    /// delete all of it.
    pub source: toml::Table,
    /// The fields as they were when the draft was built, or as the last
    /// applied edit left them. Which files an edit touches is this
    /// comparison and nothing else, so a keystroke that puts a value back
    /// where it started changes nothing and writes nothing.
    baseline: Vec<Field>,
    /// `source` as it stood at the same moment `baseline` did. For every
    /// domain but Scopes this never diverges from `source` after
    /// construction — nothing else in this scaffold mutates `source`
    /// directly — so it costs those domains nothing. Scopes' `o`
    /// (`scopes::overwrite_with`) is the one verb that replaces `source`
    /// wholesale while leaving the *painted* fields free to describe it
    /// however a summary function likes; comparing only `fields` against
    /// `baseline` would then make dirtiness depend on two summary
    /// strings never colliding, which is not a property `selects_summary`
    /// promises to keep as it grows. Comparing `source` directly closes
    /// that by construction: the actual object decides whether anything
    /// changed, not its rendering.
    baseline_source: toml::Table,
    /// Cursor over [`Draft::visible_rows`] (§18.3) — the FILTERED list,
    /// not [`Draft::rows`] — the same convention
    /// `ObjectDialogState::selected` holds for the browse stage. One
    /// cursor per stage, never both live at once.
    pub selected: usize,
    /// The edit stage's filter (§18.3), mirrored from the shared `Input`
    /// by `ObjectDialogState::set_query` exactly as the browse query is.
    /// Lives on the draft rather than beside it because `selected`
    /// indexes the FILTERED list and both must move together.
    pub query: String,
    /// [`Domain::validate`]'s output for the draft as it stands, refreshed
    /// on every change (spec §7.2).
    ///
    /// Shown on the header AND on the field row it names (§19.5): every
    /// reader across `geode-core` now fills `Diagnostic::path` with the
    /// key it was looking at, and [`Draft::row_for_path`] turns that path
    /// back into the [`EditRow`] it describes — [`Draft::flagged_rows`] is
    /// what `render.rs` reads to paint the row glyph, while the header
    /// list (this field, read directly) stays the text of record for
    /// every diagnostic, matched or not.
    pub diagnostics: Vec<Diagnostic>,
    /// A text field is open (§19.1). While it is, `query` holds the text
    /// being typed rather than a filter — the shared `Input` mirrors into
    /// it exactly as a filter does, so there is no second text buffer —
    /// and, for the chain field's `completions: true`,
    /// [`Draft::visible_rows`] is the completion list. A field on the
    /// draft beside `confirm` rather than a `Stage`, because the stage is
    /// what the escape ladder and the browse cursor restore key on, and
    /// both must still read `Edit` here.
    pub text_entry: Option<TextEntry>,
    /// The object's OWN fields, while [`Stage::Column`] has swapped
    /// `fields` out for one column's seven (Part 2c §5.2). `None`
    /// everywhere else.
    ///
    /// The view's list has to stay reachable while the stage is open,
    /// because the write path renders the whole object on every
    /// keystroke — `views::presentation_table` reads
    /// `list_items("columns")` — and the column stage's own writes are
    /// exactly writes to that list. Stashing the fields here rather than
    /// building a second `Draft` is what keeps `is_dirty`,
    /// `writes_by_destination`, `mark_saved` and the commit path
    /// unchanged: they see one draft whose fields happen to be a
    /// column's.
    parent_fields: Option<Vec<Field>>,
    /// Which column [`Draft::parent_fields`] was stashed for — the fold
    /// target, and the scope [`Draft::row_for_path`] narrows to. `Some`
    /// exactly when `parent_fields` is; [`Draft::column`] is the read.
    column: Option<String>,
    /// Which column [`Draft::parent_fields`] was stashed for by the
    /// VALUES stage (scopes-editing spec §4). `Some` exactly when
    /// `parent_fields` is and `column` is `None`; the two stages share
    /// the stash and can never both be open.
    values: Option<String>,
    /// What the door that opened [`Stage::Column`] knows and the seven
    /// fields do not (dataset-presentation spec §4.1, §5.1): which door
    /// it was, the three layers under the column, and — the Schema door
    /// — the overlay table and the scratch item the fold writes into.
    /// `None` off the column stage, set by the door and dropped by
    /// [`Draft::leave_column`].
    pub column_ctx: Option<ColumnContext>,
    /// The trader's DATASET-level presentation for this object's
    /// columns, by column name (dataset-presentation spec §5.1) — the
    /// layer between the desk view's own keys and this view's overlay.
    ///
    /// It lives on the draft because the writer's entry point,
    /// [`Domain::to_table`], has no `Config` to read it from, and the
    /// writer is exactly where getting it wrong is silent: compared
    /// against the desk alone, every dataset-level key reads as a
    /// divergence and is copied into `view_presentation.toml` as a
    /// per-view override the trader never made
    /// ([`views::baseline_below`]).
    ///
    /// **Filled for `Domain::Views` alone**, by [`Domain::draft`] from
    /// the config it is handed and refreshed by
    /// `render::enter_column_stage` from the pending-aware one. Empty
    /// for every other domain — none of them has a view whose columns a
    /// dataset could speak for, and the Schema door's own layer is the
    /// one it writes, carried on [`ColumnContext`] instead.
    pub dataset_layer: BTreeMap<String, ColumnPresentation>,
}

/// Which way [`Draft::step_selected`] moves the value under the cursor.
/// A parameter rather than a second copy of the stepping match, so the
/// forward and backward paths cannot drift apart from each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepDirection {
    Forward,
    Backward,
}

/// What one `space`/`shift+space` did to the row under the cursor.
///
/// Three answers rather than a `bool`, because [`Step::Inert`] and
/// [`Step::Refused`] are different things to say to a trader: the first
/// is "this row has no value that key changes", the second is "it does,
/// and that particular step would ask the config model for a state it
/// cannot hold". A key that appears inert is the defect class this
/// interaction model exists to remove, so both carry their own notice
/// rather than sharing one.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The value moved; the caller revalidates and commits.
    Changed,
    /// Nothing on this row has a value this key changes.
    Inert,
    /// Declined, with the reason to show. See [`Draft::step_selected`]'s
    /// `Destination::Doc` rule.
    Refused(String),
}

impl Step {
    /// Did the draft actually move? The one-bit question most callers —
    /// and most tests — are asking.
    pub fn changed(&self) -> bool {
        matches!(self, Step::Changed)
    }
}

/// What a cleared column-stage key fell to (dataset-presentation spec
/// §3.3, §4.4, §5.2): the desk view's own key, the dataset level, or —
/// from the Schema door — whatever each view says. `None` means nothing
/// below sets the key, so there is nothing to tell the trader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FellTo {
    Desk,
    Dataset,
    EachView,
}

/// One fold's outcome: the key the trader cleared, and what it fell to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub key: &'static str,
    pub to: Option<FellTo>,
}

impl Draft {
    /// The rows the edit stage paints, in order: every field, each
    /// ordered list's own items directly under it, then that list's
    /// available catalogue (§18.7.1).
    pub fn rows(&self) -> Vec<EditRow> {
        let mut out = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            out.push(EditRow::Field(i));
            if let FieldKind::OrderedList { items, available } = &field.kind {
                for item in 0..items.len() {
                    out.push(EditRow::Item { field: i, item });
                }
                for item in 0..available.as_ref().map_or(0, Vec::len) {
                    out.push(EditRow::Available { field: i, item });
                }
            }
        }
        out
    }

    /// What the filter ranks a row by: exactly the text the row paints as
    /// its label — a field's label, the name of one of the object's own
    /// items, the name of an available one — and nothing more
    /// (the browse list's own rule, for the same reason: matching text
    /// the user cannot see breaks the agreement between what ranked and
    /// what is highlighted).
    pub fn row_label(&self, row: EditRow) -> String {
        match row {
            EditRow::Field(i) => self.fields[i].label.clone(),
            EditRow::Item { field, item } => match &self.fields[field].kind {
                FieldKind::OrderedList { items, .. } => items[item].name.clone(),
                _ => String::new(),
            },
            // An available row's label is its name too — the same text,
            // said by a different row, because it is the same column
            // seen from the other side of the verb that moves it.
            EditRow::Available { field, item } => match &self.fields[field].kind {
                FieldKind::OrderedList {
                    available: Some(available),
                    ..
                } => available[item].name.clone(),
                _ => String::new(),
            },
        }
    }

    /// The rows the edit stage shows: every row whose [`Draft::row_label`]
    /// [`crate::listfilter::rank`] matches the query, in ROW order —
    /// never score order. `Ranked::row` indexes [`Draft::rows`].
    ///
    /// This is where the edit stage's own filtering deliberately parts
    /// ways with the browse list's (review round 1): browse's rows are
    /// an unordered catalogue, so ranking by match quality is a pure
    /// improvement, but the edit stage's row order **is the data** — a
    /// view's column order, a grouping slot's chain order — and
    /// reordering it out from under a filter would be actively
    /// misleading rather than merely surprising. Two reasons, both still
    /// standing after §18.1 gave each block a header and each row of the
    /// object's own list a grip. First, the order is a value the trader
    /// is editing and
    /// `shift+j`/`shift+k` are how they edit it: neither can mean
    /// anything coherent against a list whose painted order `shift+j`
    /// does not control. Second, a section header marks where its block
    /// BEGINS — it says nothing about a row that a score sort has thrown
    /// into the middle of the wrong block, so under score order the
    /// header would be actively wrong rather than merely absent. So
    /// `rank` is used only to decide which rows survive the query;
    /// `sort_by_key` afterwards restores row order among the survivors,
    /// discarding nothing but the score-derived ordering.
    pub fn visible_rows(&self) -> Vec<crate::listfilter::Ranked> {
        // §19.1: while any field is open, `query` is the value being
        // typed into IT, not a filter over the rows — so the rows below
        // must not be narrowed by it. The chain field (§18.8) is the one
        // case where the rows really are a search: its own `query` is
        // the chain being typed and the rows are its completions. A
        // plain field's rows stay every edit row, unfiltered and in row
        // order, so the trader sees the row they are editing highlighted
        // in place (spec §19.1: "the rows below stay the edit rows with
        // the edited one highlighted"). An edit-stage filter that was
        // applied before `i` is lost the moment the field opens (the
        // seed overwrites `query`) — the same rule the chain field
        // already has (§18.8) — so there is nothing left to apply here
        // even if this branch tried to.
        if let Some(entry) = self.text_entry {
            return if entry.completions {
                groupings::chain_candidates(self)
            } else {
                let labels: Vec<String> =
                    self.rows().into_iter().map(|r| self.row_label(r)).collect();
                crate::listfilter::rank(&labels, "")
            };
        }
        let labels: Vec<String> = self.rows().into_iter().map(|r| self.row_label(r)).collect();
        let mut ranked = crate::listfilter::rank(&labels, &self.query);
        ranked.sort_by_key(|m| m.row);
        ranked
    }

    /// The key of the FIELD the cursor's row belongs to — the field
    /// itself, or the list a member or available row sits in — which is
    /// what [`Domain::help`] is asked with: a row of a list explains the
    /// list. `None` off the end of a filtered list.
    pub fn selected_field_key(&self) -> Option<&str> {
        self.field_key_of(self.selected_row()?)
    }

    /// [`Self::selected_field_key`] for a row the caller has already
    /// resolved — the paint path holds `rows`/`visible` and must not
    /// derive them a third time per frame just to ask this.
    pub fn field_key_of(&self, row: EditRow) -> Option<&str> {
        let field = match row {
            EditRow::Field(field) => field,
            EditRow::Item { field, .. } | EditRow::Available { field, .. } => field,
        };
        self.fields.get(field).map(|f| f.key.as_str())
    }

    /// The row the cursor is on, if the cursor is in range — indexed
    /// through [`Draft::visible_rows`], so every verb acts on the row the
    /// trader is actually looking at, filtered or not (§18.3).
    pub fn selected_row(&self) -> Option<EditRow> {
        let rows = self.rows();
        self.visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied())
    }

    /// What the row under the cursor answers to (user ruling
    /// 2026-09-13) — see [`RowVocabulary`] for why the footer asks the
    /// row rather than the domain.
    ///
    /// Groupings is the one domain whose footer must NOT take `i` from
    /// this answer: there `i` reaches past the selected row to the
    /// slot's whole chain (§18.8), so it is live on every row including
    /// the display-only `slot`. That exception lives at the two call
    /// sites in `render` — the edit footer and `actions()`, the `i`
    /// button (spec §20.3) — not here, because it is a fact about the
    /// domain's `i` and not about any row.
    pub fn selected_vocabulary(&self, domain: Domain) -> RowVocabulary {
        self.vocabulary_of(self.selected_row(), domain)
    }

    /// [`selected_vocabulary`](Self::selected_vocabulary) for any row —
    /// what the value chip asks per painted row (spec §20.3), so the chip
    /// and the footer can never disagree about whether a row steps.
    pub fn vocabulary_of(&self, row: Option<EditRow>, domain: Domain) -> RowVocabulary {
        match row {
            None => RowVocabulary::Inert,
            Some(EditRow::Item { .. }) => RowVocabulary::Item,
            Some(EditRow::Available { .. }) => RowVocabulary::Available,
            Some(EditRow::Field(i)) => match &self.fields[i].kind {
                // `step_selected`'s own guard, mirrored: a `Choice` with
                // one option (a config with a single dataset) steps
                // nowhere in either direction, so naming the step keys
                // there would be the inert-key lie again. A `Number` at
                // the end of its range is not the same case — the other
                // direction still moves, and an out-of-range one refuses
                // out loud rather than doing nothing.
                FieldKind::Choice { options, .. } if options.len() < 2 => RowVocabulary::Inert,
                FieldKind::Choice { .. } | FieldKind::Bool(_) => RowVocabulary::Steps,
                FieldKind::Number { .. } => RowVocabulary::StepsAndTypes,
                FieldKind::Text(_) if domain.text_editable(&self.fields[i].key) => {
                    RowVocabulary::Types
                }
                FieldKind::Text(_)
                | FieldKind::MultiChoice { .. }
                | FieldKind::OrderedList { .. } => RowVocabulary::Inert,
            },
        }
    }

    /// Re-point the cursor at `row`'s own position in the FILTERED list,
    /// after a verb has changed which row that is — the identity-based
    /// replacement for the arithmetic `self.selected = self.selected -
    /// item + end` used to do when `selected` indexed the unfiltered
    /// [`Draft::rows`] directly. Leaves `selected` where it is if `row`
    /// is no longer visible under the current query, which none of its
    /// callers can actually produce (moving an item never changes its
    /// own label), but is the honest fallback for a future
    /// one that might. Neither an *add* nor a *removal* calls this — the
    /// cursor stays behind on the row that was next rather than following
    /// the item from one list into the other (`step_selected`'s and
    /// `remove_selected`'s own comments have the ruling).
    fn follow(&mut self, row: EditRow) {
        let rows = self.rows();
        if let Some(position) = self
            .visible_rows()
            .iter()
            .position(|m| rows.get(m.row) == Some(&row))
        {
            self.selected = position;
        }
    }

    /// Has anything changed since the last applied edit? Compared against
    /// the baseline rather than tracked with a flag, so putting a value
    /// back where it started changes nothing and writes nothing.
    ///
    /// `true` in exactly two situations, and a reader relying on this
    /// invariant should count on no others:
    ///
    /// 1. *within* the keystroke that changed a field, before
    ///    [`apply::commit_edit`] records it and moves the baseline;
    /// 2. indefinitely, for a change `commit_edit` **refused**: an error
    ///    diagnostic (`apply::blocking_diagnostic`) or a shell with no
    ///    writable user directory both return before `mark_saved()`, so
    ///    the value stays on the draft, painted and dirty, until a later
    ///    commit succeeds and carries it along. That is deliberate — the
    ///    keystroke is not lost — and it is why no verb here consults
    ///    dirtiness to decide whether something needs saving.
    ///
    /// `source` is compared too, not just `fields` — see
    /// [`Draft::baseline_source`]'s own doc for why a fields-only
    /// comparison is not enough once a verb (Scopes' `o`) can replace
    /// `source` out from under a painted summary.
    pub fn is_dirty(&self) -> bool {
        self.fields != self.baseline || self.source != self.baseline_source
    }

    /// The column whose presentation is open, if [`Stage::Column`] is
    /// (Part 2c §5.2). The one question `render::revalidate` asks before
    /// folding, and the reason the fold is a property of the draft rather
    /// than of the stage the gpui side happens to be painting.
    pub fn column(&self) -> Option<&str> {
        self.column.as_deref()
    }

    /// The column whose values are open, if [`Stage::Values`] is.
    pub fn values(&self) -> Option<&str> {
        self.values.as_deref()
    }

    /// Open the column stage over `column` (Part 2c §5.2): stash the
    /// object's fields, install `fields` (the column's seven —
    /// `views::column_fields`), and start the stage clean.
    ///
    /// `false`, changing nothing, when `column` is not one of the
    /// object's own members. The check is the whole safety property here:
    /// [`Draft::fold_column`] finds its item BY NAME in the stashed list,
    /// so a stage opened over a name that list does not hold would take
    /// every keystroke and fold none of them — a stage that silently
    /// discards work, which is worse than one that never opens. An
    /// AVAILABLE row is a non-member by exactly this test, which is what
    /// keeps 2b's notice on it rather than opening a stage over a column
    /// the view does not have.
    ///
    /// `baseline` becomes the installed fields, so the first keystroke in
    /// the stage is the first difference — and so `writes_by_destination`
    /// compares seven column fields against seven, never against the two
    /// it replaced. `query`, `selected`, `text_entry` and `confirm` all
    /// reset for the same reason [`ObjectDialogState::enter_edit`] resets
    /// its own: a filter or a half-open field belonging to the list
    /// behind would be live against rows that no longer exist.
    pub fn enter_column(&mut self, column: &str, fields: Vec<Field>) -> bool {
        // Re-entry would be the end of the object (the final review's
        // M-1): the membership test below passes THROUGH
        // `field_by_key`'s parent fallback, so from an already-open
        // stage it would stash the seven installed column fields as
        // `parent_fields` and drop the view's own list forever — after
        // which `list_items("columns")` answers `None` and the write
        // path renders a view with no columns at all. Unreached today
        // (this stage installs no `EditRow::Item` rows, and
        // `commit_selected_row` gates on `column().is_none()` besides),
        // so this line is what makes it unrepresentable rather than
        // merely unreached.
        //
        // The Values stage shares this same stash (`Draft::values`'s own
        // doc), so it is refused here too — entering over an open Values
        // stage would stash ITS installed fields as `parent_fields` and
        // drop the object's own list exactly as re-entry would.
        if self.column.is_some() || self.values.is_some() {
            return false;
        }
        // Membership is what the door lists: the Views door's `columns`
        // list, or — the Schema door — a parent field keyed
        // `columns.<col>`, which is how `schema::fields` names a column
        // row (dataset-presentation spec §4.1).
        let listed = self
            .list_items("columns")
            .is_some_and(|items| items.iter().any(|i| i.name == column));
        let row_key = format!("columns.{column}");
        let is_member = listed || self.fields.iter().any(|f| f.key == row_key);
        if !is_member {
            return false;
        }
        self.parent_fields = Some(std::mem::replace(&mut self.fields, fields));
        self.column = Some(column.to_string());
        self.baseline = self.fields.clone();
        self.query.clear();
        self.selected = 0;
        self.text_entry = None;
        true
    }

    /// Write the installed column fields back onto the item the object's
    /// own list holds (Part 2c §5.2) — the projection's whole mechanism.
    ///
    /// Called from `render::revalidate`, so it runs on every changed
    /// value BEFORE the validator and the write path read the list. A
    /// no-op outside the stage, and a no-op for a column the stashed list
    /// no longer holds, which [`Draft::enter_column`]'s membership check
    /// makes unreachable.
    ///
    /// **The baseline goes in and the two `Text` fields come back out.**
    /// A cleared `label` or an `auto` width means "stop overriding this",
    /// which resolves to whatever the layers BELOW this door say
    /// (`views::fold_into` has the full statement, and why writing `None`
    /// there would silently swallow the clear). The fold then re-seeds
    /// those two fields from the item, so that value is on screen on the
    /// same keystroke rather than a blank the next rebuild contradicts.
    /// Re-seeding is a no-op for every other value, since the field and
    /// the item already agree.
    ///
    /// Returns the key that was cleared and the layer it fell to, for the
    /// caller's notice — see `views::fold_into` on why at most one key
    /// can be cleared per fold.
    ///
    /// **Which door opened the stage decides both the baseline and the
    /// fold target**, read off [`Draft::column_ctx`] rather than assumed
    /// (dataset-presentation spec §5.1; the final review's M-5, now
    /// answered): the Views door folds into the item its own `columns`
    /// list holds, over desk + dataset; the Schema door folds into the
    /// context's own scratch item, over the kind default, since a dataset
    /// has no view list to project into. A stage whose door left no
    /// context folds nothing — there is no baseline to be honest about.
    pub fn fold_column(&mut self) -> Option<Fold> {
        let name = self.column.clone()?;
        // Both doors install one; a stage without it folds nothing, which
        // is the honest answer but a silently inert one, so the debug
        // build says so rather than leaving a future door to discover it
        // by watching keystrokes vanish.
        debug_assert!(
            self.column_ctx.is_some(),
            "a column stage always carries its door's context"
        );
        // The `door` is `Copy` and the `layers` are what the baseline and
        // the fell-to decision below read — cloning the whole context
        // would clone the overlay table and the scratch item on every
        // keystroke, for two fields neither arm touches.
        let (door, layers) = {
            let ctx = self.column_ctx.as_ref()?;
            (ctx.door, ctx.layers.clone())
        };
        let baseline = match door {
            ColumnDoor::View => layers.below_view(),
            ColumnDoor::Dataset => ColumnPresentation::default(),
        };
        let cleared = match door {
            ColumnDoor::View => {
                let parent = self.parent_fields.as_mut()?;
                let field = parent.iter_mut().find(|f| f.key == "columns")?;
                let FieldKind::OrderedList { items, .. } = &mut field.kind else {
                    return None;
                };
                let item = items.iter_mut().find(|i| i.name == name)?;
                let cleared = views::fold_into(item, &self.fields, &baseline);
                // Copied out so the parent's borrow ends before the
                // installed fields are written.
                let (label, width) = (item.presentation.label.clone(), item.presentation.width);
                self.reseed_cleared_texts(label, width);
                cleared
            }
            ColumnDoor::Dataset => {
                let ctx_mut = self.column_ctx.as_mut()?;
                let item = ctx_mut.item.as_mut()?;
                let cleared = views::fold_into(item, &self.fields, &baseline);
                let (label, width) = (item.presentation.label.clone(), item.presentation.width);
                self.reseed_cleared_texts(label, width);
                cleared
            }
        };
        let key = cleared?;
        let to = match door {
            // §4.4: below the dataset level is the desk view's own key,
            // which varies per view — so the honest answer is "each
            // view", not one layer's name.
            ColumnDoor::Dataset => Some(FellTo::EachView),
            ColumnDoor::View => {
                let set = |p: &ColumnPresentation| match key {
                    "label" => p.label.is_some(),
                    "width" => p.width.is_some(),
                    _ => false,
                };
                // §5.2: the dataset level sits ABOVE the desk view, so a
                // cleared view key meets it first.
                if set(&layers.dataset) {
                    Some(FellTo::Dataset)
                } else if set(&layers.desk) {
                    Some(FellTo::Desk)
                } else {
                    None
                }
            }
        };
        Some(Fold { key, to })
    }

    /// After a fold, the label and width `Text` fields show what the
    /// column now has (the baseline's value once cleared), so the desk's
    /// or dataset's value reappears on the keystroke that cleared it.
    fn reseed_cleared_texts(&mut self, label: Option<String>, width: Option<f32>) {
        for field in &mut self.fields {
            let FieldKind::Text(text) = &mut field.kind else {
                continue;
            };
            match field.key.as_str() {
                "label" => *text = label.clone().unwrap_or_default(),
                "width" => *text = views::width_text(width),
                _ => {}
            }
        }
    }

    /// Close the column stage (Part 2c §5.2): fold one last time, restore
    /// the object's fields, and leave the cursor on the column's own row.
    ///
    /// The final fold is not belt-and-braces — it is what makes leaving
    /// without having pressed anything since the last change still
    /// correct — and it costs nothing when the item is already up to
    /// date, since the fold writes the same values back.
    ///
    /// `baseline` becomes the restored fields, which says "nothing is
    /// outstanding": every change made in the stage went through
    /// `commit_or_confirm` on its own keystroke, so there is nothing here
    /// for a later comparison to rediscover. (A change the commit
    /// REFUSED — an error diagnostic standing — is the one thing that
    /// baseline forgets; it is already on screen and already blocked, and
    /// the trader's next keystroke on the restored object queues it
    /// again along with whatever they do next.)
    ///
    /// The cursor is resolved after the restore and by NAME, through the
    /// same `visible_rows` index every verb speaks: the column's position
    /// in the list is not the position it had in the seven-row stage, and
    /// an index carried across would land wherever that number happens to
    /// point.
    pub fn leave_column(&mut self) {
        // Checked before anything else touches `parent_fields`: the stash
        // is shared with the Values stage, and `parent_fields.take()`
        // alone cannot tell which stage it belongs to. Calling this while
        // `column` is `None` — the Values stage open, or no stage at all
        // — must touch nothing.
        if self.column.is_none() {
            return;
        }
        self.fold_column();
        let Some(parent) = self.parent_fields.take() else {
            return;
        };
        let column = self.column.take();
        // Dropped with the stage it belongs to: a context left behind
        // would have `fold_column` folding into a door that has closed.
        self.column_ctx = None;
        self.fields = parent;
        self.baseline = self.fields.clone();
        self.query.clear();
        // The stage's own text field can only be open with `escape`
        // claimed by `handle_text_key`, so this cannot be `Some` here;
        // cleared anyway because its `EditRow` indexes the fields being
        // replaced, and a stale one would point into the restored list.
        self.text_entry = None;
        self.selected = 0;
        if let Some(column) = column {
            self.select_item_named(&column);
        }
    }

    /// Open the Values stage (scopes-editing spec §4): stash the object's
    /// fields, install `fields` (the one values list) as a clean
    /// baseline. Refused while any projection is already open, for
    /// `enter_column`'s reason — a second stash would drop the object's
    /// own fields for good.
    pub fn enter_values(&mut self, column: &str, fields: Vec<Field>) -> bool {
        if self.column.is_some() || self.values.is_some() {
            return false;
        }
        self.parent_fields = Some(std::mem::replace(&mut self.fields, fields));
        self.values = Some(column.to_string());
        self.baseline = self.fields.clone();
        self.query.clear();
        self.selected = 0;
        self.text_entry = None;
        true
    }

    /// Close the Values stage: restore the object's fields and hand the
    /// stage's own fields back for the adapter's fold
    /// (`scopes::fold_values` has already run on every tick through
    /// `render::revalidate`; the return is for the caller's final fold
    /// and cursor placement). The restored baseline is the restored
    /// fields, for `leave_column`'s reason. `None` when no Values stage
    /// was open.
    pub fn leave_values(&mut self) -> Option<Vec<Field>> {
        self.values.as_ref()?;
        // A Values stage always carries its own stash — `enter_values` is
        // the only writer of `values` and it always sets `parent_fields`
        // in the same call — so a `Some(values)` with no stash is a bug
        // (a stray `leave_column`, say) rather than a state this door
        // should silently accept, exactly the debug check `fold_column`
        // makes for its own context.
        debug_assert!(
            self.parent_fields.is_some(),
            "a Values stage always carries its own stash"
        );
        let parent = self.parent_fields.take()?;
        self.values = None;
        let own = std::mem::replace(&mut self.fields, parent);
        self.baseline = self.fields.clone();
        self.query.clear();
        self.text_entry = None;
        Some(own)
    }

    /// Put the cursor on the row named `name` — an ordered-list item, or
    /// (the Schema door) the field keyed `columns.<name>` — or on the
    /// first row when neither exists.
    ///
    /// By NAME through the `visible_rows` index every verb speaks, never
    /// by a carried index: the column stage's seven rows and the object's
    /// own list are different lists, so a number carried from one to the
    /// other lands wherever it happens to point. Shared by
    /// [`Draft::leave_column`] and `apply::revert_failed_write`, whose
    /// rebuilt draft has the same problem for the same reason.
    ///
    /// The field fallback is exactly [`Draft::enter_column`]'s own
    /// membership rule read backwards (dataset-presentation spec §4.1):
    /// the Schema door's column rows are `Field`s keyed `columns.<col>`,
    /// not list items, so without it `escape` out of a column stage on a
    /// thirty-column dataset would land the cursor back at the top of the
    /// list rather than on the column just edited. No other domain can
    /// reach it — a view's own list field is keyed `columns`, never
    /// `columns.<something>`.
    pub(in crate::shell::objectdialog) fn select_item_named(&mut self, name: &str) {
        self.selected = 0;
        let target = self
            .fields
            .iter()
            .enumerate()
            .find_map(|(field, f)| {
                let FieldKind::OrderedList { items, .. } = &f.kind else {
                    return None;
                };
                items
                    .iter()
                    .position(|i| i.name == name)
                    .map(|item| EditRow::Item { field, item })
            })
            .or_else(|| {
                let key = format!("columns.{name}");
                self.fields
                    .iter()
                    .position(|f| f.key == key)
                    .map(EditRow::Field)
            });
        if let Some(row) = target {
            self.follow(row);
        }
    }

    /// Replace the object's own fields with a freshly derived set and
    /// treat them as applied (dataset-presentation spec §4.7).
    ///
    /// `render::leave_column_stage` uses it on the Schema door alone: the
    /// column row it returns to carries the dataset overlay's summary in
    /// its text, and the overlay has just changed, so the row must be
    /// re-derived or it would keep painting the summary the stage was
    /// opened with. `baseline` moves with it — these fields describe what
    /// is already on the batch, so leaving the old baseline behind would
    /// make the restored stage read as dirty and queue a `datasets` write
    /// nobody asked for.
    pub(in crate::shell::objectdialog) fn reseed_fields(&mut self, fields: Vec<Field>) {
        self.fields = fields;
        self.baseline = self.fields.clone();
    }

    /// Mutate one of the STASHED parent's ordered lists while a projection
    /// is open — the Values stage's fold writes the scope's `dimensions`
    /// list through here. A no-op when no stash or no such list exists.
    pub fn with_parent_list(
        &mut self,
        key: &str,
        f: impl FnOnce(&mut Vec<ListItem>, &mut Option<Vec<ListItem>>),
    ) {
        let Some(parent) = self.parent_fields.as_mut() else {
            return;
        };
        let Some(field) = parent.iter_mut().find(|fld| fld.key == key) else {
            return;
        };
        if let FieldKind::OrderedList { items, available } = &mut field.kind {
            f(items, available);
        }
    }

    /// The field keyed `key` — the installed ones first, then the
    /// object's own if [`Stage::Column`] has them stashed
    /// ([`Draft::parent_fields`]).
    ///
    /// The one lookup [`Draft::list_items`], [`Draft::available_items`]
    /// and [`Draft::choice`] all go through, so the fallback is a
    /// property of "reading a field by name" rather than something three
    /// call sites remember. Nothing shadows: the column stage's seven
    /// keys (`views::COLUMN_KEYS`) and a view's own two (`dataset`,
    /// `columns`) are disjoint, so the installed-first order only ever
    /// decides between a key and its absence.
    fn field_by_key(&self, key: &str) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.key == key)
            .or_else(|| self.parent_fields.as_ref()?.iter().find(|f| f.key == key))
    }

    /// The object's own items of the ordered-list field named `key` —
    /// what is written, counted and reorderable.
    ///
    /// Read through [`Draft::field_by_key`], which falls back to the
    /// object's stashed fields, and that fallback is load-bearing rather
    /// than tidy: the column stage (Part 2c §5.2) swaps `fields` out for
    /// one column's seven, while the write path still renders the WHOLE
    /// object on every keystroke — `views::presentation_table` and
    /// `views::doc_table` both read this — and the item being folded into
    /// lives in that stashed list. Without the fallback, a keystroke in
    /// the column stage would render a view with no columns at all.
    pub fn list_items(&self, key: &str) -> Option<&[ListItem]> {
        self.field_by_key(key).and_then(|f| match &f.kind {
            FieldKind::OrderedList { items, .. } => Some(items.as_slice()),
            _ => None,
        })
    }

    /// The catalogue of what may JOIN the ordered-list field named `key`
    /// — `None` where no catalogue exists at all (Groupings' `dimensions`,
    /// where ticking is membership; a key naming no ordered list), and
    /// `Some`, possibly empty, where one does. The two answers are
    /// different facts and callers act on them differently
    /// ([`FieldKind::OrderedList`]'s own doc), so this deliberately does
    /// not flatten them into one empty slice.
    pub fn available_items(&self, key: &str) -> Option<&[ListItem]> {
        self.field_by_key(key).and_then(|f| match &f.kind {
            FieldKind::OrderedList { available, .. } => available.as_deref(),
            _ => None,
        })
    }

    /// The selected option of the `Choice` field named `key`.
    pub fn choice(&self, key: &str) -> Option<&str> {
        self.field_by_key(key).and_then(|f| match &f.kind {
            FieldKind::Choice { options, selected } => options.get(*selected).map(String::as_str),
            _ => None,
        })
    }

    /// `space`: change the value under the cursor forward, recording it
    /// in the draft. `false` when the row has no value `space` can
    /// change, which the caller turns into a notice — a key that appears
    /// inert is the defect class this interaction model exists to
    /// remove.
    pub fn toggle_selected(&mut self) -> Step {
        self.step_selected(StepDirection::Forward)
    }

    /// `shift+space`: change the value under the cursor backward — the
    /// reverse twin of [`Draft::toggle_selected`]. Both share
    /// [`Draft::step_selected`] rather than carrying two near-identical
    /// copies of the same match, which is the failure this codebase keeps
    /// hitting (a fix applied to one copy and not the other).
    pub fn toggle_selected_back(&mut self) -> Step {
        self.step_selected(StepDirection::Backward)
    }

    /// Is the open text field the chain field — the one with a completion
    /// list below it? `false` when no field is open at all.
    pub fn chain_entry(&self) -> bool {
        self.text_entry.is_some_and(|entry| entry.completions)
    }

    /// The write half of the query mirror for this draft —
    /// `ObjectDialogState::set_query` routes an edit- or column-stage
    /// keystroke here. A filter keystroke resets the cursor to the top
    /// match, because the list just re-ranked and the old index points at
    /// an unrelated row. An open PLAIN text field is not a filter: its rows
    /// stay unfiltered with the edited row highlighted (§19.1), so the
    /// cursor stays on that row. The chain field's rows ARE its completions
    /// and keep the reset (§18.8). Found on a display 2026-09-13: every
    /// keystroke after `i` sent the highlight back to the first row.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        match self.text_entry {
            Some(TextEntry {
                row: field,
                completions: false,
            }) => self.follow(field),
            _ => self.selected = 0,
        }
    }

    /// `i` on a `Text` or `Number` row (§19.1): open the field seeded with
    /// the row's value, so appending is one keystroke away. Pure — the
    /// mode switch that hands the shared `Input` the keys is the
    /// handler's, and the sync writes the seed into the field on its
    /// return. `Step::Inert` off any other row: the caller says which
    /// verb (if any) that row has.
    ///
    /// Whether a `Text` row is *editable* is the domain's call
    /// (`Domain::text_editable`), checked by the caller before this —
    /// Groupings' `slot` and Scopes' two summaries are `Text` rows that
    /// must stay read-only, and the draft has no domain to ask.
    ///
    /// `follow(row)` after setting `text_entry`, not before: once the
    /// field is open, [`Draft::visible_rows`] answers with every row
    /// unfiltered rather than the query-filtered list `selected` indexed
    /// a moment ago, so `row`'s position there can differ from
    /// `self.selected`'s old value — that is exactly what leaves the
    /// edited row unhighlighted if this is skipped.
    ///
    /// The `Step::Changed` this returns means only that the field
    /// OPENED, never that a value changed — nothing routes it into
    /// [`Self::revalidate`] or `apply::commit_or_confirm`, which read a
    /// step from a tick or a text commit, not from opening the field
    /// that will produce one.
    pub fn begin_text_entry(&mut self) -> Step {
        let Some(row @ EditRow::Field(index)) = self.selected_row() else {
            return Step::Inert;
        };
        let seed = match &self.fields[index].kind {
            FieldKind::Text(text) => text.clone(),
            FieldKind::Number { value, .. } => value.to_string(),
            _ => return Step::Inert,
        };
        self.query = seed;
        self.text_entry = Some(TextEntry {
            row,
            completions: false,
        });
        self.follow(row);
        Step::Changed
    }

    /// `escape` in an open text field: drop the text and close it. The
    /// value is exactly as it was — nothing here was applied. The chain
    /// field's cancel is this same function (`groupings.rs` re-exports
    /// it under its old name).
    ///
    /// A plain field leaves `selected` on the row it was editing
    /// (`follow(row)`, against the now-unfiltered list `text_entry`
    /// being cleared restores) — the same place [`Draft::apply_text_entry`]
    /// leaves it, so cancelling and applying agree about where the
    /// cursor ends up. The chain field keeps its own `selected = 0`: its
    /// rows go from completions to the full edit-row list, a change
    /// nothing sensible to "follow" survives.
    pub fn cancel_text_entry(&mut self) {
        let entry = self.text_entry.take();
        self.query.clear();
        match entry {
            Some(TextEntry {
                completions: false,
                row,
            }) => self.follow(row),
            _ => self.selected = 0,
        }
    }

    /// `enter` in a plain text field: the typed text becomes the row's
    /// value and the field closes. A `Number` parses in here — its rule
    /// is the kind's own (a whole number inside `min..=max`, refused
    /// rather than clamped, because a clamp would apply a number the
    /// trader did not type). A `Text` goes through `parse_text` — the
    /// adapter's door, `(key, text) -> Result<normalised, reason>` — so
    /// a duration or a regex is refused with the field still open, the
    /// chain field's own rule for a bad chain. The same value typed
    /// back is [`Step::Inert`] and still closes the field: closing is
    /// the visible answer.
    ///
    /// `selected` is kept, not reset to 0: the rows below never changed
    /// (they are the edit rows, not a completion list), so the cursor
    /// stays on the row just edited.
    pub fn apply_text_entry(
        &mut self,
        parse_text: &dyn Fn(&str, &str) -> Result<String, String>,
    ) -> Step {
        let Some(TextEntry {
            row: EditRow::Field(index),
            completions: false,
        }) = self.text_entry
        else {
            return Step::Inert;
        };
        let typed = self.query.trim().to_string();
        let field = &mut self.fields[index];
        let label = field.label.clone();
        let outcome = match &mut field.kind {
            FieldKind::Number {
                value, min, max, ..
            } => match typed.parse::<i64>() {
                Err(_) => return Step::Refused(format!("{label} must be a whole number")),
                Ok(n) if n < *min || n > *max => {
                    return Step::Refused(format!("{label} must be between {min} and {max}"));
                }
                Ok(n) if n == *value => Step::Inert,
                Ok(n) => {
                    *value = n;
                    Step::Changed
                }
            },
            FieldKind::Text(text) => match parse_text(&field.key, &typed) {
                Err(reason) => return Step::Refused(reason),
                Ok(parsed) if parsed == *text => Step::Inert,
                Ok(parsed) => {
                    *text = parsed;
                    Step::Changed
                }
            },
            _ => Step::Inert,
        };
        self.text_entry = None;
        self.query.clear();
        outcome
    }

    /// Shared body of [`Draft::toggle_selected`] and
    /// [`Draft::toggle_selected_back`]: change the value under the
    /// cursor one step in `direction`, recording it in the draft.
    /// [`Step::Inert`] when the row has no value to change, or when the
    /// step would be a no-op (a `Number` already at the end `direction`
    /// points toward).
    ///
    /// **One step is refused rather than inert: emptying a
    /// [`Destination::Doc`] list.** Absence of a user-layer key in a
    /// domain's own doc means *inherit the layer beneath*, so an object
    /// rendered empty there cannot be written as "empty" and cannot be
    /// written as an absence either (`apply::object_value` has the full
    /// statement of that asymmetry). `GroupingSlots::set` refuses an
    /// empty chain and `GroupingSlots::from_doc` warns "slot N is empty;
    /// ignored", so the state is not representable in the model at all —
    /// and a state the model cannot hold must not be reachable by
    /// keystroke. Unticking a slot's last dimension is therefore declined
    /// here, at the one place both `space` and `shift+space` pass
    /// through, with `render::refuse_step` naming the verb (`d`/`r`) that
    /// does what the trader meant.
    ///
    /// An overlay list (`Destination::Presentation` — Views' `columns`)
    /// has no such rule: hiding every column of a view is a perfectly
    /// representable personalisation, and an empty overlay rendering IS
    /// an absence.
    fn step_selected(&mut self, direction: StepDirection) -> Step {
        let Some(row) = self.selected_row() else {
            return Step::Inert;
        };
        match row {
            EditRow::Field(i) => {
                let label = self.fields[i].label.clone();
                match &mut self.fields[i].kind {
                    // A bool has only two values, so either direction is the
                    // same flip.
                    FieldKind::Bool(b) => {
                        *b = !*b;
                        Step::Changed
                    }
                    // Steps and wraps in both directions, which is what
                    // makes the option just behind the current one reachable
                    // in one key rather than the long way round —
                    // `settings_view::step`'s own forward behaviour, mirrored
                    // for `shift+space`.
                    FieldKind::Choice { options, selected } => {
                        if options.len() < 2 {
                            return Step::Inert;
                        }
                        *selected = match direction {
                            StepDirection::Forward => (*selected + 1) % options.len(),
                            StepDirection::Backward => {
                                (*selected + options.len() - 1) % options.len()
                            }
                        };
                        Step::Changed
                    }
                    FieldKind::Number {
                        value,
                        min,
                        max,
                        step,
                        wrap,
                    } => {
                        // A value outside [min, max] can arise entirely
                        // outside this dialog's own bounds — Sources' reader
                        // accepts any positive `stable_mtime` while the
                        // dialog's picker caps display at 100 — and clamping
                        // it here would write a number the trader never
                        // typed. Refuse the step instead (§19.1's
                        // refuse-don't-clamp ruling); `i` still reaches a
                        // value in range.
                        if *value < *min || *value > *max {
                            return Step::Refused(format!(
                                "{label} is {value}, outside {min}–{max} — type a value with i"
                            ));
                        }
                        // `span` is the count of representable values —
                        // `max - min + 1`, not `max - min` — so a value
                        // that steps exactly one span past `max` wraps
                        // back to itself rather than to its neighbour.
                        // `wrap` is what makes the range circular (a hue:
                        // 359 + 15 lands at 14, not clamped at 359);
                        // without it a step past either bound clamps to
                        // that bound instead.
                        let span = *max - *min + 1;
                        let next = match direction {
                            StepDirection::Forward => *value + *step,
                            StepDirection::Backward => *value - *step,
                        };
                        let landed = if *wrap {
                            (next - *min).rem_euclid(span) + *min
                        } else if next > *max {
                            *max
                        } else if next < *min {
                            *min
                        } else {
                            next
                        };
                        if landed == *value {
                            return Step::Inert;
                        }
                        *value = landed;
                        Step::Changed
                    }
                    // See `FieldKind`: `Text` is `i`'s and `MultiChoice`
                    // needs a per-option row, neither of which Views has.
                    // The `OrderedList` header row itself has no value —
                    // its items, on the rows below, do.
                    FieldKind::Text(_)
                    | FieldKind::MultiChoice { .. }
                    | FieldKind::OrderedList { .. } => Step::Inert,
                }
            }
            EditRow::Available { field, item } => {
                let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind
                else {
                    return Step::Inert;
                };
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                if item >= available.len() {
                    return Step::Inert;
                }
                // Adding: out of the catalogue, onto the END of the
                // object's own list, shown.
                let mut entry = available.remove(item);
                entry.included = true;
                items.push(entry);
                // The cursor does NOT follow the item into the object's
                // own list: a trader adding several columns wants it on
                // the next available row, where their eye already is
                // (user ruling 2026-09-11). The added item moved
                // *earlier* in row order and its label is unchanged,
                // so the rows ahead of the next visible one are the
                // same set, merely reordered — its visible index is
                // the old cursor plus one. When the added item was the
                // catalogue's last row there is no next, and the same
                // index now holds the row that preceded it (the
                // previous available column, or the object's own last
                // item when there is none left), which is where the
                // cursor stays rather than running off the end.
                let last = self.visible_rows().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                Step::Changed
            }
            EditRow::Item { field, item } => {
                // Read off the field before the list is borrowed mutably:
                // the destination is what decides whether emptying this
                // list is representable at all (see this function's doc).
                let dest = self.fields[field].dest;
                let label = self.fields[field].label.clone();
                let FieldKind::OrderedList { items, .. } = &mut self.fields[field].kind else {
                    return Step::Inert;
                };
                let Some(included) = items.get(item).map(|entry| entry.included) else {
                    return Step::Inert;
                };
                // The Values stage may empty its list — that is "drop this
                // dimension" (scopes-editing spec §4), folded by the
                // adapter; the guard is a Groupings/Views rule.
                if included
                    && dest == Destination::Doc
                    && self.values.is_none()
                    && items.iter().filter(|i| i.included).count() == 1
                {
                    return Step::Refused(format!("{label} must keep at least one entry"));
                }
                // Inclusion is binary too, so both directions flip it.
                items[item].included = !included;
                Step::Changed
            }
        }
    }

    /// The payload a list row drags (§18.9.1); `None` for a field row,
    /// which is neither a drag source nor a drop target.
    pub fn row_drag(&self, row: EditRow) -> Option<RowDrag> {
        let (field, own, item) = match row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return None,
        };
        let FieldKind::OrderedList { items, available } = &self.fields[field].kind else {
            return None;
        };
        let list = if own {
            items.as_slice()
        } else {
            available.as_deref()?
        };
        Some(RowDrag {
            field: self.fields[field].key.clone(),
            own,
            name: list.get(item)?.name.clone(),
        })
    }

    /// Resolve a payload back to a row by name, as the draft stands NOW.
    /// `None` when the field is not a list, the block does not exist
    /// (`own == false` on a catalogue-less list) or the name has left it.
    pub fn locate(&self, drag: &RowDrag) -> Option<EditRow> {
        let field = self.fields.iter().position(|f| f.key == drag.field)?;
        let FieldKind::OrderedList { items, available } = &self.fields[field].kind else {
            return None;
        };
        let list = if drag.own {
            items.as_slice()
        } else {
            available.as_deref()?
        };
        let item = list.iter().position(|i| i.name == drag.name)?;
        Some(if drag.own {
            EditRow::Item { field, item }
        } else {
            EditRow::Available { field, item }
        })
    }

    /// A drop (§18.9.3): `src` takes `dst`'s index. One method decides
    /// every case, and it is the only place they are enumerated:
    ///
    /// - Item → Item: reorder (`remove(src)`, `insert(dst_index)`), so
    ///   downward lands after the target's old position, upward before.
    /// - Available → Item: add at index — `space`'s add, placed rather
    ///   than appended; `included = true`.
    /// - Item → Available: remove, the same act as `x`; the catalogue is
    ///   unordered so the target index is ignored.
    /// - Available → Available: `Inert` — the catalogue has no order.
    /// - Same row, different fields, or a name that no longer resolves:
    ///   `Inert`, nothing written.
    ///
    /// After a change the cursor follows the dropped item, so the next
    /// keystroke acts on the thing the trader just placed (unlike `space`
    /// and `x`, whose cursor rulings are about a *run* of adds/removes).
    ///
    /// A drop can never reach `remove_selected`'s own
    /// `Step::Refused("space unticks here")` — that wording fires when a
    /// trader asks to demote a row on a catalogue-less list, but there is
    /// no available row to drop such a demotion *onto* in the first
    /// place: `dst` claiming a catalogue that does not exist fails to
    /// `locate` and the whole drop is `Inert` before the per-case match
    /// below ever runs. The `(true, false)` **and** `(false, true)` arms'
    /// own `available.as_mut()` guards are therefore unreachable in
    /// practice — safety nets, not a second path to that refusal:
    /// whichever of `src`/`dst` resolved to an `Available` row already
    /// proved `available` was `Some` inside `locate`.
    pub fn drop_row(&mut self, src: &RowDrag, dst: &RowDrag) -> Step {
        let (Some(src_row), Some(dst_row)) = (self.locate(src), self.locate(dst)) else {
            return Step::Inert;
        };
        if src_row == dst_row {
            return Step::Inert;
        }
        let (field, src_own, src_ix) = match src_row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return Step::Inert,
        };
        let (dst_field, dst_own, dst_ix) = match dst_row {
            EditRow::Item { field, item } => (field, true, item),
            EditRow::Available { field, item } => (field, false, item),
            EditRow::Field(_) => return Step::Inert,
        };
        if field != dst_field {
            return Step::Inert;
        }
        let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind else {
            return Step::Inert;
        };
        let landed = match (src_own, dst_own) {
            (true, true) => {
                let entry = items.remove(src_ix);
                items.insert(dst_ix, entry);
                EditRow::Item {
                    field,
                    item: dst_ix,
                }
            }
            (false, true) => {
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                let mut entry = available.remove(src_ix);
                entry.included = true;
                items.insert(dst_ix, entry);
                EditRow::Item {
                    field,
                    item: dst_ix,
                }
            }
            (true, false) => {
                let Some(available) = available.as_mut() else {
                    return Step::Inert;
                };
                let mut entry = items.remove(src_ix);
                entry.included = false;
                available.push(entry);
                EditRow::Available {
                    field,
                    item: available.len() - 1,
                }
            }
            (false, false) => return Step::Inert,
        };
        self.follow(landed);
        Step::Changed
    }

    /// `shift+j` / `shift+k`: move the item under the cursor past the next
    /// VISIBLE item in that direction within the object's own list
    /// (§18.3) — under a filter that is what reordering means, and the
    /// count of hidden rows jumped over is returned so the notice can say
    /// so. `None` at either end of that list, and on a row that is not
    /// one of its items.
    ///
    /// An [`EditRow::Available`] row is one of those: the catalogue is
    /// unordered by construction (§18.7.2), so there is no order there to
    /// change and a "move" would be painted and never written. It is
    /// declined here rather than represented — which is also why the
    /// object's own last item has nowhere further down to go, even with a
    /// catalogue painted below it.
    pub fn move_item(&mut self, delta: i32) -> Option<usize> {
        let (field, item) = match self.selected_row()? {
            EditRow::Item { field, item } => (field, item),
            // Said, rather than left to a wildcard: an available row's
            // index means a position in the CATALOGUE, so reading it as
            // one in `items` would reorder a different column entirely.
            EditRow::Available { .. } | EditRow::Field(_) => return None,
        };
        let visible: BTreeSet<usize> = self.visible_rows().iter().map(|m| m.row).collect();
        let rows = self.rows();
        let row_of = |i: usize| {
            rows.iter()
                .position(|r| *r == EditRow::Item { field, item: i })
        };
        let FieldKind::OrderedList { items, .. } = &mut self.fields[field].kind else {
            return None;
        };
        let mut target = item;
        let mut skipped = 0usize;
        loop {
            let next = target.checked_add_signed(delta as isize)?;
            if next >= items.len() {
                return None;
            }
            target = next;
            if row_of(target).is_some_and(|r| visible.contains(&r)) {
                break;
            }
            skipped += 1;
        }
        let entry = items.remove(item);
        items.insert(target, entry);
        self.follow(EditRow::Item {
            field,
            item: target,
        });
        Some(skipped)
    }

    /// `x`: take the item under the cursor out of the object (§18.2) —
    /// the definitional twin of `space`'s hide.
    ///
    /// Whether a list has a separate membership at all is whether it HAS
    /// a catalogue (§18.7.2), never whether that catalogue happens to be
    /// empty right now, and never a `dest`. A Views draft with every
    /// available column already added still has one — an empty one — so
    /// `x` still removes there; a Groupings `dimensions` list has none at
    /// all (ticking IS membership) and refuses. Deciding by emptiness
    /// (which is what scanning one flat list's flags amounted to, in the
    /// build before this one) made `x` go dead on that first case: the moment a
    /// trader added the view's last available column, the list looked
    /// catalogue-less and `x` started refusing a removal it had done a
    /// keystroke earlier.
    ///
    /// [`Step::Refused`] rather than [`Step::Inert`] for both declined
    /// cases, so the footer says why: on a catalogue-less list, `space` is
    /// the verb that already unticks; on an available row, `space` is the
    /// verb that adds it — `x` has nothing to remove from a row that is
    /// not there yet. Neither reason names `d`/`r` the way `space`'s own
    /// "must keep at least one entry" refusal does, so `render` routes
    /// these straight to the footer rather than through `refuse_step`.
    pub fn remove_selected(&mut self) -> Step {
        let (field, item) = match self.selected_row() {
            Some(EditRow::Item { field, item }) => (field, item),
            Some(EditRow::Available { .. }) => {
                return Step::Refused("not in the view — space adds it".to_string());
            }
            _ => return Step::Inert,
        };
        let FieldKind::OrderedList { items, available } = &mut self.fields[field].kind else {
            return Step::Inert;
        };
        let Some(available) = available.as_mut() else {
            return Step::Refused("space unticks here".to_string());
        };
        if item >= items.len() {
            return Step::Inert;
        }
        let mut entry = items.remove(item);
        entry.included = false;
        // A note is a property of the SELECTION (Scopes' own values, the
        // Values stage's counts) — `fold_values`'s demotion arm clears it
        // the same way when a tick empties a selection, and every other
        // adapter's note is already `None`, so this is a no-op for them.
        // Left set, a dropped dimension's available row would keep
        // painting its old values after `x`.
        entry.note = None;
        available.push(entry);
        let last = available.len() - 1;
        // The cursor does NOT follow the item to the end of the
        // catalogue, for the same reason `space`'s add leaves it behind
        // (user ruling 2026-09-11): a trader removing several columns
        // wants it on the row that was next. The removed item moved
        // *later* in row order with its label unchanged, so the visible
        // rows ahead of the next one lost exactly one — the next row now
        // sits at the old index and `selected` is already right. The one
        // exception is a removal with nothing visible after it: the item
        // lands at the end, which is where it already was, so the old
        // index would still be on it — step back to the previous row
        // instead, the way `dd` on a buffer's last line does.
        let moved = EditRow::Available { field, item: last };
        let rows = self.rows();
        let under_cursor = self
            .visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied());
        if under_cursor == Some(moved) {
            self.selected = self.selected.saturating_sub(1);
        }
        Step::Changed
    }

    /// Which files this draft's changes have to be written to, and which
    /// field keys sent them there — the grouping a flush turns into one
    /// `config_write::edit` call per destination, never a write per field.
    ///
    /// A clean field contributes nothing, which is what keeps a
    /// presentation-only edit out of `views.toml` entirely.
    ///
    /// An ordered list contributes to its own destination **and** to
    /// [`Destination::Doc`] when its item *names* change, because which
    /// entries an object HAS is definitional however the list is
    /// presented:
    /// adding a column the view did not have changes the view, not its
    /// presentation (spec §8.1). Reordering, hiding and resizing never
    /// reach that branch, which is the split this whole design exists for.
    pub fn writes_by_destination(&self) -> BTreeMap<Destination, Vec<String>> {
        let mut out: BTreeMap<Destination, Vec<String>> = BTreeMap::new();
        for (i, field) in self.fields.iter().enumerate() {
            let before = self.baseline.get(i);
            if before == Some(field) {
                continue;
            }
            out.entry(field.dest).or_default().push(field.key.clone());
            if field.dest != Destination::Doc && membership_changed(before, field) {
                out.entry(Destination::Doc)
                    .or_default()
                    .push(field.key.clone());
            }
        }
        // `source` underlies only `Destination::Doc` renderings
        // (`views::doc_table`, `scopes::to_table`) — nothing presentation-
        // destined ever reads it — so a `source` change no field noticed
        // (see `Draft::baseline_source`'s own doc) still has to reach the
        // batch as a `Doc` write, with no field key of its own to name.
        if self.source != self.baseline_source {
            out.entry(Destination::Doc).or_default();
        }
        out
    }

    /// Accept the draft as applied: the current fields become the
    /// baseline, so the next keystroke's difference is that keystroke's
    /// alone.
    ///
    /// Called when the keystroke is **recorded** — before the flush
    /// merges, applies or writes anything — because the batch carries the
    /// whole rendered object rather than a delta, so the baseline's only
    /// job is to keep the next keystroke's difference to itself. A failed
    /// write is the one path that moves the baseline backwards:
    /// `apply::revert_failed_write` rebuilds the draft from the reverted
    /// config, baseline and all.
    pub fn mark_saved(&mut self) {
        self.baseline = self.fields.clone();
        self.baseline_source = self.source.clone();
    }

    /// A draft for an object nothing defines yet. Both baselines are
    /// EMPTY, so every field reads as changed and `writes_by_destination`
    /// names every destination — which is why `apply::commit_create`
    /// builds its own single Doc edit rather than calling `edits_for`: a
    /// new view's untouched column list would otherwise also queue an
    /// empty presentation write.
    pub fn new_object(name: &str, fields: Vec<Field>, source: toml::Table) -> Draft {
        Draft {
            name: name.to_string(),
            is_new: true,
            fields,
            source,
            baseline: Vec::new(),
            baseline_source: toml::Table::new(),
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            text_entry: None,
            parent_fields: None,
            column: None,
            values: None,
            column_ctx: None,
            // An object nothing defines yet has no columns for a dataset
            // to speak for; `Domain::draft` is where the layer arrives.
            dataset_layer: BTreeMap::new(),
        }
    }

    /// The row a reader's diagnostic path names (§19.5), or `None` for a
    /// path that is not this object's or names no row — those stay on the
    /// header. The grammar is `<doc>.<object>.<field>[.<index>[...]]`: a
    /// field matches by `key`; a list field's next segment, when it is an
    /// index into the array, resolves through [`Self::resolve_list_index`]
    /// to the item that array position currently names (never an
    /// available row — the object has no diagnostic about a column it
    /// does not have).
    ///
    /// **In the column stage (Part 2c §5.5) the grammar is narrower**,
    /// because the rows are one column's format keys rather than the
    /// object's fields: the path must be `columns.<i>.[format.]<key>`,
    /// its index must resolve — by name, the same 2b rule — to the OPEN
    /// column, and `<key>` must be one of the installed fields. A path
    /// naming another column lands nowhere while this stage is open (its
    /// row is not on screen to carry the glyph), and so does a path
    /// naming the object itself (`views.tree.dataset`); both stay on the
    /// header, where every diagnostic's text is of record regardless.
    /// The `format.` segment is optional because `label` and `width` sit
    /// on the column's own table while the other five sit under its
    /// `format` sub-table — one grammar, both spellings.
    pub fn row_for_path(&self, doc: &str, path: &str) -> Option<EditRow> {
        let rest = path.strip_prefix(&format!("{doc}.{}.", self.name))?;
        if let Some(column) = self.column.as_deref() {
            let rest = rest.strip_prefix("columns.")?;
            let (raw_index, after) = rest.split_once('.')?;
            let raw_index = raw_index.parse::<usize>().ok()?;
            let items = match &self.field_by_key("columns")?.kind {
                FieldKind::OrderedList { items, .. } => items,
                _ => return None,
            };
            let resolved = self.resolve_list_index("columns", raw_index, items)?;
            let resolved_name = items.get(resolved)?.name.as_str();
            if resolved_name != column {
                return None;
            }
            let key = after.strip_prefix("format.").unwrap_or(after);
            return self
                .fields
                .iter()
                .position(|field| field.key == key)
                .map(EditRow::Field);
        }
        self.fields.iter().enumerate().find_map(|(i, field)| {
            let after = if rest == field.key {
                ""
            } else {
                rest.strip_prefix(&format!("{}.", field.key))?
            };
            let raw_index = after
                .split('.')
                .next()
                .and_then(|s| s.parse::<usize>().ok());
            match (&field.kind, raw_index) {
                (FieldKind::OrderedList { items, .. }, Some(raw_index)) => {
                    let item = self
                        .resolve_list_index(&field.key, raw_index, items)
                        .unwrap_or(raw_index);
                    if item < items.len() {
                        Some(EditRow::Item { field: i, item })
                    } else {
                        Some(EditRow::Field(i))
                    }
                }
                _ => Some(EditRow::Field(i)),
            }
        })
    }

    /// A reader's diagnostic index is the position in `Draft::source`'s
    /// OWN array for this field — for Views that is `views.toml`'s
    /// definitional column order (`views::columns_for` reads
    /// `draft.source["columns"]` the same way to write the file back),
    /// which is unrelated to `items`' order once `ViewPresentation::
    /// apply` has permuted it by a trader's personal drag order, or once
    /// the source has since dropped a column `items` still remembers
    /// (review round 1's Important-2 finding: without this indirection,
    /// a reordered or shortened presentation makes `row_for_path` flag
    /// the wrong column entirely). Resolved by NAME — `source[key][raw_
    /// index]`'s own `name`, whether that entry is a table (Views'
    /// `[[columns]]`) or a bare string (a hypothetical future list shaped
    /// like Groupings' own array-of-strings, `dimensions` in this crate,
    /// though that field's diagnostics never carry an index today: see
    /// `groupings.rs`'s own doc on why) — found in `items` by NAME, never
    /// by position, since `items`' order is exactly what may have moved.
    /// `None` when `source[key]` has no entry at that index, or that
    /// entry's name is no longer among `items` at all; the caller falls
    /// back to the raw index in either case (harmless for Groupings,
    /// whose `source` is always empty — its object is a bare array, not
    /// a table, so `Domain::draft` never populates one).
    fn resolve_list_index(&self, key: &str, index: usize, items: &[ListItem]) -> Option<usize> {
        let entry = self.source.get(key)?.as_array()?.get(index)?;
        let name = match entry {
            toml::Value::Table(t) => t.get("name")?.as_str()?,
            toml::Value::String(s) => s.as_str(),
            _ => return None,
        };
        items.iter().position(|i| i.name == name)
    }

    /// Every row a current diagnostic lands on, with the worst severity
    /// there, for the row glyph; the header list is what carries the
    /// text.
    pub fn flagged_rows(&self, doc: &str) -> Vec<(EditRow, Severity)> {
        let mut out: Vec<(EditRow, Severity)> = Vec::new();
        for d in &self.diagnostics {
            let Some(row) = d.path.as_deref().and_then(|p| self.row_for_path(doc, p)) else {
                continue;
            };
            match out.iter_mut().find(|(r, _)| *r == row) {
                Some((_, s)) if *s == Severity::Warning && d.severity == Severity::Error => {
                    *s = Severity::Error
                }
                Some(_) => {}
                None => out.push((row, d.severity)),
            }
        }
        out
    }
}

/// Did an ordered list's membership — the set of the object's OWN item
/// names, ignoring order and ignoring the available catalogue entirely —
/// change between `before` and `field`? See
/// [`Draft::writes_by_destination`].
///
/// The catalogue is not part of the comparison because it is not part of
/// the object: taking `available`'s names in too would call an add "no
/// membership change" the moment the added name already sat in the
/// catalogue — which is every add there is, since `space` only ever
/// promotes a row already painted — and route it to the overlay
/// destination alone, silently never writing the definitional file.
fn membership_changed(before: Option<&Field>, field: &Field) -> bool {
    let names = |f: &Field| match &f.kind {
        FieldKind::OrderedList { items, .. } => Some(
            items
                .iter()
                .map(|i| i.name.clone())
                .collect::<BTreeSet<_>>(),
        ),
        _ => None,
    };
    match (before.and_then(names), names(field)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

impl Domain {
    /// The one-line explanation of the field keyed `key` on this domain,
    /// painted in the edit footer for the row under the cursor
    /// (2026-09-19, spec §22). Computed at paint from `(domain, stage,
    /// key)` rather than stored on [`Field`], so no constructor has to
    /// carry it and the sentence is data in one table per adapter. A
    /// list item or an available row asks with its LIST's key
    /// ([`Draft::selected_field_key`]), so a column row explains the
    /// columns list.
    ///
    /// **The column stage answers from `views::column_help` whatever the
    /// domain**: Views and Schema open the same seven rows (dataset-
    /// presentation spec §4.1), so one table serves both. Empty for a key
    /// no table knows — the footer keeps the slot and paints nothing —
    /// and `every_field_on_every_domain_has_help` sweeps every adapter's
    /// rows so a new field cannot ship silent.
    ///
    /// Copy rule: the field's MEANING and its value grammar (`30s`,
    /// `k`/`M`, a hue 0..360), never the keys — the hint rows beside it
    /// already say which keys act on the row.
    pub fn help(self, stage: &Stage, key: &str) -> &'static str {
        if matches!(stage, Stage::Column { .. }) {
            return views::column_help(key);
        }
        // Scopes-editing spec §4: the Values stage's one row is always
        // keyed `values`, so this answers the same as the general
        // `Domain::Scopes` arm below would — stated explicitly, ahead of
        // it, so a future domain that grows a Values-shaped stage of its
        // own cannot silently fall through to its OWN `help` table
        // instead.
        if matches!(stage, Stage::Values { .. }) {
            return scopes::help("values");
        }
        match self {
            Domain::Views => views::help(key),
            Domain::Sources => sources::help(key),
            Domain::Groupings => groupings::help(key),
            Domain::Scopes => scopes::help(key),
            Domain::Colours => colours::help(key),
            Domain::Schema => schema::help(key),
        }
    }

    /// May `i` edit the `Text` row keyed `key` on this domain? `false` on
    /// Groupings and Colours — Groupings' `slot` is a display-only
    /// `Text`; Colours has no `Text` row at all (`hue` is a `Number`,
    /// `tone`/`token` are `Choice`), so `i` never reaches this door for
    /// it. Sources (§19.3) was the first `true`, for
    /// `paths`/`poll_interval`/`pending_timeout`/`batch_pattern`
    /// (`sources::text_editable`); Views answers `true` for the column
    /// stage's `label` and `width` (Part 2c §5.3, `views::text_editable`)
    /// and for nothing else it has. **Scopes answers `true` for `text`
    /// and `expression` alone** (2026-09-19, `scopes.rs`'s own module
    /// doc) — its `dimensions` row is an `OrderedList`, not a `Text`, and
    /// `i` never reaches this door for it either.
    ///
    /// **Schema shares the Views answer** (dataset-presentation spec
    /// §4.1): its column stage paints the very same seven rows, so `i`
    /// must open the same two. Its rows OUTSIDE that stage are keyed
    /// `columns.<name>` / `derived.<name>`, which `views::text_editable`
    /// answers `false` for — it matches `label | width` and nothing else
    /// — so routing here does not make one read-only schema row typeable.
    pub fn text_editable(self, key: &str) -> bool {
        match self {
            Domain::Groupings | Domain::Colours => {
                let _ = key;
                false
            }
            Domain::Scopes => matches!(key, "text" | "expression"),
            Domain::Views | Domain::Schema => views::text_editable(key),
            Domain::Sources => sources::text_editable(key),
        }
    }

    /// The adapter's door for a committed `Text` (§19.1): normalise the
    /// typed text, or refuse it with the reason the notice shows. Trims
    /// by default; an adapter with a real grammar (a duration, a regex, a
    /// path list) overrides its own keys — Sources was the first
    /// (`sources::parse_text`), Views the second (the column stage's
    /// `width`, Part 2c §5.3), Scopes the third (`scopes::parse_text`,
    /// which also refuses a broken `expression`).
    pub fn parse_text(self, key: &str, text: &str) -> Result<String, String> {
        match self {
            // Colours joins for the same reason `text_editable` gives
            // it no `true` above: no `Text` row for this door to ever
            // be called on.
            Domain::Groupings | Domain::Colours => {
                let _ = key;
                Ok(text.trim().to_string())
            }
            Domain::Scopes => scopes::parse_text(key, text),
            // Schema joins Views for the reason `text_editable` gives:
            // the two column stages are the same seven rows, so `width`
            // must have the same grammar through either door.
            Domain::Views | Domain::Schema => views::parse_text(key, text),
            Domain::Sources => sources::parse_text(key, text),
        }
    }

    /// The fields of `object`, derived fresh from `config` — never cached,
    /// for the same reason [`Domain::objects`] is not.
    ///
    /// A `match`, unlike [`Domain::objects`]: what an object's fields are
    /// IS the domain-specific part, and there is nothing shared here for
    /// an adapter to diverge from.
    pub fn fields(self, config: &Config, object: Option<&str>) -> Vec<Field> {
        match self {
            Domain::Views => views::fields(config, object),
            Domain::Groupings => groupings::fields(config, object),
            Domain::Scopes => scopes::fields(config, object),
            Domain::Schema => schema::fields(config, object),
            Domain::Sources => sources::fields(config, object),
            Domain::Colours => colours::fields(config, object),
        }
    }

    /// The fields a `c`-copied object opens with, built straight from the
    /// table `create_from_name` just copied rather than from a named
    /// object `config` has a row for yet — the copy has not been written
    /// when this runs (§6). Only [`Domain::duplicable`] needs the real
    /// answer: every other domain falls back to `self.fields(config,
    /// None)`, its own empty-object shape, since nothing else can reach
    /// this door.
    pub fn fields_from_source(self, config: &Config, table: &toml::Table) -> Vec<Field> {
        match self {
            Domain::Scopes => scopes::fields_from_table(config, Some(table)),
            Domain::Views
            | Domain::Groupings
            | Domain::Schema
            | Domain::Sources
            | Domain::Colours => self.fields(config, None),
        }
    }

    /// A fresh [`Draft`] of `object`, validated once so the edit stage
    /// opens showing whatever is already wrong with it.
    pub fn draft(self, config: &Config, object: &str) -> Draft {
        let fields = self.fields(config, Some(object));
        let source = config
            .doc(self.doc())
            .and_then(|doc| doc.value.get(object))
            .and_then(|value| value.as_table())
            .cloned()
            .unwrap_or_default();
        let mut draft = Draft {
            name: object.to_string(),
            // `draft()` sets `is_new: false` in its literal.
            is_new: false,
            baseline: fields.clone(),
            fields,
            baseline_source: source.clone(),
            source,
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            text_entry: None,
            parent_fields: None,
            column: None,
            values: None,
            column_ctx: None,
            // §5.1: Views alone. Reloading the views a second time here
            // (`fields` above already did once) is the price of the
            // scaffold's one-`Domain`-match-per-function rule — the
            // alternative is a `fields` that returns two things, which
            // every other adapter would then have to answer for.
            dataset_layer: match self {
                Domain::Views => views::dataset_layer_for(config, object),
                _ => BTreeMap::new(),
            },
        };
        draft.diagnostics = self.validate(&draft, config);
        draft
    }

    /// The draft `n` opens after a name is committed (§18.2): the
    /// adapter's empty-object fields — `fields(config, None)`, which every
    /// adapter already answers — over an empty source, validated once.
    /// `n` creates an EMPTY object (scopes-editing spec §6, reversing the
    /// earlier "Scopes' `n` saves the frame's scope" behaviour) — `c`'s
    /// copy is a separate path (`create_from_name`'s `NameSeed::CopyOf`
    /// arm, over [`Domain::fields_from_source`]) that replaces this
    /// result's fields and source once the name is committed, so the pure
    /// core still never reads a `Frame`.
    pub fn new_draft(self, config: &Config, name: &str) -> Draft {
        let mut draft = Draft::new_object(name, self.fields(config, None), toml::Table::new());
        draft.diagnostics = self.validate(&draft, config);
        draft
    }

    /// The draft rendered as the item that would be written to `dest`.
    ///
    /// `toml_edit::Item`, not `toml_edit::Table`: every Views field
    /// renders a table, but a grouping slot's own value is a bare array
    /// (`groupings::to_table`'s doc comment has the full story), and a
    /// return type that could only ever be a table would have had no way
    /// to say so.
    pub fn to_table(self, draft: &Draft, dest: Destination) -> toml_edit::Item {
        match self {
            Domain::Views => views::to_table(draft, dest),
            Domain::Groupings => groupings::to_table(draft, dest),
            Domain::Scopes => scopes::to_table(draft, dest),
            Domain::Schema => schema::to_table(draft, dest),
            Domain::Sources => sources::to_table(draft, dest),
            Domain::Colours => colours::to_table(draft, dest),
        }
    }

    /// Everything wrong with the draft as it stands (spec §7.2), run on
    /// every field change, synchronously, with no debounce — it is a
    /// parse of a few hundred bytes.
    pub fn validate(self, draft: &Draft, config: &Config) -> Vec<Diagnostic> {
        match self {
            Domain::Views => views::validate(draft, config),
            Domain::Groupings => groupings::validate(draft, config),
            Domain::Scopes => scopes::validate(draft, config),
            Domain::Schema => schema::validate(draft, config),
            Domain::Sources => sources::validate(draft, config),
            Domain::Colours => colours::validate(draft, config),
        }
    }
}

/// Put `item` in `document` under `name`, replacing whatever was there.
///
/// The one spelling of what a flush does to a file, shared by the write
/// path and by [`object_text`]. Whole-value replacement, never a
/// key-by-key merge: every doc these dialogs edit is atomic at depth one
/// (`config::merge::atomic_depth`), so a stale `hidden` left behind by a
/// merge would be a key nothing in the UI could remove.
///
/// `toml_edit::Item`, not `toml_edit::Table`: every domain but Groupings
/// stores an object as a table (`[tree]` header), but a grouping slot's
/// value is a bare array (`3 = ["book", "lhu"]`), and `DocumentMut`'s own
/// indexing assignment renders either shape correctly — a `Table` gets
/// its header, a `Value` gets `name = value` inline — without this
/// function needing to know which one it was handed.
pub(super) fn set_object(document: &mut toml_edit::DocumentMut, name: &str, item: toml_edit::Item) {
    document[name] = item;
}

/// One object's item as the TOML text a write would produce.
///
/// Goes through a `DocumentMut` rather than `Table::to_string`, which is
/// not the same thing and quietly loses work: a bare table renders only
/// its own key-value pairs, so an array of tables (`[[tree.columns]]`)
/// and a sub-table (`[tree.columns.npv]`) both need the document's
/// header path to appear at all.
pub fn object_text(name: &str, item: toml_edit::Item) -> String {
    let mut document = toml_edit::DocumentMut::new();
    set_object(&mut document, name, item);
    document.to_string()
}

/// One `toml::Table` (what `Config` holds) as a `toml_edit::Table` (what
/// a write produces).
///
/// A hand-written conversion rather than a `to_string`/`parse` round trip
/// so it cannot depend on the serializer's own idea of key ordering, and
/// so an array of tables comes out as an array of tables — `[[x.columns]]`
/// on the way out, which parses back to exactly what went in.
pub(super) fn toml_table_to_edit(table: &toml::Table) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    for (key, value) in table {
        out.insert(key, toml_value_to_item(value));
    }
    out
}

fn toml_value_to_item(value: &toml::Value) -> toml_edit::Item {
    match value {
        toml::Value::Table(t) => toml_edit::Item::Table(toml_table_to_edit(t)),
        // An array whose every element is a table renders as
        // `[[name]]` blocks, which is how these config docs are written
        // by hand and how they read back identically.
        toml::Value::Array(a) if !a.is_empty() && a.iter().all(|v| v.is_table()) => {
            let mut arr = toml_edit::ArrayOfTables::new();
            for element in a {
                if let Some(t) = element.as_table() {
                    arr.push(toml_table_to_edit(t));
                }
            }
            toml_edit::Item::ArrayOfTables(arr)
        }
        other => toml_edit::Item::Value(toml_value_to_value(other)),
    }
}

fn toml_value_to_value(value: &toml::Value) -> toml_edit::Value {
    match value {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        // No config doc these dialogs edit carries a datetime; kept
        // lossless anyway by going through toml_edit's own parser, and
        // degrading to the same text as a string rather than to a
        // silently wrong value if it ever cannot.
        toml::Value::Datetime(d) => match d.to_string().parse::<toml_edit::Datetime>() {
            Ok(dt) => dt.into(),
            Err(_) => d.to_string().into(),
        },
        toml::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for element in a {
                arr.push(toml_value_to_value(element));
            }
            arr.into()
        }
        toml::Value::Table(t) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, v) in t {
                inline.insert(key, toml_value_to_value(v));
            }
            inline.into()
        }
    }
}

/// One open object dialog's pure state — the analogue of
/// `keybindings_view::KeybindingsState`, and stored on `ShellView` the
/// same way, with its `ScrollHandle` in a sibling field rather than in
/// here (see this module's own "Layout" note).
#[derive(Debug)]
pub struct ObjectDialogState {
    pub domain: Domain,
    pub stage: Stage,
    /// Index into the **filtered** list ([`visible_rows`]), not the full
    /// one — the palette's convention, shared by every list surface in
    /// this crate and the `len` bound `vimnav::apply` moves within
    /// (wrapping a bare ±1, clamping a larger or counted step — spec
    /// §20.5).
    pub selected: usize,
    /// The filter query, mirrored here from `ShellView::dialog_input` by
    /// that field's `InputEvent::Change` subscription. The `Input` owns
    /// the text; this is the pure copy the rows are ranked against. It
    /// survives leaving filter mode, because the ladder's first rung
    /// keeps the query applied: leaving a search leaves you on the
    /// match.
    pub query: String,
    /// `Normal` on open — bare letters are verbs, and the shared filter
    /// input is left blurred so they reach [`render::handle_key`] rather
    /// than being typed.
    pub mode: DialogMode,
    /// A one-line report about the keystroke just pressed, painted in the
    /// footer and dropped at the next keystroke or click. Same contract
    /// as `KeybindingsState::notice`: a verb that deliberately did
    /// nothing says so, because a key that appears inert is the defect
    /// class this interaction model exists to remove.
    pub notice: Option<String>,
    /// The object being edited. `None` in [`Stage::Browse`], and the only
    /// state this dialog stores rather than derives — deliberately, since
    /// it is also what the edit stage paints; see [`Draft`].
    pub draft: Option<Draft>,
    /// The destructive keystroke waiting on a second one, if any — the
    /// dialog's ONE open question, in whichever stage it was asked. It
    /// replaces the stage's action bar while armed, so the row list above
    /// it never changes length, and it owns the keys and the mouse until
    /// answered (spec §20.1).
    ///
    /// On the state rather than on [`Draft`] (where it lived until
    /// 2026-09-19) because the browse stage arms it too — `d`/`r` on the
    /// selected row, with no draft in existence — and the keybindings
    /// dialog already keeps its own confirm on its state. What the
    /// question is ABOUT is not stored beside it: `render::target_object`
    /// answers the stage's object in `Edit`/`Column` and the selected
    /// browse row in `Browse`, and neither can move while the question
    /// is open (every other key is claimed and dropped, and a row click
    /// is dropped too) — except a config reload, which is what
    /// [`Self::confirm_target`] exists for. Every stage transition clears
    /// both through [`Self::disarm`].
    pub confirm: Option<Confirm>,
    /// The object [`Self::confirm`] was asked about, as `render::
    /// target_object` resolved it at arming time. Meaningful only while
    /// `confirm` is `Some`, and written only beside it (`render::
    /// arm_confirm`): the answer is carried out only if the target still
    /// resolves to this name, since a reload can re-rank the browse list
    /// under an index cursor (review finding, 2026-09-19).
    pub confirm_target: Option<String>,
    /// The dataset the row under the cursor fed when `n` was pressed
    /// (§19.3, Sources only): the new source's `dataset` seed. Cleared by
    /// [`Self::cancel_naming`] and consumed by
    /// `render::create_from_name`.
    pub naming_dataset: Option<String>,
    /// What [`Stage::Naming`]'s `enter` creates (scopes-editing spec §6):
    /// `Empty` for `n`, `CopyOf(source)` for `c`. Read once, by
    /// `render::create_from_name`, and reset to `Empty` by
    /// [`Self::cancel_naming`] so a later `n` on the same dialog instance
    /// cannot inherit a stale `c`'s target.
    pub naming_seed: NameSeed,
    /// The tag of the latest distinct request the Values stage submitted
    /// (`render::enter_values_stage`); an outcome with any other tag is
    /// stale and dropped (spec §7.3).
    pub values_tag: u64,
}

impl ObjectDialogState {
    /// Hand-written rather than `Default`-derived for the reason
    /// `KeybindingsState`'s own `Default` is: the opening mode is a
    /// per-surface decision and deserves one explicit, greppable line
    /// (`DialogMode` has no `Default` on purpose).
    pub fn new(domain: Domain) -> Self {
        Self {
            domain,
            stage: Stage::Browse,
            selected: 0,
            query: String::new(),
            mode: DialogMode::Normal,
            notice: None,
            draft: None,
            confirm: None,
            confirm_target: None,
            naming_dataset: None,
            naming_seed: NameSeed::Empty,
            values_tag: 0,
        }
    }

    /// Drop the open question, if any, and the target it recorded — the
    /// one place both are cleared, so they cannot drift apart.
    pub fn disarm(&mut self) {
        self.confirm = None;
        self.confirm_target = None;
    }

    /// The **pure half** of entering the edit stage — call
    /// [`render::enter_edit_stage`], never this, from anywhere with a
    /// `Window` in scope.
    ///
    /// It opens `object`'s edit stage, turning the `escape` ladder's
    /// `PreviousStage` rung on by constructing [`Stage::Edit`], and it
    /// sets the two things that decide where the next keystroke goes:
    ///
    /// - the **browse query is dropped**, because the edit stage has its
    ///   OWN filter (`Draft::query`, §18.3) — a separate cursor space
    ///   from the browse list's, since `Draft::selected` indexes the
    ///   draft's own `visible_rows()`, never `ObjectDialogState::
    ///   selected`'s list. A stale browse query left in `state.query`
    ///   would rank nothing here (the edit stage never reads it) while
    ///   still being live enough to eat `escape`'s `ClearQuery` rung the
    ///   moment the trader stepped back to browse — the newly-built
    ///   draft's own `query` is separately guaranteed empty here too,
    ///   defensively, though every constructor already starts it that
    ///   way;
    /// - the **mode is set explicitly**, never inherited, because `enter`
    ///   opens an object from filter mode too, and a stage left in
    ///   `Filter` with no field of its own sends the next `escape` down
    ///   the `LeaveFilter` rung — which the edit handler does not claim,
    ///   so the shell's modal branch closes the whole dialog instead of
    ///   stepping back to the list, without ever asking. `Normal` for
    ///   every domain, Groupings included (user ruling 2026-09-14): a
    ///   slot opens in its chooser, and `i` opens the chain field.
    ///
    /// Both are pure, and that is now the whole transition: the shared
    /// `Input` is emptied and blurred to match by `dialog::
    /// sync_dialog_text` (spec §16.1), which reads the mode and
    /// [`Self::effective_query`] after the handler returns rather than
    /// being hand-written beside each mutation. A mode and a focus that
    /// disagree is the one thing this dialog's "one switch" exists to
    /// prevent — the defect this method's own second half was once fixed
    /// for, and the reason that half has one owner now instead of a
    /// copy at every site. The blur still needs a `Window`, which this
    /// file may not name; what changed is that no caller has to remember
    /// it. This method stays visible only inside this module's subtree
    /// so [`render::enter_edit_stage`] remains the one door onto the
    /// stage change itself.
    pub(in crate::shell::objectdialog) fn enter_edit(&mut self, config: &Config, object: &str) {
        let mut draft = self.domain.draft(config, object);
        draft.query.clear();
        // Every domain opens in normal mode with no field open — a
        // Groupings slot lands in the chooser and `i` opens its chain
        // field (user ruling 2026-09-14, superseding §18.8's 2026-09-12
        // chain-field landing). There is deliberately no domain arm here:
        // the one door every entry goes through has one answer.
        self.draft = Some(draft);
        self.stage = Stage::Edit {
            object: object.to_string(),
        };
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
        self.disarm();
    }

    /// [`Self::enter_edit`]'s twin for a name `n` or `c` has just
    /// committed (§18.2, scopes-editing spec §6): the same stage
    /// transition, over an already-built `draft` rather than one derived
    /// from `config`. A committed name has nothing in `config` to derive
    /// from yet — the write is still on its way through the debounced
    /// flush — and `create_from_name` has already built whatever this
    /// draft should hold (the domain's empty object for `n`, or `c`'s
    /// copied source and fields), which a fresh `domain.draft(config,
    /// name)` call would throw away.
    ///
    /// Visible only inside this subtree for the same reason
    /// [`Self::enter_edit`] is: [`render::enter_edit_stage`] is the one
    /// door onto either, and a caller reaching this directly would skip
    /// the scroll reset and the frame request that door also owns.
    pub(in crate::shell::objectdialog) fn enter_edit_with(&mut self, mut draft: Draft) {
        // Same defensive clear `enter_edit` gives its own freshly-derived
        // draft — see that method's doc.
        draft.query.clear();
        self.stage = Stage::Edit {
            object: draft.name.clone(),
        };
        self.draft = Some(draft);
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
        self.disarm();
    }

    /// Back to the browse list, dropping the draft. `selected` is left to
    /// the caller, which puts it back on the object just edited — by
    /// name, since the unfiltered list is a different list from the one
    /// the object was opened from.
    ///
    /// No mode is set here, and none needs to be: see
    /// [`render::leave_edit`], the one caller, for which modes can
    /// actually stand at this point and why the sync is left to settle
    /// the field and the focus from whichever one does.
    ///
    /// `query` is cleared here too, belt-and-braces: `set_query`'s
    /// one-way mirror (§18.3) should already have kept it empty for the
    /// whole life of the edit stage, but this is the same habit
    /// `cancel_naming` and `begin_naming` already keep of clearing it on
    /// every stage transition, rather than trusting an invariant a future
    /// change to `set_query` could quietly break.
    pub fn leave_edit(&mut self) {
        self.draft = None;
        self.stage = Stage::Browse;
        self.query.clear();
        self.notice = None;
        self.disarm();
    }

    /// Replace the query — the **write half** of the one-way mirror
    /// [`Self::effective_query`] reads back
    /// (the pure half of the `InputEvent::Change` subscription). Resets
    /// the selection to the top match (after an edit the old index
    /// points at an unrelated row) and drops the notice, which named a
    /// row the re-ranked list has just moved the selection off.
    ///
    /// The mirror is **one-way per stage**, never both at once: in
    /// [`Stage::Edit`] the shared `Input` is the edit stage's own filter
    /// (§18.3), so the keystroke belongs to `Draft::query`, and `state.
    /// query` — the browse list's cursor space — must not also change
    /// underneath it; everywhere else (browse, naming) it belongs to
    /// `state.query` as it always did. Writing both, as an earlier build
    /// of this method did, left a stale copy in whichever field the
    /// current stage was NOT reading: the edit stage's own `ClearQuery`
    /// rung only ever clears `Draft::query`, so a query typed while
    /// editing was still sitting in `state.query` after `escape` walked
    /// all the way back to browse — a filtered browse list under an
    /// empty-looking filter field, and a `leave_edit` cursor restore that
    /// silently failed to find the object it was looking for.
    pub fn set_query(&mut self, query: String) {
        // The column stage (Part 2c §5.2) is the edit stage's own filter
        // row over a different set of rows — one draft, one cursor space
        // — so it takes the same side of the mirror.
        if matches!(self.stage, Stage::Edit { .. } | Stage::Column { .. })
            && let Some(draft) = self.draft.as_mut()
        {
            draft.set_query(query);
        } else {
            self.query = query;
            self.selected = 0;
        }
        self.notice = None;
    }

    /// The query the open stage is filtering by — the draft's in
    /// `Stage::Edit`, the state's own otherwise (spec §16.2). The **read
    /// half** of [`Self::set_query`]'s one-way mirror: `dialog::
    /// sync_dialog_text` writes the shared `Input` from this, so a query
    /// left sitting in the other stage's slot can never reach the screen.
    pub fn effective_query(&self) -> &str {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. } | Stage::Column { .. }, Some(draft)) => draft.query.as_str(),
            _ => self.query.as_str(),
        }
    }

    /// The cursor of the open stage, in the same slot rule as
    /// [`Self::effective_query`]: the draft's in the edit and column
    /// stages, the state's own otherwise. What the change subscription
    /// scrolls to after a keystroke — the top for a filter (the reset),
    /// the edited row for an open plain field, which `set_query` keeps.
    pub fn effective_selected(&self) -> usize {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. } | Stage::Column { .. }, Some(draft)) => draft.selected,
            _ => self.selected,
        }
    }

    /// `n`: the browse list stays, the filter row becomes the name field.
    /// The shared `Input` holds the name exactly as it holds a query —
    /// `set_query` mirrors it into `query` — so there is no second text
    /// buffer to keep in step.
    ///
    /// Pure, like every transition here: `DialogMode::Filter` is what
    /// gives the field the keys (through `dialogmode::focus_target`, via
    /// `dialog::sync_dialog_text`), and clearing `query` is what empties
    /// it — the sync writes the field from [`Self::effective_query`], so
    /// a browse filter left standing would be written straight back into
    /// the name field the trader is about to type into.
    pub fn begin_naming(&mut self) {
        self.stage = Stage::Naming;
        self.query.clear();
        self.mode = DialogMode::Filter;
        self.notice = None;
        self.disarm();
    }

    /// `escape` from [`Stage::Naming`]: back to browse, nothing written.
    /// Pure, and complete on its own — `Browse`, `Normal`, an empty
    /// `query` — so `dialog::sync_dialog_text` empties the field and
    /// hands the keys back to the shell root with nothing else to say.
    pub fn cancel_naming(&mut self) {
        self.stage = Stage::Browse;
        self.query.clear();
        self.mode = DialogMode::Normal;
        self.selected = 0;
        self.notice = None;
        self.disarm();
        self.naming_dataset = None;
        self.naming_seed = NameSeed::Empty;
    }

    /// Whether `escape` has a stage to step back into before it closes
    /// the dialog — the third rung of
    /// [`crate::dialogmode::escape_step`]'s ladder, and this scaffold is
    /// its first consumer (the keybinding dialog is one flat list and
    /// always passes `false`).
    ///
    /// It is a predicate over the stage rather than a literal `false` at
    /// the call site, and that is what made the edit stage turn the rung
    /// on by merely existing: `enter_edit` constructs [`Stage::Edit`] and
    /// nothing in `render` had to remember to change. A `false` spelled
    /// at the call site is exactly the shape that gets left behind when a
    /// stage arrives, and the failure is silent — `escape` in the edit
    /// stage would close the whole dialog out from under the object being
    /// edited, instead of going back one stage.
    pub fn has_previous_stage(&self) -> bool {
        matches!(
            self.stage,
            Stage::Edit { .. } | Stage::Naming | Stage::Column { .. } | Stage::Values { .. }
        )
    }
}

/// The text one row exposes to the filter: its painted label
/// ([`ObjectRow::display_name`] — the prefix and all, on a Sources row)
/// and its summary — exactly what the row paints, and nothing more.
/// `keybindings_view`'s own `searchable_text` carries the same rule and
/// the review finding behind it: matching text the user cannot see
/// breaks the agreement between what ranked and what is highlighted.
pub fn searchable_text(row: &ObjectRow) -> String {
    format!("{} {}", row.display_name(), row.summary)
}

/// The rows this dialog currently shows, ranked by
/// [`crate::listfilter::rank`] — derived fresh at every call site
/// (render, key handling, click resolution), never cached, for the same
/// reason [`Domain::objects`] is not.
pub fn visible_rows(
    state: &ObjectDialogState,
    rows: &[ObjectRow],
) -> Vec<crate::listfilter::Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    crate::listfilter::rank(&texts, &state.query)
}

/// Where the row named `clicked` currently sits in the *filtered* list,
/// or `None` if the filter is hiding it. Click handlers stay keyed by
/// object name — identity, never position, so a row survives the list
/// being re-ranked under it — and this is the one place that identity is
/// turned back into the index [`ObjectDialogState::selected`] speaks.
pub fn filtered_position(
    visible: &[crate::listfilter::Ranked],
    rows: &[ObjectRow],
    clicked: &str,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| r.name == clicked))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, Layer, LayerDoc, Severity};

    /// dataset-presentation spec §5.2: a cleared view-level key meets
    /// the DATASET level before the desk view's own, because that is the
    /// order they resolve in — so the notice names the layer the trader
    /// will actually see from here, not the one furthest down.
    ///
    /// All three answers, from one clear over three different sets of
    /// layers: the key cleared is the same in each, and only what sits
    /// below it changes.
    #[test]
    fn a_cleared_view_key_falls_to_the_dataset_level_before_the_desk() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\nlabel = \"NPV\"\n",
            ),
        ]);
        // The stage as the door opens it, with `layers` under it and the
        // label field emptied — the one keystroke every case here makes.
        let open = |layers: ColumnLayers| {
            let mut draft = Domain::Views.draft(&config, "tree");
            let item = draft.list_items("columns").unwrap()[0].clone();
            assert_eq!(
                item.presentation.label.as_deref(),
                Some("NPV"),
                "sanity: the item holds a label, so emptying it is a real clear"
            );
            draft.column_ctx = Some(ColumnContext {
                door: ColumnDoor::View,
                layers,
                overlay_object: toml::Table::new(),
                item: Some(item.clone()),
            });
            assert!(draft.enter_column(
                "npv",
                views::column_fields(&item, &[], Destination::Presentation)
            ));
            for field in &mut draft.fields {
                if field.key == "label" {
                    field.kind = FieldKind::Text(String::new());
                }
            }
            draft
        };
        let labelled = |text: &str| ColumnPresentation {
            label: Some(text.to_string()),
            ..Default::default()
        };

        let mut both = open(ColumnLayers {
            desk: labelled("NPV"),
            dataset: labelled("Δ"),
        });
        assert_eq!(
            both.fold_column(),
            Some(Fold {
                key: "label",
                to: Some(FellTo::Dataset)
            }),
            "the dataset level sits above the desk view, so it is what \
             the cleared key lands on first"
        );

        let mut desk_only = open(ColumnLayers {
            desk: labelled("NPV"),
            ..Default::default()
        });
        assert_eq!(
            desk_only.fold_column(),
            Some(Fold {
                key: "label",
                to: Some(FellTo::Desk)
            })
        );

        let mut neither = open(ColumnLayers::default());
        assert_eq!(
            neither.fold_column(),
            Some(Fold {
                key: "label",
                to: None
            }),
            "the key really was cleared, but nothing below sets it — so \
             the caller has no layer to name and says nothing"
        );
    }

    #[test]
    fn schema_types_only_into_the_column_stages_label_and_width() {
        assert!(Domain::Schema.text_editable("label"));
        assert!(Domain::Schema.text_editable("width"));
        assert!(!Domain::Schema.text_editable("columns.npv"));
        assert!(!Domain::Schema.text_editable("derived.region"));
    }

    #[test]
    fn schema_is_writable_in_the_column_stage_alone() {
        let column = Stage::Column {
            object: "risk".into(),
            column: "npv".into(),
        };
        for stage in [
            Stage::Browse,
            Stage::Naming,
            Stage::Edit {
                object: "risk".into(),
            },
        ] {
            assert!(!Domain::Schema.writable(&stage), "{stage:?}");
            assert!(Domain::Views.writable(&stage), "{stage:?}");
        }
        assert!(Domain::Schema.writable(&column));
        assert!(Domain::Views.writable(&column));
    }

    /// A `Config` assembled from literal per-layer documents, in the
    /// Builtin → Desk → User order `merge_docs` documents its callers
    /// must pass. Everything goes through `ConfigSources::builtin` —
    /// that field is a `Vec<LayerDoc>` whose entries carry their OWN
    /// `layer`, and `Config::load` pushes them into the layered map in
    /// slice order without re-stamping it, so this reproduces a
    /// three-layer config exactly while staying a pure unit test (the
    /// desk and user fields are directories, and reading them would put
    /// filesystem I/O in the pure core's own tests).
    fn config_from(docs: &[(Layer, &str, &str)]) -> Config {
        let layered: Vec<LayerDoc> = docs
            .iter()
            .map(|(layer, name, text)| LayerDoc {
                layer: *layer,
                name: (*name).to_string(),
                file: std::path::PathBuf::from(format!("<test:{}:{name}>", layer.name())),
                table: text.parse().expect("fixture TOML parses"),
            })
            .collect();
        Config::load(&ConfigSources {
            builtin: layered,
            desk: None,
            user: None,
        })
    }

    /// A `tree` view over a `risk` dataset with two member columns — `npv`
    /// (a measure) and `book` (a dimension) — and nothing left in the
    /// available catalogue.
    ///
    /// The column stage's own fixture (Part 2c §5): it needs a REAL
    /// dataset doc, unlike most tests here, because the `dataset` choice
    /// is what proves the parent's fields are still reachable through
    /// [`Draft::field_by_key`] while the stage has swapped `fields` out,
    /// and a second member is what proves a path naming the OTHER column
    /// lands nowhere (§5.5).
    fn config_with_view_and_datasets() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n\
                 [[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n",
            ),
        ])
    }

    #[test]
    fn groupings_always_lists_nine_slots_and_an_unconfigured_one_has_no_layer() {
        let config = config_from(&[(Layer::Desk, "groupings", "3 = [\"book\"]\n")]);
        let rows = Domain::Groupings.objects(&config);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
        let three = &rows[2];
        assert_eq!(three.layer, Some(Layer::Desk));
        assert_eq!(three.summary, "book");
        let one = &rows[0];
        assert_eq!(one.layer, None, "nothing defines slot 1");
        assert_eq!(one.summary, "empty");
        assert!(!one.overridden);
    }

    #[test]
    fn views_have_no_roster_so_a_config_with_no_views_lists_nothing() {
        let config = config_from(&[]);
        assert!(Domain::Views.objects(&config).is_empty());
    }

    #[test]
    fn a_row_carries_the_layer_that_won_and_marks_a_user_override() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "views",
                "[tree]\ndataset = \"risk\"\n[wide]\ndataset = \"risk\"\n",
            ),
            (Layer::User, "views", "[tree]\ndataset = \"risk\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        let wide = rows.iter().find(|r| r.name == "wide").expect("wide");
        assert_eq!(tree.layer, Some(Layer::User));
        assert!(
            tree.overridden,
            "user layer plus an earlier layer means overridden"
        );
        assert_eq!(wide.layer, Some(Layer::Builtin));
        assert!(
            !wide.overridden,
            "a view only one layer defines is not overridden"
        );
    }

    /// A view the user layer alone defines is theirs, not an override —
    /// getting this wrong would offer "Revert to desk" on a view no desk
    /// has, and reverting would delete it.
    #[test]
    fn a_user_only_object_is_not_marked_overridden() {
        let config = config_from(&[(Layer::User, "views", "[mine]\ndataset = \"risk\"\n")]);
        let rows = Domain::Views.objects(&config);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].overridden);
    }

    #[test]
    fn drifted_needs_an_override_entry_and_a_changed_shadow() {
        let desk_v1 = "[tree]\ndataset = \"risk\"\ncolumns = []\n";
        let user = "[tree]\ndataset = \"risk\"\n";
        let entry = |text: &str| {
            format!("[\"views.tree\"]\nshadowed_layer = \"desk\"\nshadowed_text = '''\n{text}'''\n")
        };
        // Entry recorded against exactly the desk text on disk: not drifted.
        let shadow_text = object_text(
            "tree",
            toml_value_to_item(&desk_v1.parse::<toml::Table>().unwrap()["tree"]),
        );
        let recorded = entry(&shadow_text);
        let config = config_from(&[
            (Layer::Desk, "views", desk_v1),
            (Layer::User, "views", user),
            (Layer::User, "overrides", recorded.as_str()),
        ]);
        let rows = Domain::Views.objects(&config);
        assert!(rows[0].overridden);
        assert!(!rows[0].drifted, "the desk has not moved");

        // The desk adds a column: drifted.
        let desk_v2 = "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n";
        let config = config_from(&[
            (Layer::Desk, "views", desk_v2),
            (Layer::User, "views", user),
            (Layer::User, "overrides", recorded.as_str()),
        ]);
        assert!(Domain::Views.objects(&config)[0].drifted);

        // No entry at all (an override that predates 2b): never drifted.
        let config = config_from(&[
            (Layer::Desk, "views", desk_v2),
            (Layer::User, "views", user),
        ]);
        assert!(!Domain::Views.objects(&config)[0].drifted);

        // Not overridden (user-only): an entry is stale and ignored.
        let stale = entry("x");
        let config = config_from(&[
            (Layer::User, "views", user),
            (Layer::User, "overrides", stale.as_str()),
        ]);
        assert!(!Domain::Views.objects(&config)[0].drifted);
        assert_eq!(stale_override_keys(&config), vec!["views.tree".to_string()]);
    }

    #[test]
    fn override_entry_records_the_shadowed_layer_and_its_text() {
        let value: toml::Value = "dataset = \"risk\"\n"
            .parse::<toml::Table>()
            .unwrap()
            .into();
        let entry = override_entry(Layer::Desk, "tree", &value);
        assert_eq!(entry["shadowed_layer"].as_str(), Some("desk"));
        assert_eq!(
            entry["shadowed_text"].as_str(),
            Some(object_text("tree", toml_value_to_item(&value)).as_str())
        );
        assert_eq!(override_key("views", "tree"), "views.tree");
    }

    /// A presentation-only override **is** an override. A desk view a
    /// trader has hidden a column on has a user-layer file naming it, so
    /// `r` has to be offered: telling them "no user override to revert"
    /// would be false about a file that demonstrably exists, and hiding a
    /// column is the commonest edit this whole design exists to make
    /// cheap. Spec §5.3 assumed presentation always accompanies a doc
    /// override; it does not — that is the entire point of §4.1's split.
    #[test]
    fn a_presentation_only_override_is_marked_overridden() {
        let config = config_from(&[
            (Layer::Desk, "views", "[tree]\ndataset = \"risk\"\n"),
            (
                Layer::User,
                "view_presentation",
                "[tree]\nhidden = [\"npv\"]\n",
            ),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        assert_eq!(
            tree.layer,
            Some(Layer::Desk),
            "presentation forks nothing — the desk's copy still wins"
        );
        assert!(
            tree.overridden,
            "a user-layer view_presentation entry is a user override to revert"
        );
    }

    /// And the guard that keeps `r` non-destructive: reverting deletes the
    /// user's copy, so it may only be offered when something is left
    /// behind. A view only the USER layer defines, personalised on top, is
    /// still not overridden — reverting there would delete the view.
    #[test]
    fn a_user_only_object_with_presentation_is_still_not_overridden() {
        let config = config_from(&[
            (Layer::User, "views", "[mine]\ndataset = \"risk\"\n"),
            (
                Layer::User,
                "view_presentation",
                "[mine]\nhidden = [\"npv\"]\n",
            ),
        ]);
        let rows = Domain::Views.objects(&config);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].overridden);
    }

    /// §18.2's "a name any layer already holds is refused" spans the
    /// presentation overlay, not just the domain's own doc. A view the
    /// desk has since dropped can still be named by the trader's own
    /// `view_presentation.toml` (they hid a column on it while it
    /// existed); that name produces no browse row at all, so a create
    /// checking only the rows would happily make a fresh user view that
    /// silently inherits the orphaned overlay's `hidden`/`order`/`width`
    /// — and being user-only, `r` then refuses, leaving no dialog verb
    /// that can clear it.
    #[test]
    fn a_presentation_only_name_is_taken_even_with_no_row_to_show_for_it() {
        let config = config_from(&[(
            Layer::User,
            "view_presentation",
            "[tree]\nhidden = [\"npv\"]\n",
        )]);
        assert!(
            Domain::Views.objects(&config).is_empty(),
            "no layer of views.toml defines tree, so it has no browse row"
        );
        assert!(
            Domain::Views.name_taken(&config, "tree"),
            "the orphaned overlay still holds the name"
        );
        assert!(
            !Domain::Views.name_taken(&config, "fresh"),
            "and a name nothing holds is still free"
        );
    }

    /// The other two thirds of `name_taken`'s union, each on the domain
    /// that has it: a layered doc key (Views, any layer) and a roster
    /// name no doc defines (Groupings' nine slots).
    #[test]
    fn name_taken_spans_every_layers_doc_keys_and_the_roster() {
        let config = config_from(&[(Layer::Desk, "views", "[tree]\ndataset = \"risk\"\n")]);
        assert!(Domain::Views.name_taken(&config, "tree"));
        assert!(
            Domain::Groupings.name_taken(&config, "7"),
            "an unconfigured slot is still a name the roster holds"
        );
        assert!(!Domain::Scopes.name_taken(&config, "tree"));
    }

    /// The middle layer is the case a two-layer fixture cannot see: a
    /// desk view with no user copy is the desk's, not overridden, and a
    /// desk view the user overrode reports the USER as the winning layer
    /// even though a builtin copy also exists underneath.
    #[test]
    fn the_desk_layer_wins_over_builtin_and_loses_to_the_user() {
        let config = config_from(&[
            (Layer::Builtin, "views", "[tree]\ndataset = \"a\"\n"),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"b\"\n[desk_only]\ndataset = \"c\"\n",
            ),
            (Layer::User, "views", "[tree]\ndataset = \"d\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        let tree = rows.iter().find(|r| r.name == "tree").expect("tree");
        assert_eq!(tree.layer, Some(Layer::User));
        assert!(tree.overridden);
        assert!(
            tree.summary.contains('d'),
            "the summary must describe the copy that takes effect, got {:?}",
            tree.summary
        );
        let desk_only = rows.iter().find(|r| r.name == "desk_only").expect("desk");
        assert_eq!(desk_only.layer, Some(Layer::Desk));
        assert!(
            !desk_only.overridden,
            "a desk view with no user copy is not overridden — reverting it \
             would delete the desk's own view"
        );
    }

    /// Drift needs `overrides.toml` (spec §5.2, Part 2). Until then the
    /// honest answer is `false` for every row, including an overridden
    /// one — a stand-in derived from the current config would mark every
    /// deliberate customisation as drifted.
    #[test]
    fn drift_is_not_claimed_before_overrides_toml_exists() {
        let config = config_from(&[
            (Layer::Desk, "views", "[tree]\ndataset = \"a\"\n"),
            (Layer::User, "views", "[tree]\ndataset = \"b\"\n"),
        ]);
        let rows = Domain::Views.objects(&config);
        assert!(rows[0].overridden, "sanity: this row IS an override");
        assert!(!rows[0].drifted);
    }

    /// `config_version` is a schema stamp every layered doc carries, not
    /// an object — a row for it would be a view the user could try to
    /// open and edit.
    #[test]
    fn the_config_version_stamp_is_not_an_object() {
        let config = config_from(&[(
            Layer::User,
            "views",
            "config_version = 1\n[tree]\ndataset = \"risk\"\n",
        )]);
        let names: Vec<String> = Domain::Views
            .objects(&config)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["tree".to_string()]);
    }

    /// A domain with no doc at all — a fresh install with no views —
    /// browses as an empty list rather than failing.
    #[test]
    fn a_missing_doc_is_an_empty_list() {
        let config = config_from(&[]);
        assert!(Domain::Views.objects(&config).is_empty());
    }

    /// The rung the edit stage turns on. `Browse` has nothing behind
    /// it, so `escape` must reach the close rung; `Edit` does, and the
    /// ladder must stop there first.
    #[test]
    fn only_a_nested_stage_offers_escape_a_previous_stage() {
        let mut state = ObjectDialogState::new(Domain::Views);
        assert_eq!(
            state.mode,
            DialogMode::Normal,
            "dialogs open in normal mode"
        );
        assert!(!state.has_previous_stage());
        state.stage = Stage::Edit {
            object: "tree".to_string(),
        };
        assert!(state.has_previous_stage());
    }

    #[test]
    fn a_new_view_draft_picks_the_first_real_dataset_and_no_columns_of_its_own() {
        let config = config_from(&[(
            Layer::Builtin,
            "datasets",
            "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
        )]);
        let draft = Domain::Views.new_draft(&config, "mine");
        assert!(draft.is_new);
        assert_eq!(draft.name, "mine");
        assert_eq!(
            draft.choice("dataset"),
            Some("risk"),
            "not the empty placeholder"
        );
        // No columns are the view's OWN yet — `npv` is only in the
        // available catalogue, for `space` to add (§18.2).
        assert!(draft.list_items("columns").unwrap().is_empty());
        assert!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .all(|i| !i.included)
        );
        assert!(
            draft
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{:?}",
            draft.diagnostics
        );
        // Everything counts as a change against an empty baseline.
        assert!(draft.is_dirty());
    }

    #[test]
    fn naming_is_a_stage_escape_can_step_back_from() {
        let mut state = ObjectDialogState::new(Domain::Views);
        assert!(!state.has_previous_stage());
        state.begin_naming();
        assert_eq!(state.stage, Stage::Naming);
        assert!(state.has_previous_stage());
        state.cancel_naming();
        assert_eq!(state.stage, Stage::Browse);
    }

    #[test]
    fn groupings_and_views_answer_a_new_draft_but_the_roster_domain_is_never_asked() {
        // Documented, not enforced: `n` is inert on Groupings in render.
        let config = config_from(&[]);
        assert!(Domain::Groupings.new_draft(&config, "4").is_new);
    }

    /// Editing the query re-ranks the list, so the old index points at an
    /// unrelated row and any notice names a row the selection has just
    /// left.
    #[test]
    fn setting_the_query_resets_the_selection_and_drops_the_notice() {
        let mut state = ObjectDialogState::new(Domain::Views);
        state.selected = 4;
        state.notice = Some("nothing to do".to_string());
        state.set_query("tr".to_string());
        assert_eq!(state.query, "tr");
        assert_eq!(state.selected, 0);
        assert!(state.notice.is_none());
    }

    /// Every domain opens its edit stage in normal mode with no field
    /// open — Groupings included (user ruling 2026-09-14, superseding
    /// 2026-09-12's chain-field landing, §18.8): a slot opens in the
    /// chooser, and `i` is the way into the chain field. The Views half
    /// pins that the rule has no domain arm at all.
    #[test]
    fn every_domain_opens_in_normal_mode_with_no_field_open() {
        let config = config_from(&[
            (Layer::Desk, "groupings", "3 = [\"book\"]\n"),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut state = ObjectDialogState::new(Domain::Groupings);
        state.enter_edit(&config, "3");
        let draft = state.draft.as_ref().unwrap();
        assert!(!draft.chain_entry(), "the chooser, not the chain field");
        assert!(draft.text_entry.is_none());
        assert_eq!(state.mode, DialogMode::Normal);
        assert_eq!(state.effective_query(), "");

        let mut state = ObjectDialogState::new(Domain::Views);
        state.enter_edit(&config, "tree");
        let draft = state.draft.as_ref().unwrap();
        assert!(!draft.chain_entry());
        assert_eq!(state.mode, DialogMode::Normal);
        assert_eq!(state.effective_query(), "");
    }

    /// `effective_query` is the read half of `set_query`'s one-way
    /// mirror (§16.2): each stage's own keystrokes land in — and are
    /// read back from — that stage's own slot, never the other one.
    #[test]
    fn the_effective_query_is_the_stages_own() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
        )]);
        let mut state = ObjectDialogState::new(Domain::Views);
        state.set_query("br".to_string());
        assert_eq!(state.effective_query(), "br");
        state.enter_edit(&config, "tree"); // pub(in objectdialog) — this test is inside the module tree
        assert_eq!(
            state.effective_query(),
            "",
            "entering the edit stage starts with no filter"
        );
        state.set_query("np".to_string());
        assert_eq!(state.effective_query(), "np");
        assert_eq!(
            state.query, "",
            "the browse query is untouched by an edit-stage keystroke"
        );
        state.leave_edit();
        assert_eq!(state.effective_query(), "");
    }

    /// The filter sees the name and the summary — what the row paints —
    /// so a match can always be explained by a highlight.
    #[test]
    fn the_filter_matches_the_two_lines_the_row_actually_shows() {
        let config = config_from(&[(
            Layer::User,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\ncolumns = [{ name = \"npv\" }]\n",
        )]);
        let rows = Domain::Views.objects(&config);
        let text = searchable_text(&rows[0]);
        assert!(text.contains("tree"), "{text}");
        assert!(
            text.contains("risk_snapshot"),
            "the dataset is on the row, so it must be searchable: {text}"
        );
    }

    // ---- The edit stage ---------------------------------------------

    /// A fixture with a `datasets` doc (so the dataset `Choice` has real
    /// options) and a desk `views` doc — the shape the demo config has,
    /// which is what the edit stage is built against.
    fn demo_config() -> Config {
        config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n\
                 [risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\n\
                 [other.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\n\
                 [[tree.columns]]\nname = \"delta01\"\n",
            ),
        ])
    }

    /// A draft of `name` with the cursor already on its first list item.
    /// `space` and `shift+j` are the column list's verbs, so that is where
    /// a trader who pressed `j` past the field rows would be — found by
    /// walking [`Draft::rows`] rather than hardcoded, so adding a field
    /// above the list does not silently move these tests onto a different
    /// row and keep passing.
    fn draft_for(name: &str) -> Draft {
        let config = demo_config();
        let mut draft = Domain::Views.draft(&config, name);
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .expect("the fixture view has columns");
        draft
    }

    fn list_items(draft: &Draft) -> &[ListItem] {
        draft
            .list_items("columns")
            .expect("the columns field is an ordered list")
    }

    fn list_names(draft: &Draft) -> Vec<String> {
        list_items(draft).iter().map(|i| i.name.clone()).collect()
    }

    /// A draft with one `Doc` field and one `Presentation` field changed.
    fn dirty_draft_touching_both() -> Draft {
        let mut draft = draft_for("tree");
        draft.toggle_selected(); // a column's inclusion — Presentation
        draft.selected = 0;
        draft.toggle_selected(); // the dataset — Doc
        draft
    }

    /// The whole reason Views is built first. A dragged width, a hidden
    /// column and a reordered list must never reach `views.toml`: writing
    /// them there forks the desk's view, and a forked view is frozen — the
    /// desk adds a column next week and the trader never sees it.
    #[test]
    fn presentation_fields_and_doc_fields_go_to_different_destinations() {
        let config = demo_config();
        let fields = Domain::Views.fields(&config, Some("tree"));
        let dataset = fields.iter().find(|f| f.key == "dataset").expect("dataset");
        assert_eq!(
            dataset.dest,
            Destination::Doc,
            "the dataset defines the view"
        );
        let columns = fields.iter().find(|f| f.key == "columns").expect("columns");
        assert_eq!(
            columns.dest,
            Destination::Presentation,
            "order, inclusion and width are presentation — a dragged width must \
             never fork a desk view"
        );
    }

    /// `space` toggles inclusion; `shift+j` moves the item, not the cursor.
    #[test]
    fn an_ordered_list_toggles_and_reorders() {
        let mut draft = draft_for("tree");
        let before: Vec<String> = list_names(&draft);
        draft.toggle_selected(); // space on item 0
        assert!(!list_items(&draft)[0].included);
        draft.move_item(1); // shift+j
        assert_eq!(
            list_names(&draft)[1],
            before[0],
            "the item moved, not the cursor"
        );
    }

    /// A draft with a single field of `kind`, cursor already on it —
    /// mirrors `Domain::draft`'s own construction so a directly-built
    /// field behaves exactly as one that came from a real config would.
    fn single_field_draft(kind: FieldKind) -> Draft {
        let field = Field {
            key: "value".to_string(),
            label: "Value".to_string(),
            kind,
            dest: Destination::Doc,
            layer: None,
        };
        Draft {
            name: "test".to_string(),
            is_new: false,
            baseline: vec![field.clone()],
            fields: vec![field],
            source: toml::Table::new(),
            baseline_source: toml::Table::new(),
            selected: 0,
            query: String::new(),
            diagnostics: Vec::new(),
            text_entry: None,
            parent_fields: None,
            column: None,
            values: None,
            column_ctx: None,
            dataset_layer: BTreeMap::new(),
        }
    }

    /// `Number` refused at `max` and had no way down at all, so the first
    /// adapter with a real `Number` field would inherit one that can be
    /// raised and never lowered. (Groupings turned out not to be that
    /// adapter after all — its `slot` is display-only, see
    /// `groupings.rs`'s module doc — so this still guards a field no
    /// domain has built yet.)
    #[test]
    fn a_number_steps_both_ways_and_stops_at_each_end() {
        // At min: stepping back is a no-op returning false; forward moves it.
        let mut draft = single_field_draft(FieldKind::Number {
            value: 0,
            min: 0,
            max: 2,
            step: 1,
            wrap: false,
        });
        assert!(
            !draft.toggle_selected_back().changed(),
            "already at min, so backward changes nothing"
        );
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 0,
                min: 0,
                max: 2,
                step: 1,
                wrap: false,
            }
        );
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 1,
                min: 0,
                max: 2,
                step: 1,
                wrap: false,
            }
        );

        // At max: forward is a no-op; backward moves it.
        let mut draft = single_field_draft(FieldKind::Number {
            value: 2,
            min: 0,
            max: 2,
            step: 1,
            wrap: false,
        });
        assert!(
            !draft.toggle_selected().changed(),
            "already at max, so forward changes nothing"
        );
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 2,
                min: 0,
                max: 2,
                step: 1,
                wrap: false,
            }
        );
        assert!(draft.toggle_selected_back().changed());
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 1,
                min: 0,
                max: 2,
                step: 1,
                wrap: false,
            }
        );
    }

    /// 2c §5.4: a `Number` with `wrap: true` steps past its bound and
    /// lands on the other side rather than clamping — the hue field's
    /// own behaviour — while a plain (`wrap: false`) field still clamps
    /// to the bound, exactly as `a_number_steps_both_ways_and_stops_at_
    /// each_end` pins for `step: 1`.
    #[test]
    fn a_number_steps_by_its_step_and_wraps_only_when_asked() {
        let mut draft = single_field_draft(FieldKind::Number {
            value: 350,
            min: 0,
            max: 359,
            step: 15,
            wrap: true,
        });
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(
            matches!(draft.fields[0].kind, FieldKind::Number { value: 5, .. }),
            "wraps: {:?}",
            draft.fields[0].kind
        );
        assert_eq!(draft.toggle_selected_back(), Step::Changed);
        assert!(matches!(
            draft.fields[0].kind,
            FieldKind::Number { value: 350, .. }
        ));
        let mut draft = single_field_draft(FieldKind::Number {
            value: 355,
            min: 0,
            max: 359,
            step: 15,
            wrap: false,
        });
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(
            matches!(draft.fields[0].kind, FieldKind::Number { value: 359, .. }),
            "lands on the bound without wrap"
        );
        assert_eq!(
            draft.toggle_selected(),
            Step::Inert,
            "at the bound, no wrap: inert"
        );
    }

    /// A value the dialog's own bounds never produced — Sources' reader
    /// accepts any positive `stable_mtime` while the dialog caps display
    /// at 100 — must be refused, not clamped: a clamp would write a
    /// number (the bound) the trader never typed (§19.1's
    /// refuse-don't-clamp ruling).
    #[test]
    fn a_number_outside_its_range_is_refused_not_clamped_by_a_step() {
        let mut draft = single_field_draft(FieldKind::Number {
            value: 500,
            min: 1,
            max: 100,
            step: 1,
            wrap: false,
        });
        assert!(matches!(draft.toggle_selected(), Step::Refused(_)));
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 500,
                min: 1,
                max: 100,
                step: 1,
                wrap: false,
            },
            "forward must not clamp the value"
        );
        assert!(matches!(draft.toggle_selected_back(), Step::Refused(_)));
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Number {
                value: 500,
                min: 1,
                max: 100,
                step: 1,
                wrap: false,
            },
            "backward must not clamp the value either"
        );
    }

    /// `Choice` wraps forward; it must wrap backward symmetrically, or the
    /// last option is three keystrokes away and the first is unreachable
    /// from it.
    #[test]
    fn a_choice_wraps_in_both_directions() {
        let options = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut draft = single_field_draft(FieldKind::Choice {
            options: options.clone(),
            selected: 0,
        });
        assert!(draft.toggle_selected_back().changed());
        assert_eq!(
            draft.choice("value"),
            Some("c"),
            "from 0, back wraps to last"
        );

        let mut draft = single_field_draft(FieldKind::Choice {
            options,
            selected: 2,
        });
        assert!(draft.toggle_selected().changed());
        assert_eq!(
            draft.choice("value"),
            Some("a"),
            "from last, forward wraps to 0"
        );
    }

    /// A draft groups its writes by destination, so one save is at most one
    /// `config_write::edit` per file — never a write per field.
    #[test]
    fn a_save_groups_writes_by_destination() {
        let draft = dirty_draft_touching_both();
        let groups = draft.writes_by_destination();
        assert_eq!(groups.len(), 2);
        assert!(groups.contains_key(&Destination::Doc));
        assert!(groups.contains_key(&Destination::Presentation));
    }

    /// The `Doc` write is a whole-object override, so anything the field
    /// vocabulary does not model has to survive it. A view rendered from
    /// `dataset` plus a list of names would silently drop the grouping,
    /// the sort, the column kinds and every format — and the user would
    /// only find out when the blotter came back wrong.
    #[test]
    fn a_doc_write_preserves_everything_the_fields_do_not_model() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\ngrouping = [\"book\"]\n\
             [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
             [[tree.columns]]\nname = \"npv\"\nlabel = \"NPV\"\n\
             [tree.columns.format]\nprecision = 3\n\
             [[tree.sort]]\ncolumn = \"npv\"\ndescending = true\n",
        )]);
        let draft = Domain::Views.draft(&config, "tree");
        let text = object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
        for kept in [
            "grouping",
            "book",
            "kind = \"dimension\"",
            "label = \"NPV\"",
            "precision = 3",
            "descending = true",
        ] {
            assert!(text.contains(kept), "{kept:?} was dropped from:\n{text}");
        }
    }

    /// And the mirror image, which is the fork this whole design exists to
    /// prevent: nothing presentational may appear in the view's own table.
    #[test]
    fn a_doc_write_carries_no_order_no_hidden_and_no_width() {
        let mut draft = draft_for("tree");
        draft.toggle_selected(); // hide the first column
        draft.move_item(1); // and reorder it
        let text = object_text("tree", Domain::Views.to_table(&draft, Destination::Doc));
        for presentational in ["hidden", "order", "width"] {
            assert!(
                !text.contains(presentational),
                "{presentational:?} reached views.toml, forking the desk's view:\n{text}"
            );
        }
        // The column order in the view's own table is the source's, not
        // the trader's: order is presentation.
        assert_eq!(
            text.matches("name = ").count(),
            3,
            "all three columns are still there:\n{text}"
        );
        assert!(
            text.find("name = \"book\"").unwrap() < text.find("name = \"npv\"").unwrap(),
            "the source's column order is untouched by a reorder:\n{text}"
        );
    }

    /// The presentation table is what `view_presentation.toml` receives:
    /// `order` once the trader has actually reordered, and `hidden` and
    /// `width` only when there is something to say.
    #[test]
    fn the_presentation_table_holds_order_hidden_and_width() {
        let mut draft = draft_for("tree");
        if let Some(FieldKind::OrderedList { items, .. }) = draft
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .map(|f| &mut f.kind)
        {
            items[1].presentation.width = Some(120.0);
        }
        draft.toggle_selected(); // hide `book`
        draft.move_item(1); // and move it below `npv`
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(
            text.contains("order = [\"npv\", \"book\", \"delta01\"]"),
            "{text}"
        );
        assert!(
            text.contains("[tree.columns.book]") && text.contains("hidden = true"),
            "{text}"
        );
        assert!(
            text.contains("[tree.columns.npv]") && text.contains("width = 120"),
            "{text}"
        );
    }

    /// **A presentation save must not freeze the desk's own values.**
    /// `order` and `width` are both read off the EFFECTIVE view, which
    /// already carries whatever `views.toml` declared — so writing every
    /// column's position and every declared width back into the trader's
    /// file pins the desk's layout for that trader against the desk's
    /// later changes. That is the same freeze [`Destination`] exists to
    /// prevent, one field-granularity down, and it fires on the single
    /// commonest edit there is: hiding one column.
    #[test]
    fn a_presentation_save_writes_only_what_the_trader_changed() {
        let config = config_from(&[(
            Layer::Desk,
            "views",
            "[tree]\ndataset = \"risk_snapshot\"\n\
             [[tree.columns]]\nname = \"book\"\n\
             [[tree.columns]]\nname = \"npv\"\nwidth = 140\n",
        )]);
        let mut draft = Domain::Views.draft(&config, "tree");
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .expect("the fixture view has columns");
        assert_eq!(
            draft.list_items("columns").map(|i| i[1].presentation.width),
            Some(Some(140.0)),
            "the desk's width reaches the draft — that is why it can be copied back"
        );

        draft.toggle_selected(); // hide `book`, and change nothing else
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(
            text.contains("[tree.columns.book]") && text.contains("hidden = true"),
            "{text}"
        );
        assert!(
            !text.contains("order"),
            "nothing was reordered, so pinning the desk's order is a freeze:\n{text}"
        );
        assert!(
            !text.contains("140"),
            "the desk's own width must not be copied into the trader's file:\n{text}"
        );
    }

    /// A view with nothing hidden and no widths writes neither key, rather
    /// than empty containers — and because the table is replaced whole, a
    /// previously hidden column really does come back.
    #[test]
    fn unhiding_the_last_column_removes_the_hidden_key_entirely() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        draft.toggle_selected();
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(!text.contains("hidden"), "{text}");
        assert!(!text.contains("width"), "{text}");
    }

    /// A width already in `view_presentation.toml` survives an edit that
    /// was about something else. Losing it on save would be the same class
    /// of defect as forking the view: silent, and only visible next time
    /// the trader looked at the column.
    #[test]
    fn an_existing_width_survives_a_save_that_was_about_something_else() {
        let config = config_from(&[
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\n\
                 [[tree.columns]]\nname = \"book\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
            (
                Layer::User,
                "view_presentation",
                "[tree]\nhidden = [\"book\"]\n[tree.width]\nnpv = 140.0\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        assert_eq!(
            draft.list_items("columns").map(|i| i[1].presentation.width),
            Some(Some(140.0)),
            "the draft has to read the width back before it can keep it"
        );
        // Unhide `book`, which is a change to something else entirely.
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| matches!(r, EditRow::Item { .. }))
            .unwrap();
        draft.toggle_selected();
        let text = object_text(
            "tree",
            Domain::Views.to_table(&draft, Destination::Presentation),
        );
        assert!(
            text.contains("[tree.columns.npv]") && text.contains("width = 140"),
            "the width was dropped:\n{text}"
        );
        assert!(
            !text.contains("[tree.columns.book]"),
            "book returned to its desk default (not hidden) and needs no table:\n{text}"
        );
        assert!(!text.contains("hidden"), "{text}");
    }

    /// Membership is definitional however the list is presented: a column
    /// the object gained or lost has to reach `views.toml` even though the
    /// field it lives on is a `Presentation` field. Reordering and hiding
    /// must not — that is the split.
    #[test]
    fn changing_the_column_set_writes_the_doc_but_reordering_does_not() {
        let mut reordered = draft_for("tree");
        reordered.move_item(1);
        assert_eq!(
            reordered
                .writes_by_destination()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![Destination::Presentation],
            "a reorder is presentation and nothing else"
        );

        let mut dropped = draft_for("tree");
        if let Some(FieldKind::OrderedList { items, .. }) = dropped
            .fields
            .iter_mut()
            .find(|f| f.key == "columns")
            .map(|f| &mut f.kind)
        {
            items.remove(0);
        }
        let groups = dropped.writes_by_destination();
        assert!(
            groups.contains_key(&Destination::Doc),
            "losing a column changes what the view IS: {groups:?}"
        );
        let text = object_text("tree", Domain::Views.to_table(&dropped, Destination::Doc));
        assert!(
            !text.contains("name = \"book\""),
            "and the view's own table has to lose it:\n{text}"
        );
    }

    /// `fields` takes `Option<&str>` because spec §4 has it serve the
    /// create path too, and a name nothing defines has to come back as an
    /// object with no items of its OWN rather than a panic — a
    /// `Choice` with no dataset selected and an empty column list. Its
    /// catalogue is not empty though (§18.2): with no view to read,
    /// `current` falls back to the schema's first dataset
    /// (alphabetically, `other`, which has one column, `book`), and that
    /// column is in the catalogue for `space` to add.
    #[test]
    fn an_object_that_does_not_exist_has_empty_fields_rather_than_panicking() {
        let config = demo_config();
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Views.fields(&config, object);
            assert_eq!(fields.len(), 2, "{object:?}");
            let FieldKind::OrderedList { items, available } = &fields[1].kind else {
                panic!("{object:?} columns field must be an ordered list");
            };
            assert!(
                items.is_empty(),
                "{object:?} should have no columns of its own yet, got {items:?}"
            );
            assert_eq!(
                available
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect::<Vec<_>>(),
                ["book"],
                "only the dataset's available ones, got {available:?}"
            );
            // The destinations do not depend on the object, which is what
            // lets the create path reuse them unchanged.
            assert_eq!(fields[0].dest, Destination::Doc);
            assert_eq!(fields[1].dest, Destination::Presentation);
        }
    }
    /// A draft nobody touched writes nothing at all — which is what keeps
    /// opening a view, looking at it and pressing `s` from forking it.
    #[test]
    fn an_untouched_draft_is_clean_and_writes_nothing() {
        let draft = draft_for("tree");
        assert!(!draft.is_dirty());
        assert!(draft.writes_by_destination().is_empty());
    }

    /// Putting a value back where it started makes the draft clean again:
    /// dirtiness is a comparison against the baseline, not a flag that
    /// latches on the first keystroke.
    #[test]
    fn a_change_undone_leaves_the_draft_clean() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        assert!(draft.is_dirty());
        draft.toggle_selected();
        assert!(!draft.is_dirty(), "the value is back where it started");
    }

    /// `space` on a row with nothing to change reports `false` so the
    /// dialog can say so — a key that appears inert is the defect class
    /// the interaction model exists to remove.
    #[test]
    fn space_on_a_row_with_no_value_says_it_did_nothing() {
        let mut draft = draft_for("tree");
        draft.selected = draft
            .rows()
            .iter()
            .position(|r| *r == EditRow::Field(1))
            .expect("the columns field's own header row");
        assert!(
            !draft.toggle_selected().changed(),
            "a list header has no value"
        );
        assert!(!draft.is_dirty());
    }

    /// `shift+k` at the top and `shift+j` at the bottom do nothing and say
    /// so, rather than wrapping the item round the list.
    #[test]
    fn an_item_does_not_move_past_either_end() {
        let mut draft = draft_for("tree");
        let before = list_names(&draft);
        assert!(
            draft.move_item(-1).is_none(),
            "the first item has nowhere up to go"
        );
        draft.selected += 2;
        assert!(draft.move_item(1).is_none(), "nor the last one down");
        assert_eq!(list_names(&draft), before);
    }

    /// Validation runs against the DRAFT alone (spec §7.2). Validating the
    /// merged doc instead would report every other broken view in the
    /// config against the one object the user is editing.
    #[test]
    fn validation_sees_the_draft_and_not_the_rest_of_the_config() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[good]\ndataset = \"risk_snapshot\"\n[[good.columns]]\nname = \"npv\"\n\
                 [broken]\n[[broken.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let good = Domain::Views.draft(&config, "good");
        assert!(
            good.diagnostics.is_empty(),
            "another view's problem is not this view's: {:?}",
            good.diagnostics
        );
        let broken = Domain::Views.draft(&config, "broken");
        assert!(
            broken
                .diagnostics
                .iter()
                .any(|d| d.message.contains("dataset")),
            "the object being edited IS validated: {:?}",
            broken.diagnostics
        );
    }

    /// A dataset the schema does not have is the one problem the doc's own
    /// reader cannot see — it parses cleanly and then compiles to nothing.
    #[test]
    fn a_dataset_the_schema_lacks_is_reported() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"retired\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let draft = Domain::Views.draft(&config, "tree");
        assert!(
            draft
                .diagnostics
                .iter()
                .any(|d| d.message.contains("retired")),
            "{:?}",
            draft.diagnostics
        );
        // And the choice can still show it, or stepping the field would
        // silently change the view the moment it was touched.
        assert_eq!(draft.choice("dataset"), Some("retired"));
    }

    /// Each destination knows exactly one file. `Presentation` is one of
    /// the two scaffold doors onto `view_presentation.toml` — the other is
    /// `Domain::presentation_doc`, asserted below — and both read the name
    /// off the Views adapter's own constant, which is what keeps them from
    /// naming different files.
    #[test]
    fn each_destination_names_its_own_file() {
        assert_eq!(Destination::Doc.doc(Domain::Views), "views");
        assert_eq!(
            Destination::Presentation.doc(Domain::Views),
            "view_presentation"
        );
        // The second door onto the same name, and the two domains that
        // have no overlay file at all.
        assert_eq!(Domain::Views.presentation_doc(), Some("view_presentation"));
        assert_eq!(Domain::Groupings.presentation_doc(), None);
        assert_eq!(Domain::Scopes.presentation_doc(), None);
    }

    /// Saving accepts the draft so `s` pressed twice does not write twice.
    #[test]
    fn marking_a_draft_saved_makes_it_clean_again() {
        let mut draft = draft_for("tree");
        draft.toggle_selected();
        assert!(draft.is_dirty());
        draft.mark_saved();
        assert!(!draft.is_dirty());
        assert!(draft.writes_by_destination().is_empty());
    }

    /// Entering the edit stage turns the ladder's `PreviousStage` rung on
    /// and drops the browse query with it: `state.query` (browse) and
    /// `Draft::query` (edit, §18.3) are two separate cursor spaces, so a
    /// stale browse query left applied would rank a list the trader is no
    /// longer looking at, and would also eat the `escape` that was meant
    /// to go back a stage before the ladder ever reached the edit stage's
    /// own query.
    #[test]
    fn entering_the_edit_stage_arms_the_previous_stage_rung() {
        let config = demo_config();
        let mut state = ObjectDialogState::new(Domain::Views);
        state.set_query("tr".to_string());
        assert!(!state.has_previous_stage());
        state.mode = DialogMode::Filter;
        state.enter_edit(&config, "tree");
        assert!(state.has_previous_stage());
        assert_eq!(state.query, "", "the browse query does not follow you in");
        assert_eq!(
            state.mode,
            DialogMode::Normal,
            "nor does filter mode — escape would take the LeaveFilter rung \
             and close the dialog instead of going back a stage"
        );
        assert_eq!(
            state.draft.as_ref().map(|d| d.name.clone()),
            Some("tree".to_string())
        );
        state.leave_edit();
        assert!(!state.has_previous_stage());
        assert!(state.draft.is_none());
    }

    /// The confirm prompts name the object rather than asking "are you
    /// sure". They no longer spell out the fork's long-term cost ("it stops
    /// following the desk") — a user ruling of 2026-09-11 (commit 9916049)
    /// took that sentence out of every prompt; the cost is documented in
    /// spec §4.1, and the prompt's job is to name what is being copied.
    #[test]
    fn a_confirm_names_the_object_and_the_consequence() {
        assert!(Confirm::Delete.prompt("tree").contains("tree"));
        assert!(Confirm::Revert.prompt("tree").contains("tree"));
    }

    /// `Confirm::Overwrite` is armed only on a user-owned scope (a
    /// desk-owned one writes at once, user ruling 2026-09-14), so its one
    /// prompt must be true of that case alone: the previous contents
    /// really are lost, and there is no fork to claim.
    #[test]
    fn overwrite_prompts_tell_the_truth_about_what_it_costs() {
        let owned = Confirm::Overwrite.prompt("mine");
        assert!(owned.contains("mine"), "{owned}");
        assert!(owned.contains("lost"), "{owned}");
        assert!(
            !owned.contains("reverts"),
            "a user-owned scope's prompt must not claim a fork: {owned}"
        );
    }

    /// Rows are ordered by name, not by the order three separate files
    /// happen to list them in.
    #[test]
    fn rows_are_ordered_by_name_across_layers() {
        let config = config_from(&[
            (Layer::Builtin, "views", "[zebra]\ndataset = \"a\"\n"),
            (Layer::User, "views", "[alpha]\ndataset = \"a\"\n"),
        ]);
        let names: Vec<String> = Domain::Views
            .objects(&config)
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["alpha".to_string(), "zebra".to_string()]);
    }

    // ---- Task 4, as §18.7 left it: the object's own list and the
    // ---- catalogue behind it -----------------------------------------

    /// A Groupings list has no available catalogue at ALL (§18.7.1) —
    /// ticking IS membership there — so `remove_selected` refuses and
    /// names the verb that does work here (`space`) rather than silently
    /// unticking. Decided by the catalogue's absence, never by a `dest`
    /// and never by whether some catalogue happens to be empty right now.
    #[test]
    fn a_groupings_list_has_no_available_block_and_x_refuses() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"book\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        assert!(
            draft.available_items("dimensions").is_none(),
            "no catalogue exists here, which is not the same as an empty one"
        );
        draft.selected = 2;
        assert_eq!(
            draft.remove_selected(),
            Step::Refused("space unticks here".to_string())
        );
    }

    /// **The regression this ruling exists to close.** A Views draft
    /// whose catalogue is EMPTY — every column the dataset offers already
    /// added — must still let `x` remove one: a catalogue that exists and
    /// holds nothing is not the same as no catalogue at all (§18.7.2).
    /// Deciding by emptiness (as `dest`-scanning and item-scanning builds
    /// before it both did, each in its own way) has `x` go dead the moment
    /// the trader finishes adding everything the dataset offers.
    #[test]
    fn remove_selected_still_works_when_the_catalogue_is_empty() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk\"\n[[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        let mut draft = Domain::Views.draft(&config, "tree");
        // `npv` is the dataset's only column and the view already has it,
        // so the catalogue exists and is empty.
        assert!(
            draft
                .available_items("columns")
                .is_some_and(<[_]>::is_empty),
            "sanity: this fixture's catalogue must exist and be empty"
        );
        draft.selected = 2; // the one item row: npv
        assert!(draft.remove_selected().changed());
        assert!(draft.list_items("columns").unwrap().is_empty());
        assert_eq!(
            draft
                .available_items("columns")
                .unwrap()
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            ["npv"],
            "and the removed column is what the catalogue now holds"
        );
    }

    // ---- Mouse parity Task 4 (§18.9): dropping a list row by name ----

    /// A Views draft over `book`, `npv`, `delta01` — every column already
    /// in the view, so the catalogue exists and is empty (`draft_for`'s
    /// own fixture).
    fn three_column_draft() -> Draft {
        draft_for("tree")
    }

    /// A Views draft over the same dataset with only `book` and `npv` in
    /// the view — `delta01` sits in the catalogue, ready to be dragged in.
    fn two_column_draft_with_one_available() -> Draft {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk_snapshot.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk_snapshot.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
                 [risk_snapshot.columns.delta01]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
            ),
            (
                Layer::Desk,
                "views",
                "[tree]\ndataset = \"risk_snapshot\"\n\
                 [[tree.columns]]\nname = \"book\"\nkind = \"dimension\"\n\
                 [[tree.columns]]\nname = \"npv\"\n",
            ),
        ]);
        Domain::Views.draft(&config, "tree")
    }

    /// A Groupings slot with `book`, `lhu` already chosen — no catalogue
    /// at all (§18.7.1), the same shape
    /// `a_groupings_list_has_no_available_block_and_x_refuses` builds.
    fn groupings_draft() -> Draft {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"book\", \"lhu\"]\n"),
        ]);
        Domain::Groupings.draft(&config, "3")
    }

    /// A bare, unincluded item — the shape `dataset_catalogue` builds for
    /// an available row — for tests that need a second catalogue entry.
    fn item(name: &str) -> ListItem {
        ListItem {
            name: name.to_string(),
            included: false,
            presentation: ColumnPresentation::default(),
            kind: None,
            note: None,
        }
    }

    /// A `RowDrag` payload naming `field` by key, the way a real drag
    /// carries the field the row it grabbed lives in — never hardcoded to
    /// one domain's field name, since Views' list is `columns` and
    /// Groupings' is `dimensions`.
    fn drag(field: &str, own: bool, name: &str) -> RowDrag {
        RowDrag {
            field: field.to_string(),
            own,
            name: name.to_string(),
        }
    }

    /// §18.9.3: "drop on a row" means take that row's index. Downward
    /// lands after the target's old position, upward before it.
    #[test]
    fn a_drop_takes_the_target_rows_index() {
        let mut draft = three_column_draft(); // book, npv, delta01 as items; catalogue empty-but-Some
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "book"),
                &drag("columns", true, "delta01")
            ),
            Step::Changed
        );
        assert_eq!(list_names(&draft), ["npv", "delta01", "book"]);
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "book"),
                &drag("columns", true, "npv")
            ),
            Step::Changed
        );
        assert_eq!(list_names(&draft), ["book", "npv", "delta01"]);
    }

    /// The cursor follows the dropped item, so the next keystroke acts on
    /// the thing the trader just placed.
    #[test]
    fn the_cursor_follows_the_dropped_item() {
        let mut draft = three_column_draft();
        draft.drop_row(
            &drag("columns", true, "book"),
            &drag("columns", true, "delta01"),
        );
        assert_eq!(draft.row_label(draft.selected_row().unwrap()), "book");
    }

    /// Available → Item adds at the target's index rather than appending.
    #[test]
    fn dropping_an_available_row_onto_the_list_adds_it_at_that_index() {
        let mut draft = two_column_draft_with_one_available(); // items book, npv; available delta01
        assert_eq!(
            draft.drop_row(
                &drag("columns", false, "delta01"),
                &drag("columns", true, "book")
            ),
            Step::Changed
        );
        assert_eq!(list_names(&draft), ["delta01", "book", "npv"]);
        assert!(draft.list_items("columns").unwrap()[0].included);
        let FieldKind::OrderedList { available, .. } = &draft.fields[1].kind else {
            panic!()
        };
        assert!(available.as_ref().unwrap().is_empty());
    }

    /// Item → Available removes, exactly as `x` does, catalogue index
    /// ignored (it has no order).
    #[test]
    fn dropping_an_item_onto_the_catalogue_removes_it() {
        let mut draft = two_column_draft_with_one_available();
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "npv"),
                &drag("columns", false, "delta01")
            ),
            Step::Changed
        );
        assert_eq!(list_names(&draft), ["book"]);
        let FieldKind::OrderedList { available, .. } = &draft.fields[1].kind else {
            panic!()
        };
        let avail: Vec<&str> = available
            .as_ref()
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(avail, ["delta01", "npv"]);
        assert!(!available.as_ref().unwrap()[1].included);
    }

    /// Nothing to do: same row, catalogue to catalogue, a name that no
    /// longer resolves (a keystroke removed it mid-drag), a field whose
    /// kind is not a list at all, or two rows that resolve on different
    /// fields. None of these writes, and none moves the cursor either —
    /// only a real drop reaches `follow`.
    #[test]
    fn inert_drops_change_nothing() {
        let mut draft = two_column_draft_with_one_available();
        draft.selected = 1;
        let before = draft.fields.clone();
        let selected_before = draft.selected;
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "book"),
                &drag("columns", true, "book")
            ),
            Step::Inert
        );
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "gone"),
                &drag("columns", true, "book")
            ),
            Step::Inert
        );
        assert_eq!(
            draft.drop_row(
                &drag("columns", true, "book"),
                &drag("columns", true, "gone")
            ),
            Step::Inert
        );
        // `dataset` exists but is a `Choice`, not a list — `locate`
        // refuses it before the per-case match ever runs.
        assert_eq!(
            draft.drop_row(
                &drag("dataset", true, "irrelevant"),
                &drag("columns", true, "book")
            ),
            Step::Inert
        );
        // Two rows that both resolve, on two different real lists: the
        // `field != dst_field` guard, not a locate failure.
        draft.fields.push(Field {
            key: "other".to_string(),
            label: "Other".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![item("z")],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        });
        assert_eq!(
            draft.drop_row(&drag("columns", true, "book"), &drag("other", true, "z")),
            Step::Inert
        );
        draft.fields.pop();
        assert_eq!(draft.fields, before);
        assert_eq!(
            draft.selected, selected_before,
            "an inert drop must never move the cursor"
        );
        // Two catalogue rows (add a second available item first).
        let FieldKind::OrderedList { available, .. } = &mut draft.fields[1].kind else {
            panic!()
        };
        available.as_mut().unwrap().push(item("gamma"));
        assert_eq!(
            draft.drop_row(
                &drag("columns", false, "delta01"),
                &drag("columns", false, "gamma")
            ),
            Step::Inert
        );
        assert_eq!(draft.selected, selected_before);
    }

    /// A list with no catalogue (Groupings) is `Inert`, not
    /// `remove_selected`'s `Step::Refused("space unticks here")`: `dst`
    /// claims a catalogue that does not exist, so it fails to `locate`
    /// and the whole drop is `Inert` before `drop_row`'s per-case match
    /// ever runs — there is no available row for that refusal's demotion
    /// to land on in the first place (see `drop_row`'s own doc).
    #[test]
    fn a_drop_onto_a_missing_catalogue_is_inert() {
        let mut draft = groupings_draft(); // items book, lhu; available None
        // There is no available row to target, so the refusal is reached
        // through a payload claiming one.
        let step = draft.drop_row(
            &drag("dimensions", true, "book"),
            &RowDrag {
                field: "dimensions".into(),
                own: false,
                name: "lhu".into(),
            },
        );
        assert!(
            matches!(step, Step::Inert),
            "a target that does not resolve is inert, never a phantom removal"
        );
        let names: Vec<&str> = draft
            .list_items("dimensions")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["book", "lhu"],
            "an Inert drop must write nothing — book is not a phantom removal"
        );
    }

    /// `row_drag` and `locate` are inverses over every list row, and a
    /// field row has no payload at all.
    #[test]
    fn row_drag_round_trips_through_locate() {
        let draft = two_column_draft_with_one_available();
        for row in draft.rows() {
            match row {
                EditRow::Field(_) => assert_eq!(draft.row_drag(row), None),
                EditRow::Item { .. } | EditRow::Available { .. } => {
                    let payload = draft.row_drag(row).expect("list rows drag");
                    assert_eq!(draft.locate(&payload), Some(row));
                }
            }
        }
    }

    // ---- Task 6: filtering the edit stage (§18.3) --------------------

    /// The edit stage's own filter narrows [`Draft::visible_rows`], and
    /// [`Draft::selected`] indexes that filtered list — the same
    /// convention the browse stage already has — so a verb like `space`
    /// acts on the row the trader is looking at, not on whatever
    /// unfiltered row happened to sit at that position.
    #[test]
    fn the_edit_stage_filters_by_label_and_the_cursor_indexes_the_filtered_list() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.lhu]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"book\", \"lhu\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "lhu".to_string();
        let visible = draft.visible_rows();
        assert_eq!(visible.len(), 1);
        draft.selected = 0;
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Item { field: 1, item: 1 })
        );
        assert!(
            draft.toggle_selected().changed(),
            "the verb acts on the filtered row"
        );
    }

    /// `shift+j`/`shift+k` under a filter move the item past the next
    /// VISIBLE neighbour, not the next literal one — jumping over
    /// whatever the query is hiding — and report how many rows it
    /// skipped so the caller's notice can say so. Single-letter column
    /// names cannot carry this test (a one-character candidate cannot
    /// hold a query long enough to pick two of three of them out from
    /// the third — see `listfilter::rank`'s subsequence matcher), so the
    /// fixture uses longer names and a query ("al") that is a subsequence
    /// of `alpha` and of `charlie` but not of `bravo`.
    #[test]
    fn reordering_under_a_filter_moves_past_the_hidden_rows_and_says_how_many() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.alpha]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.bravo]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.charlie]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (
                Layer::User,
                "groupings",
                "3 = [\"alpha\", \"bravo\", \"charlie\"]\n",
            ),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "al".to_string(); // hides bravo, the middle item
        assert_eq!(
            draft.visible_rows().len(),
            2,
            "sanity: the query must hide exactly bravo"
        );
        draft.selected = 0; // alpha
        assert_eq!(draft.move_item(1), Some(1), "one hidden row skipped");
        let names: Vec<&str> = draft
            .list_items("dimensions")
            .unwrap()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(&names[..3], ["bravo", "charlie", "alpha"]);
        // Row order, not fuzzy score, decides the filtered index now
        // (review round 1): after the move, `rows()` reads
        // [.., charlie (row 3), alpha (row 4)], and since "al" still
        // scores alpha far higher than charlie, a score-ordered
        // `visible_rows` would have put alpha BACK at index 0 — the
        // identity check below would pass either way, which is exactly
        // why this index is asserted explicitly too.
        assert_eq!(
            draft.selected, 1,
            "alpha is the second VISIBLE row in row order, not the first by score"
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Item { field: 1, item: 2 }),
            "the cursor followed alpha"
        );
    }

    /// Review round 1: the edit stage's row order IS the data — column
    /// order, chain order — so a filter must narrow it, never reorder
    /// it. `visible_rows` used to sort by fuzzy score like the browse
    /// list does, which put a later, better-scoring match ahead of an
    /// earlier, worse-scoring one; that made `shift+j` unable to change
    /// the painted order at all, and left §18.1's section header sitting
    /// above whichever row happened to score best rather than at the
    /// start of its block. `apple` (row 3) scores far higher against "a"
    /// than `banana` (row 2) does (an idx-0 match earns a head-start bonus —
    /// see `palette::fuzzy_match_lowered`), so a score-ordered list would
    /// paint them in the wrong order despite both matching.
    #[test]
    fn visible_rows_lists_matches_in_row_order_not_score_order() {
        let config = config_from(&[
            (
                Layer::Builtin,
                "datasets",
                "[risk.columns.banana]\ntype = \"utf8\"\nrole = \"dimension\"\n\
                 [risk.columns.apple]\ntype = \"utf8\"\nrole = \"dimension\"\n",
            ),
            (Layer::User, "groupings", "3 = [\"banana\", \"apple\"]\n"),
        ]);
        let mut draft = Domain::Groupings.draft(&config, "3");
        draft.query = "a".to_string();
        let rows = draft.rows();
        let names: Vec<String> = draft
            .visible_rows()
            .iter()
            .filter_map(|m| rows.get(m.row))
            .map(|r| draft.row_label(*r))
            .collect();
        assert_eq!(
            names,
            vec!["banana".to_string(), "apple".to_string()],
            "list order must win over apple's higher fuzzy score"
        );
    }

    fn draft_with_number_and_text() -> Draft {
        Draft::new_object(
            "x",
            vec![
                Field {
                    key: "polls".to_string(),
                    label: "Stable polls".to_string(),
                    kind: FieldKind::Number {
                        value: 3,
                        min: 1,
                        max: 100,
                        step: 1,
                        wrap: false,
                    },
                    dest: Destination::Doc,
                    layer: None,
                },
                Field {
                    key: "interval".to_string(),
                    label: "Poll interval".to_string(),
                    kind: FieldKind::Text("30s".to_string()),
                    dest: Destination::Doc,
                    layer: None,
                },
            ],
            toml::Table::new(),
        )
    }

    /// A keystroke in an open plain field reaches the draft through the
    /// `Input`'s change subscription as `set_query`. The rows under a plain
    /// field stay unfiltered with the edited row highlighted (§19.1), so
    /// the cursor must stay on that row — the filter's "reset to the top
    /// match" rule is for a list that just re-ranked, and this one did
    /// not. Found on a display 2026-09-13: every keystroke after `i` sent
    /// the highlight back to the first row.
    #[test]
    fn a_keystroke_in_a_plain_field_keeps_the_cursor_on_the_edited_row() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(draft.selected, 1);
        draft.set_query("45".to_string());
        assert_eq!(draft.query, "45");
        assert_eq!(
            draft.selected, 1,
            "typing must not move the cursor off the field"
        );
        draft.set_query(String::new());
        assert_eq!(draft.selected, 1, "an emptied field is still the same row");
        // Without a field open the same call is the filter, and the filter
        // starts from the top match.
        draft.cancel_text_entry();
        draft.set_query("po".to_string());
        assert_eq!(draft.selected, 0);
    }

    #[test]
    fn begin_text_entry_seeds_the_query_from_a_number_row() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 0;
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(draft.query, "3");
        assert_eq!(
            draft.text_entry,
            Some(TextEntry {
                row: EditRow::Field(0),
                completions: false
            })
        );
        assert!(!draft.chain_entry(), "a plain field has no completion list");
    }

    #[test]
    fn begin_text_entry_seeds_the_query_from_a_text_row() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(draft.query, "30s");
    }

    #[test]
    fn begin_text_entry_is_inert_off_a_text_or_number_row() {
        let mut draft = Draft::new_object(
            "x",
            vec![Field {
                key: "on".to_string(),
                label: "On".to_string(),
                kind: FieldKind::Bool(true),
                dest: Destination::Doc,
                layer: None,
            }],
            toml::Table::new(),
        );
        assert_eq!(draft.begin_text_entry(), Step::Inert);
        assert_eq!(draft.text_entry, None);
    }

    #[test]
    fn applying_a_number_parses_and_refuses_out_of_range_without_clamping() {
        let ok = |_: &str, t: &str| Ok(t.to_string());
        let mut draft = draft_with_number_and_text();
        draft.selected = 0;
        draft.begin_text_entry();
        draft.query = "abc".to_string();
        assert!(
            matches!(draft.apply_text_entry(&ok), Step::Refused(r) if r.contains("whole number"))
        );
        assert!(draft.text_entry.is_some(), "a refusal keeps the field open");
        draft.query = "500".to_string();
        assert!(matches!(draft.apply_text_entry(&ok), Step::Refused(r) if r.contains("1 and 100")));
        draft.query = "42".to_string();
        assert_eq!(draft.apply_text_entry(&ok), Step::Changed);
        assert_eq!(draft.text_entry, None);
        assert!(matches!(
            draft.fields[0].kind,
            FieldKind::Number { value: 42, .. }
        ));
        assert!(draft.query.is_empty());
    }

    #[test]
    fn applying_text_goes_through_the_domains_parser_and_the_same_value_is_inert() {
        let parse = |key: &str, t: &str| -> Result<String, String> {
            if key == "interval" && t.ends_with('s') {
                Ok(t.trim().to_string())
            } else {
                Err("unit needed".to_string())
            }
        };
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        draft.begin_text_entry();
        // `t.ends_with('s')` is the fake parser's whole grammar, so the
        // refused input must actually fail that check — "2 minutes"
        // would pass it by accident (the word "minutes" itself ends in
        // 's'), which is not what this assertion is testing.
        draft.query = "2m".to_string();
        assert_eq!(
            draft.apply_text_entry(&parse),
            Step::Refused("unit needed".to_string())
        );
        draft.query = " 30s ".to_string();
        assert_eq!(
            draft.apply_text_entry(&parse),
            Step::Inert,
            "the trimmed value is the one already there"
        );
        assert_eq!(
            draft.text_entry, None,
            "an inert apply still closes the field"
        );
        draft.begin_text_entry();
        draft.query = "45s".to_string();
        assert_eq!(draft.apply_text_entry(&parse), Step::Changed);
        assert_eq!(draft.fields[1].kind, FieldKind::Text("45s".to_string()));
    }

    #[test]
    fn cancelling_text_entry_drops_the_text_and_leaves_the_value() {
        let mut draft = draft_with_number_and_text();
        draft.selected = 1;
        draft.begin_text_entry();
        draft.query = "garbage".to_string();
        draft.cancel_text_entry();
        assert_eq!(draft.text_entry, None);
        assert!(draft.query.is_empty());
        assert_eq!(draft.fields[1].kind, FieldKind::Text("30s".to_string()));
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(1)),
            "cancel leaves the cursor on the row it was editing, same as apply"
        );
    }

    /// Review round 1's Important: `query` inside an open plain field is
    /// the value being typed, not a filter, so [`Draft::visible_rows`]
    /// must not narrow the rows by it — narrowing would paint an empty
    /// list the moment a seeded `Number` (`"3"`) matches no row label,
    /// and would leave `selected` indexing a position the unfiltered
    /// list disagrees with. A filter applied before `i` is opened is
    /// lost with it (the seed overwrites `query`), the same rule the
    /// chain field already has — this test's `draft.query = "interval"`
    /// beforehand is there to prove exactly that: opening the field on
    /// the one row that filter left visible must still show every row
    /// underneath, not the one-row filtered list frozen in place.
    #[test]
    fn a_plain_field_leaves_the_rows_unfiltered_and_the_edited_row_selected() {
        let mut draft = Draft::new_object(
            "x",
            vec![
                Field {
                    key: "polls".to_string(),
                    label: "Stable polls".to_string(),
                    kind: FieldKind::Number {
                        value: 3,
                        min: 1,
                        max: 100,
                        step: 1,
                        wrap: false,
                    },
                    dest: Destination::Doc,
                    layer: None,
                },
                Field {
                    key: "interval".to_string(),
                    label: "Poll interval".to_string(),
                    kind: FieldKind::Text("30s".to_string()),
                    dest: Destination::Doc,
                    layer: None,
                },
                Field {
                    key: "on".to_string(),
                    label: "On".to_string(),
                    kind: FieldKind::Bool(true),
                    dest: Destination::Doc,
                    layer: None,
                },
            ],
            toml::Table::new(),
        );
        draft.query = "interval".to_string();
        draft.selected = 0;
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(1)),
            "the pre-existing filter's only surviving row"
        );

        assert_eq!(draft.begin_text_entry(), Step::Changed);
        assert_eq!(
            draft.visible_rows().len(),
            draft.rows().len(),
            "every edit row, not just the ones the old filter matched"
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(1)),
            "the row being edited is the one highlighted"
        );

        draft.cancel_text_entry();
        assert_eq!(
            draft.visible_rows().len(),
            draft.rows().len(),
            "still unfiltered after closing — the old filter does not come back"
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(1)),
            "cancel leaves the cursor where the field left it"
        );

        // Applying agrees: reopening and typing a valid value still
        // leaves the row list unfiltered and the cursor on that row.
        assert_eq!(draft.begin_text_entry(), Step::Changed);
        draft.query = "45s".to_string();
        let ok = |_: &str, t: &str| Ok(t.to_string());
        assert_eq!(draft.apply_text_entry(&ok), Step::Changed);
        assert_eq!(
            draft.visible_rows().len(),
            draft.rows().len(),
            "unchanged after applying too"
        );
        assert_eq!(draft.selected_row(), Some(EditRow::Field(1)));
    }

    #[test]
    fn row_for_path_matches_a_field_by_key_and_a_list_item_by_index() {
        let draft = Draft::new_object(
            "tree",
            vec![
                Field {
                    key: "dataset".into(),
                    label: "Dataset".into(),
                    kind: FieldKind::Text("risk".into()),
                    dest: Destination::Doc,
                    layer: None,
                },
                Field {
                    key: "columns".into(),
                    label: "Columns".into(),
                    kind: FieldKind::OrderedList {
                        items: vec![
                            ListItem {
                                name: "npv".into(),
                                included: true,
                                presentation: ColumnPresentation::default(),
                                kind: None,
                                note: None,
                            },
                            ListItem {
                                name: "delta".into(),
                                included: true,
                                presentation: ColumnPresentation::default(),
                                kind: None,
                                note: None,
                            },
                        ],
                        available: Some(vec![ListItem {
                            name: "vega".into(),
                            included: false,
                            presentation: ColumnPresentation::default(),
                            kind: None,
                            note: None,
                        }]),
                    },
                    dest: Destination::Doc,
                    layer: None,
                },
            ],
            toml::Table::new(),
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.dataset"),
            Some(EditRow::Field(0))
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.1.format.precision"),
            Some(EditRow::Item { field: 1, item: 1 })
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns"),
            Some(EditRow::Field(1))
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.7"),
            Some(EditRow::Field(1)),
            "an index off the list lands on the field"
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.2"),
            Some(EditRow::Field(1)),
            "an index exactly at the list's length is still out of bounds \
             (there is no items[2] when len() == 2) — the off-by-a-lot case \
             above cannot tell `<` from `<=` on its own"
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree"),
            None,
            "object-level stays on the header"
        );
        assert_eq!(draft.row_for_path("views", "views.other.dataset"), None);
        assert_eq!(draft.row_for_path("views", "sources.tree.dataset"), None);
        assert_eq!(
            draft.row_for_path("views", "views.tree.nonexistent"),
            None,
            "a field key nothing on this object has stays on the header, \
             not on whichever field happens to be first"
        );
    }

    /// §19.5, review round 1's Important-2 finding: a reader's diagnostic
    /// index is a position in `Draft::source`'s own array — for Views,
    /// `views.toml`'s definitional column order, the same order
    /// `views::columns_for` reads to write the file back — which is NOT
    /// `items`' order once `ViewPresentation::apply` (or a fresh drag)
    /// has permuted `items` into the trader's personal presentation. This
    /// fixture: `source.columns` is `[npv, delta]` (npv first, as
    /// `views.toml` itself declares them), but `items` has been reordered
    /// to `[delta, npv]`. A diagnostic path index of `1` names
    /// `source.columns[1]`, which is `delta` — resolving it by name to
    /// delta's CURRENT position in `items` (`0`) is the fix; resolving it
    /// as a raw index into `items` (the pre-fix behaviour) would instead
    /// land on `items[1]`, which is `npv` — the wrong column entirely.
    #[test]
    fn row_for_path_resolves_a_reordered_list_index_by_name() {
        let mut source = toml::Table::new();
        source.insert(
            "columns".to_string(),
            toml::Value::Array(vec![
                toml::Value::Table(toml::Table::from_iter([(
                    "name".to_string(),
                    toml::Value::String("npv".to_string()),
                )])),
                toml::Value::Table(toml::Table::from_iter([(
                    "name".to_string(),
                    toml::Value::String("delta".to_string()),
                )])),
            ]),
        );
        let draft = Draft::new_object(
            "tree",
            vec![Field {
                key: "columns".into(),
                label: "Columns".into(),
                kind: FieldKind::OrderedList {
                    items: vec![
                        ListItem {
                            name: "delta".into(),
                            included: true,
                            presentation: ColumnPresentation::default(),
                            kind: None,
                            note: None,
                        },
                        ListItem {
                            name: "npv".into(),
                            included: true,
                            presentation: ColumnPresentation::default(),
                            kind: None,
                            note: None,
                        },
                    ],
                    available: None,
                },
                dest: Destination::Doc,
                layer: None,
            }],
            source,
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.1.format.precision"),
            Some(EditRow::Item { field: 0, item: 0 }),
            "source index 1 (delta) resolves to delta's current position \
             in items (0), not to whatever now sits at raw index 1 (npv)"
        );
    }

    /// §19.5, review round 1's Minor-4: two diagnostics on the same row —
    /// a Warning and an Error — must show the WORSE of the two, since the
    /// glyph is one colour per row and a trader must never see a mild
    /// warning colour when an error is also standing on that row.
    #[test]
    fn flagged_rows_promotes_a_warning_to_error_on_the_same_row() {
        let mut draft = Draft::new_object(
            "tree",
            vec![Field {
                key: "dataset".into(),
                label: "Dataset".into(),
                kind: FieldKind::Text("risk".into()),
                dest: Destination::Doc,
                layer: None,
            }],
            toml::Table::new(),
        );
        draft.diagnostics = vec![
            Diagnostic {
                severity: Severity::Warning,
                layer: None,
                file: None,
                message: "a warning on dataset".into(),
                path: Some("views.tree.dataset".into()),
            },
            Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: "an error on dataset too".into(),
                path: Some("views.tree.dataset".into()),
            },
        ];
        assert_eq!(
            draft.flagged_rows("views"),
            vec![(EditRow::Field(0), Severity::Error)],
            "the row's severity is the worse of the two, regardless of order"
        );
    }

    /// The edit footer's row-sensitive question (user ruling 2026-09-13,
    /// superseding the 2026-09-12 "i for edit text isn't discoverable"
    /// answer, which asked whether the OBJECT had such a row anywhere
    /// and so put the `i` chip on `Choice` rows `i` refuses): what the
    /// row under the CURSOR answers to. Every arm, because the footer
    /// paints a different pair of chip groups for each.
    #[test]
    fn selected_vocabulary_answers_for_the_row_under_the_cursor() {
        let field = |key: &str, kind: FieldKind| Field {
            key: key.to_string(),
            label: key.to_string(),
            kind,
            dest: Destination::Doc,
            layer: None,
        };
        let choice = |options: &[&str]| FieldKind::Choice {
            options: options.iter().map(|o| (*o).to_string()).collect(),
            selected: 0,
        };

        // A `Text` row takes `i` where the domain marks the key editable
        // and offers nothing at all where it does not — the same
        // `Domain::text_editable` door `open_text_field` asks.
        let text = Draft::new_object(
            "live",
            vec![field("poll_interval", FieldKind::Text("2s".to_string()))],
            toml::Table::new(),
        );
        assert_eq!(
            text.selected_vocabulary(Domain::Sources),
            RowVocabulary::Types
        );
        assert_eq!(
            text.selected_vocabulary(Domain::Views),
            RowVocabulary::Inert,
            "the same key is read-only on Views, where i only refuses"
        );

        // A `Number` is the one row that does both.
        let number = Draft::new_object(
            "live",
            vec![field(
                "polls",
                FieldKind::Number {
                    value: 3,
                    min: 1,
                    max: 100,
                    step: 1,
                    wrap: false,
                },
            )],
            toml::Table::new(),
        );
        assert_eq!(
            number.selected_vocabulary(Domain::Views),
            RowVocabulary::StepsAndTypes,
            "a Number steps and takes a typed value on any domain"
        );

        // `Choice` and `Bool` step and nothing more — except a `Choice`
        // with one option, which steps nowhere in either direction.
        let steps = Draft::new_object(
            "live",
            vec![
                field("dataset", choice(&["risk", "vol"])),
                field("thousands", FieldKind::Bool(true)),
                field("only", choice(&["risk"])),
            ],
            toml::Table::new(),
        );
        assert_eq!(
            steps.selected_vocabulary(Domain::Sources),
            RowVocabulary::Steps
        );
        let mut on_bool = steps.clone();
        on_bool.selected = 1;
        assert_eq!(
            on_bool.selected_vocabulary(Domain::Sources),
            RowVocabulary::Steps
        );
        let mut on_lone = steps.clone();
        on_lone.selected = 2;
        assert_eq!(
            on_lone.selected_vocabulary(Domain::Sources),
            RowVocabulary::Inert,
            "a one-option Choice steps nowhere, so the footer must not say it does"
        );

        // The list rows: the header itself has no value, an item ticks
        // and an available row adds.
        let mut list = two_column_draft_with_one_available();
        assert_eq!(
            list.rows(),
            vec![
                EditRow::Field(0),
                EditRow::Field(1),
                EditRow::Item { field: 1, item: 0 },
                EditRow::Item { field: 1, item: 1 },
                EditRow::Available { field: 1, item: 0 },
            ],
            "sanity: the fixture's row order, which the indices below rely on"
        );
        list.selected = 1;
        assert_eq!(
            list.selected_vocabulary(Domain::Views),
            RowVocabulary::Inert,
            "an OrderedList's own header row has no value to change"
        );
        list.selected = 2;
        assert_eq!(list.selected_vocabulary(Domain::Views), RowVocabulary::Item);
        list.selected = 4;
        assert_eq!(
            list.selected_vocabulary(Domain::Views),
            RowVocabulary::Available
        );

        // No row at all — an over-narrow filter — is inert, not a panic.
        let empty = Draft::new_object("x", Vec::new(), toml::Table::new());
        assert_eq!(
            empty.selected_vocabulary(Domain::Sources),
            RowVocabulary::Inert
        );
    }

    /// Part 2c §5.2: the column stage is a PROJECTION over the same draft
    /// — the view's fields are stashed, the column's seven installed, and
    /// the view's own list stays reachable underneath (which is what lets
    /// the overlay writer keep rendering from it mid-stage). A step there
    /// folds onto the item, writes presentation alone, and leaving
    /// restores the view with the cursor back on the column.
    #[test]
    fn entering_a_column_swaps_the_fields_and_leaving_restores_them_with_the_fold() {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let parent_len = draft.fields.len();
        let npv = draft
            .list_items("columns")
            .unwrap()
            .iter()
            .find(|i| i.name == "npv")
            .unwrap()
            .clone();
        // The context the door installs (dataset-presentation spec
        // §5.1): without it the stage opens but folds nothing. Through
        // the door's OWN builder, so this mirror of
        // `render::enter_column_stage` cannot drift from it.
        draft.column_ctx = Some(views::column_context(&draft, "npv", npv.clone()));
        assert!(draft.enter_column(
            "npv",
            views::column_fields(&npv, &[], Destination::Presentation)
        ));
        assert_eq!(draft.column(), Some("npv"));
        assert_eq!(draft.fields.len(), 7);
        assert!(
            draft.list_items("columns").is_some(),
            "the view's list is still reachable through the parent"
        );
        assert_eq!(draft.choice("dataset"), Some("risk"), "so is the dataset");
        let i = draft.fields.iter().position(|f| f.key == "scale").unwrap();
        draft.selected = i;
        assert_eq!(draft.toggle_selected(), Step::Changed);
        draft.fold_column();
        assert_eq!(
            draft.list_items("columns").unwrap()[0].presentation.scale,
            Some(geode_core::view::Scale::Thousands)
        );
        assert!(
            draft
                .writes_by_destination()
                .contains_key(&Destination::Presentation)
        );
        assert!(
            !draft
                .writes_by_destination()
                .contains_key(&Destination::Doc)
        );
        draft.mark_saved();
        draft.leave_column();
        assert_eq!(draft.column(), None);
        assert_eq!(draft.fields.len(), parent_len);
        assert!(
            !draft.is_dirty(),
            "leaving after a committed change is clean"
        );
        assert!(
            matches!(draft.selected_row(), Some(EditRow::Item { .. })),
            "cursor back on the column"
        );
        assert!(!draft.enter_column("ghost", Vec::new()), "not a member");
    }

    /// M-1 (Part 2c final review): a second `enter_column` while a column
    /// stage is already open is refused, changing nothing.
    ///
    /// Unreachable through the dialog today, which is exactly why the
    /// guard is worth its line: the membership test below it passes
    /// through `field_by_key`'s parent fallback, so a re-entry would
    /// stash the SEVEN installed column fields as `parent_fields` and
    /// drop the view's own `columns` list forever — and the next write
    /// would render a view with no columns at all. The assertions are on
    /// that list surviving, not merely on the `false`.
    #[test]
    fn enter_column_refuses_re_entry_and_keeps_the_objects_own_list() {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let item = draft.list_items("columns").unwrap()[0].clone();
        assert!(draft.enter_column(
            "npv",
            views::column_fields(&item, &[], Destination::Presentation)
        ));
        let installed = draft.fields.len();
        let names = |draft: &Draft| {
            draft
                .list_items("columns")
                .map(|items| items.iter().map(|i| i.name.clone()).collect::<Vec<_>>())
        };
        let before = names(&draft);
        assert_eq!(
            before.as_deref(),
            Some(&["npv".to_string(), "book".to_string()][..])
        );

        assert!(
            !draft.enter_column("npv", Vec::new()),
            "re-entry on the open column is refused"
        );
        assert!(
            !draft.enter_column("book", Vec::new()),
            "re-entry on another member is refused too — it is the open \
             stage that forbids this, not the name"
        );
        assert_eq!(draft.column(), Some("npv"));
        assert_eq!(
            draft.fields.len(),
            installed,
            "the seven fields still stand"
        );
        assert_eq!(
            names(&draft),
            before,
            "the view's own column list survived the refused re-entry"
        );
    }

    /// The Values stage is a projection like the column stage: entering
    /// swaps the fields, leaving restores them and hands the stage's own
    /// fields back so the adapter can fold them.
    #[test]
    fn entering_values_swaps_the_fields_and_leaving_restores_them() {
        let mut draft = groupings_draft();
        let before = draft.fields.clone();
        let values = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![item("BK001")],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values.clone()));
        assert_eq!(draft.values(), Some("book"));
        assert_eq!(draft.fields, values);
        assert!(!draft.is_dirty(), "freshly installed values are not dirt");
        // Re-entry is refused, as `enter_column` refuses it.
        assert!(!draft.enter_values("lhu", Vec::new()));
        let own = draft.leave_values().expect("the stage's fields");
        assert_eq!(own, values);
        assert_eq!(draft.values(), None);
        assert_eq!(draft.fields, before);
    }

    /// In the Values stage the last ticked value may be unticked — an
    /// emptied selection is "drop this dimension", not an invalid object
    /// — where the same untick on a Groupings chain is refused.
    #[test]
    fn the_last_tick_may_be_removed_in_the_values_stage_alone() {
        let mut draft = groupings_draft();
        let values = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![ListItem {
                    included: true,
                    ..item("BK001")
                }],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values));
        draft.selected = 1; // the one item row under the header
        assert_eq!(draft.toggle_selected(), Step::Changed);
        assert!(!draft.list_items("values").unwrap()[0].included);
    }

    /// A stage with a previous rung: `escape` from Values steps back.
    #[test]
    fn values_is_a_stage_escape_can_step_back_from() {
        let mut state = ObjectDialogState::new(Domain::Scopes);
        state.stage = Stage::Values {
            object: "mine".into(),
            column: "book".into(),
        };
        assert!(state.has_previous_stage());
    }

    /// The Column and Values stages share one stash (`Draft::values`'s own
    /// doc: "the two stages share the stash and can never both be open"),
    /// so `enter_column` must refuse exactly as re-entry on itself does —
    /// entering over an open Values stage would stash ITS installed
    /// fields as `parent_fields` and drop the object's own list for good.
    #[test]
    fn enter_column_is_refused_while_the_values_stage_is_open() {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let column_item = draft.list_items("columns").unwrap()[0].clone();
        let column_fields = views::column_fields(&column_item, &[], Destination::Presentation);
        let values_fields = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![item("BK001")],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values_fields.clone()));
        assert!(
            !draft.enter_column("npv", column_fields),
            "the values stage already holds the shared stash"
        );
        assert_eq!(
            draft.fields, values_fields,
            "the refused enter_column touched nothing"
        );
        assert_eq!(draft.values(), Some("book"));
        assert_eq!(draft.column(), None);
    }

    /// `leave_column` is the Column stage's own door — called while the
    /// Values stage holds the shared stash, it must do nothing rather
    /// than take a stash that belongs to the other stage (the review
    /// finding this test and `enter_column_is_refused_while_the_values_stage_is_open`
    /// close): the checked discriminant is `column`, which is `None`
    /// while Values is open.
    #[test]
    fn leave_column_does_nothing_while_the_values_stage_is_open() {
        let mut draft = groupings_draft();
        let before = draft.fields.clone();
        let values = vec![Field {
            key: "values".to_string(),
            label: "Values".to_string(),
            kind: FieldKind::OrderedList {
                items: vec![item("BK001")],
                available: None,
            },
            dest: Destination::Doc,
            layer: None,
        }];
        assert!(draft.enter_values("book", values.clone()));
        draft.leave_column();
        assert_eq!(
            draft.values(),
            Some("book"),
            "leave_column must not touch the Values stage"
        );
        assert_eq!(draft.fields, values, "leave_column touched nothing");
        let own = draft
            .leave_values()
            .expect("the stage's own fields survive leave_column's no-op");
        assert_eq!(own, values);
        assert_eq!(draft.values(), None);
        assert_eq!(draft.fields, before);
    }

    /// Part 2c §5.5: a diagnostic path whose index resolves — by name, 2b's
    /// rule — to the OPEN column lands on the row keyed by its format key;
    /// a path naming any other column, or the view itself, lands on
    /// nothing while this stage is open (those stay on the header).
    #[test]
    fn row_for_path_in_the_column_stage_lands_on_the_format_key() {
        let config = config_with_view_and_datasets();
        let mut draft = Domain::Views.draft(&config, "tree");
        let item = draft.list_items("columns").unwrap()[0].clone();
        draft.enter_column(
            "npv",
            views::column_fields(&item, &[], Destination::Presentation),
        );
        let precision = draft
            .fields
            .iter()
            .position(|f| f.key == "precision")
            .unwrap();
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.0.format.precision"),
            Some(EditRow::Field(precision))
        );
        assert_eq!(
            draft.row_for_path("views", "views.tree.columns.1.format.precision"),
            None,
            "another column's path lands nowhere here"
        );
        assert_eq!(draft.row_for_path("views", "views.tree.dataset"), None);
    }
}
