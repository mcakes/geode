//! Shared scaffold for browsing and editing typed configuration objects.
//!
//! Each domain supplies names, fields, validation, folding, and destinations;
//! the scaffold owns selection, normal/filter modes, the field editor, confirm
//! state, and user-layer persistence. Traders edit objects and fields rather
//! than raw TOML. The writable layer and destination are explicit data, so an
//! adapter cannot silently write inherited desk configuration.
//!
//! The browse stage lists objects. The edit stage holds a typed draft and may
//! open nested choice, ordered-list, or values stages. Pure dialog state is the
//! source of truth; the shared input is synchronized only through the dialog
//! focus seam.

pub mod apply;
mod colours;
mod dataset_columns;
mod groupings;
pub mod render;
mod schema;
mod scopes;
mod sources;
mod views;

/// `ShellView::deliver_distinct` routes a `SCOPES_KEY` outcome to the Values
/// stage's own delivery. Scoped to this crate's shell because nothing outside
/// it needs the door.
pub(in crate::shell) use render::deliver_values;

use std::collections::{BTreeMap, BTreeSet};

use geode_core::config::{Config, Diagnostic, Layer, Severity};
use geode_core::view::ColumnPresentation;

use crate::dialogmode::{self, DialogMode};
use crate::vimnav::{self, NavCommand};

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
    /// Read-only: the datasets the other adapters build their choices from.
    Schema,
    /// The ingest feeds, one object per source.
    Sources,
    /// The shared color vocabulary a column's `colour` field and a chart series can
    /// name — one object per named color, a hue (with its tone) or a theme token.
    Colors,
}

/// The current stage. Nested stages give Escape a previous stage to return to; mutable
/// fields and selection remain on `ObjectDialogState` and `Draft`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Browse,
    /// Naming an object: Enter creates it and Escape returns to browsing without a
    /// write. The shared input holds the proposed name.
    Naming,
    /// Editing one object's fields. The draft itself lives in
    /// [`ObjectDialogState::draft`] rather than in here, because the
    /// *name* is what identifies the stage (it is what `escape` restores
    /// the browse selection to) while the draft is mutable state that a
    /// `PartialEq` stage comparison has no business walking.
    Edit {
        object: String,
    },
    /// One column's presentation projected over the same draft. Parent fields are
    /// stashed while its seven presentation fields are installed; each edit folds back
    /// into the column, and Escape restores the parent selection by name.
    Column {
        object: String,
        column: String,
    },
    /// One saved-scope dimension's distinct values projected over the same draft.
    /// Escape restores the scope's fields and selects that dimension.
    Values {
        object: String,
        column: String,
    },
}

/// Source for a newly named object: the domain's defaults, a named saved scope, or
/// current frame scope. Copy targets are recorded by name because a reload can reorder
/// the browse list before creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSeed {
    Empty,
    CopyOf(String),
    FromFrame,
}

/// A browse row with its effective layer, user override status, and recorded drift from
/// the inherited definition.
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
    /// Whether the inherited definition differs from its recorded value at fork time.
    /// Missing or stale `overrides.toml` entries report no drift. Comparing the user's
    /// edited definition against the current inherited one would incorrectly flag
    /// deliberate customisations.
    pub drifted: bool,
    /// A grouping key painted before the name, dimmed: the dataset a source feeds.
    /// `Some` only on Sources; the primary sort key when present, part of
    /// `searchable_text`, never the identity — the doc key is still `name`, so a
    /// dataset with two sources is two rows and every click handler and selector stays
    /// keyed by `name`.
    pub prefix: Option<String>,
}

impl ObjectRow {
    /// What the browse row paints as its label: `"<prefix> · <name>"` for a prefixed
    /// row, the bare name otherwise. The one spelling of that join, shared by the
    /// painted label (`render.rs`'s browse painter) and [`searchable_text`], so a hit
    /// inside the prefix ranks and highlights against the exact text on screen.
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
            Domain::Colors => colours::DOC,
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
            Domain::Colors => "Colors",
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
            Domain::Colors => "colors",
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
            Domain::Colors => colours::summary,
        }
    }

    /// The overlay document used to personalise this domain without copying its
    /// definition. Only Views has this object-level overlay. Schema column editing uses
    /// `DatasetPresentation` separately; other domains write definitions.
    fn presentation_doc(self) -> Option<&'static str> {
        match self {
            Domain::Views => Some(views::PRESENTATION_DOC),
            // These domains have no view-presentation overlay.
            Domain::Groupings
            | Domain::Scopes
            | Domain::Schema
            | Domain::Sources
            | Domain::Colors => None,
        }
    }

    /// Always-listed names, including unconfigured slots. Groupings reserves 1–9 and
    /// therefore offers no creation verb. Slot 0 restores a view's own grouping and is
    /// not a configurable slot. This is separate from write permission.
    pub(super) fn roster(self) -> Option<&'static [&'static str]> {
        match self {
            Domain::Groupings => Some(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]),
            Domain::Views | Domain::Scopes | Domain::Schema | Domain::Sources | Domain::Colors => {
                None
            }
        }
    }

    /// Whether this stage permits mutation. Schema is writable only in its column
    /// stage, where edits target dataset presentation rather than the schema doc.
    /// Creation additionally requires a domain without a fixed roster.
    pub fn writable(self, stage: &Stage) -> bool {
        !matches!(self, Domain::Schema) || matches!(stage, Stage::Column { .. })
    }

    /// May `c` copy an object under a new name? Scopes alone for now; the mechanism is
    /// generic.
    pub fn duplicable(self) -> bool {
        self == Domain::Scopes
    }

    /// The text painted before an object's name, if this domain groups its objects.
    /// `None` on every domain but Sources — a source's row leads with the dataset it
    /// feeds (`sources::prefix`), painted dimmed ahead of the name and used as the
    /// primary sort key in [`derive_rows`]; every other domain's objects are already
    /// uniquely named with nothing to group them by.
    fn prefix_fn(self) -> Option<fn(&toml::Value) -> Option<String>> {
        match self {
            Domain::Sources => Some(sources::prefix),
            Domain::Views
            | Domain::Groupings
            | Domain::Scopes
            | Domain::Schema
            | Domain::Colors => None,
        }
    }

    /// Derive every domain's rows through the shared layer/override walk. Adapters
    /// supply document, summary, prefix, roster, and presentation metadata; they do not
    /// independently decide whether a destructive revert is valid.
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

    /// Names reserved by syntax outside the domain's own document. Colors excludes
    /// `none` and `sign`, which already mean built-in formatting choices. Scopes
    /// excludes the `save_current` action name to avoid ambiguous palette dispatch.
    /// Other domains have no additional reserved names.
    pub fn reserved_names(self) -> &'static [&'static str] {
        match self {
            Domain::Colors => &geode_core::colour::RESERVED_NAMES,
            Domain::Scopes => &geode_core::scopes::RESERVED_NAMES,
            Domain::Views | Domain::Groupings | Domain::Schema | Domain::Sources => &[],
        }
    }

    /// Whether `name` is reserved in this domain: one of [`Self::reserved_names`],
    /// or — for colors only — any name starting with `#`, which spells an
    /// absolute `#rrggbb` color wherever a color name is also read
    /// (`geode_core::colour::RESERVED_PREFIX`; the reader drops such a name too).
    pub fn is_reserved(self, name: &str) -> bool {
        self.reserved_names().contains(&name)
            || (self == Domain::Colors && name.starts_with(geode_core::colour::RESERVED_PREFIX))
    }

    /// Whether a name is already present in a definition, fixed roster, or user
    /// presentation overlay. Include orphaned overlays so a new object cannot silently
    /// inherit their old personalisation. `config_version` is rejected separately by
    /// `check_object_name`.
    pub fn name_taken(self, config: &Config, name: &str) -> bool {
        if self.is_reserved(name) {
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

/// User-layer `overrides.toml` records inherited definitions at fork time. Entries are
/// keyed by document and object name and contain canonical object text for later drift
/// comparison.
pub const OVERRIDES_DOC: &str = "overrides";

/// The sidecar's own key for `object` in `doc` — `"<doc>.<object>"`, the
/// join every reader and writer of `OVERRIDES_DOC` uses to name an entry.
pub fn override_key(doc: &str, object: &str) -> String {
    format!("{doc}.{object}")
}

/// The entry recorded when `object` is forked over `shadowed`'s copy. The canonical
/// text, not a hash: `DefaultHasher` is not stable across Rust versions, a crypto
/// dependency is unjustified, and keeping the text makes a real diff free if it is ever
/// wanted.
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

/// The sidecar key as the current document name spells it: an entry recorded
/// before a document rename (`colours.<name>`) names the renamed document
/// (`colors.<name>`), so its fork baseline still counts.
fn current_override_key(raw: &str) -> String {
    raw.split_once('.')
        .and_then(|(doc, object)| {
            geode_core::config::renamed_doc(doc).map(|new| override_key(new, object))
        })
        .unwrap_or_else(|| raw.to_string())
}

/// The overrides sidecar's well-formed user-layer entries under their spelling in
/// the file, as `(raw key, (shadowed_layer, shadowed_text))`.
fn raw_override_entries(config: &Config) -> Vec<(String, (String, String))> {
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

/// The overrides sidecar's own entries, user layer only, as `key ->
/// (shadowed_layer, shadowed_text)` under current document names; a
/// current-spelled key wins over an old-spelled one for the same object.
/// Private: every reader outside this module goes through
/// [`stale_override_keys`] or `derive_rows`'s own drift computation, never
/// the raw map.
fn override_entries(config: &Config) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for (raw, entry) in raw_override_entries(config) {
        let key = current_override_key(&raw);
        if key == raw {
            out.insert(key, entry);
        } else {
            out.entry(key).or_insert(entry);
        }
    }
    out
}

/// The sidecar keys, as spelled in the file, recording `doc.object` — what
/// [`render::removal_edits`] removes beside a delete/revert, so a missing
/// sidecar is never created just to remove nothing from it, and an entry
/// recorded under an old document name goes with its object.
pub(super) fn override_keys_of(config: &Config, doc: &str, object: &str) -> Vec<String> {
    let key = override_key(doc, object);
    raw_override_entries(config)
        .into_iter()
        .map(|(raw, _)| raw)
        .filter(|raw| current_override_key(raw) == key)
        .collect()
}

/// Entries that describe nothing any more: the user layer no longer holds the object,
/// or no layer beneath shadows it, or an old-spelled key duplicates a current one.
/// Ignored by `derive_rows` and pruned by the next overrides write. Keys are
/// returned as the file spells them, so the prune removes them.
pub fn stale_override_keys(config: &Config) -> Vec<String> {
    let raws: Vec<String> = raw_override_entries(config)
        .into_iter()
        .map(|(raw, _)| raw)
        .collect();
    raws.iter()
        .filter(|raw| {
            let key = current_override_key(raw);
            if key != **raw && raws.contains(&key) {
                return true;
            }
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

/// The gate [`derive_rows`] applies to compute [`ObjectRow::drifted`]: drift is
/// provable only from the sidecar's recorded text, so no entry means not drifted, never
/// a guess from the shadow's current copy.
fn drift_of(entry: Option<&(String, String)>, shadow: Option<&toml::Value>, name: &str) -> bool {
    match (entry, shadow) {
        (Some((_, recorded)), Some(value)) => {
            object_text(name, toml_value_to_item(value)) != *recorded
        }
        _ => false,
    }
}

/// Build browse rows from layered object definitions and the optional fixed roster.
/// Skip `config_version`. Unconfigured roster entries have no layer and an `empty`
/// summary; configured entries use the last defining layer.
///
/// `overridden` requires an inherited definition plus a user definition or user
/// presentation overlay. That inherited definition ensures reverting has something to
/// restore. Drift compares the recorded fork baseline with the current inherited
/// definition, not with the user's edited copy.
///
/// Rows sort by name, or by `(prefix, name)` for Sources so sources cluster by dataset.
/// Presentation-only names affect override status but do not create browse rows without
/// a definition.
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
            // drift is "the shadowed copy moved since the fork" — provable only from
            // the sidecar's recorded text, so no entry means not drifted, never a
            // guess.
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

/// The user-layer document a field edits. Definition changes replace the whole named
/// object and stop inheriting changes from lower layers. View presentation changes
/// retain that inheritance by writing a separate overlay.
///
/// Dataset selection and column membership are definitional. Order, inclusion, width,
/// and formatting are presentation. Schema's column stage writes dataset presentation.
/// `writes_by_destination` groups changed fields for the flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    /// The domain's own doc, user layer.
    Doc,
    /// `view_presentation.toml`, user layer.
    Presentation,
    /// `dataset_presentation.toml`, user layer — the Schema dialog's column stage.
    DatasetPresentation,
}

impl Destination {
    /// The config doc (file stem) this destination writes, for `domain`.
    pub fn doc(self, domain: Domain) -> &'static str {
        match (self, domain) {
            (Destination::Doc, domain) => domain.doc(),
            (Destination::Presentation, Domain::Views) => views::PRESENTATION_DOC,
            // Every Groupings field is `Destination::Doc` there is nothing
            // presentational about a dimension chain), so this arm exists only to keep
            // the match exhaustive as domains are added, not because anything can reach
            // it.
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
            // Colors joins the same list: `colours.rs`'s module doc has
            // the reasoning (every field is `Destination::Doc`, there is
            // no presentation overlay for a shared color).
            (Destination::Presentation, Domain::Colors) => {
                unreachable!("Colors has no Presentation-destined fields")
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

/// An ordered-list member or available candidate. Its containing list determines
/// membership; `included` determines the tick state within that list.
///
/// View items carry effective presentation keys from the definition and overlays;
/// formatting resolves remaining unset keys through the column-kind default. Domains
/// without column presentation use an empty `ColumnPresentation`.
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

/// The supported field shapes. Adapters supply choices, bounds, and validation. Options
/// can retain stale configured values so users can inspect and repair them. Text
/// editing is separately permitted by the domain; not every displayed text field is
/// writable.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text(String),
    /// `step` is the distance one `space` moves; `wrap` makes the range circular — a
    /// hue, say — so a step past `max` lands at `min + overshoot` rather than pinning
    /// at `max`. Every `Number` this crate builds today is `step: 1, wrap: false` (the
    /// only one live is Sources' `stable_polls`; Groupings' `slot` is display-only and
    /// a [`FieldKind::Text`], not a `Number`, per `groupings.rs`'s own doc) — a future
    /// field that steps by more than one, or wraps, is what this pair exists for.
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
    /// The object's ordered items and an optional catalogue of candidates. Only `items`
    /// contributes to writes. `available: None` means ticking controls membership
    /// within one list, as in Groupings. `Some`, even when empty, means items can be
    /// promoted from or demoted into a separate catalogue.
    ///
    /// The distinction keeps `x` usable after all available view columns are added; an
    /// empty catalogue still accepts a removed member.
    OrderedList {
        items: Vec<ListItem>,
        available: Option<Vec<ListItem>>,
    },
}

/// The layer supplying a column-stage value: per-view override, dataset presentation,
/// or the view definition. No badge means the kind default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Desk,
    Dataset,
    View,
}

impl Provenance {
    pub const fn name(self) -> &'static str {
        match self {
            Provenance::Desk => "desk",
            Provenance::Dataset => "dataset",
            Provenance::View => "view",
        }
    }
}

/// Which door opened the column stage: the Views dialog's member row (the fields write
/// the view overlay over a desk + dataset baseline) or the Schema dialog's column row
/// (the fields write the dataset overlay over the kind default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDoor {
    View,
    Dataset,
}

/// Definition and dataset presentation keys kept separately for provenance and clear
/// notices. View provenance compares current fields with their merged baseline rather
/// than consulting a possibly stale saved overlay key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnLayers {
    pub desk: ColumnPresentation,
    pub dataset: ColumnPresentation,
}

impl ColumnLayers {
    /// desk with the dataset level merged over — what a cleared VIEW key falls to, and
    /// what the view writer compares against.
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
    /// The Schema door only: the `[<dataset>]` table of `dataset_presentation.toml` as
    /// it stands (empty when absent), so the writer can render the dataset's OTHER
    /// personalised columns verbatim beside the one being edited.
    pub overlay_object: toml::Table,
    /// The Schema door only: the scratch item the fields fold into,
    /// where the Views door folds into its parent list's item.
    pub item: Option<ListItem>,
}

/// One editable property of one object.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The TOML key within the object (`dataset`, `columns`). Also what a diagnostic's
    /// key path is matched against once readers carry one see [`Draft::diagnostics`]).
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub dest: Destination,
    /// The layer this row's value came from, painted as a badge on the row when `Some`.
    /// Filled by the Schema adapter from `Config::explain`; every writable domain
    /// leaves it `None`, since the object-level badge in the header already says whose
    /// copy is on screen and a second badge per row would only repeat it.
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

/// Operations available on the selected row. Footers and value buttons use this same
/// vocabulary so they advertise only actions the row can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowVocabulary {
    /// Nothing on this row changes and nothing types: a display-only
    /// `Text` (Groupings' `slot`, Scopes' two summaries, every Schema
    /// row), an `OrderedList`'s own header row, a `MultiChoice`, or no
    /// row at all under an over-narrow filter.
    Inert,
    /// A value the step keys cycle and `i` cannot open: `Bool`.
    Steps,
    /// Both: a `Number` (steps by one, takes a typed value) or a
    /// multi-option `Choice` (steps, and `i` opens a typeahead over its
    /// options).
    StepsAndTypes,
    /// `i` alone: a `Text` row the domain marks editable.
    Types,
    /// One of the object's own list entries — `space` ticks it.
    Item,
    /// A catalogue row — `space` adds it to the object's list.
    Available,
}

/// What a dragged list row carries: the field's key, which block it came from, and the
/// item's NAME — never an index. The keyboard stays live during a drag, so a keystroke
/// can reorder or remove between the grab and the drop; a payload resolved by name at
/// drop time lands on the row the trader picked up, or on nothing, never on whichever
/// column now holds the grabbed index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDrag {
    pub field: String,
    /// Whether this payload names an object member or an available candidate.
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
    /// Confirm replacement of a user-owned saved scope with current frame scope. An
    /// inherited scope instead receives an announced user-layer fork without a
    /// confirmation, leaving the lower-layer definition available to revert.
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

/// Rows shown under an open field. Plain entry retains unfiltered field rows; Chain
/// shows dimension completions; Choice shows options for typeahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completions {
    None,
    Chain,
    Choice,
}

/// An open value field using the shared input. Enter validates/applies and Escape
/// cancels typed text. `completions` selects plain, chain, or choice routing; `row`
/// identifies the edited draft row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEntry {
    pub row: EditRow,
    pub completions: Completions,
}

/// The object's edit buffer and the source of the painted field values. Changed fields
/// appear immediately; `apply::commit_edit` queues their rendered objects for the
/// shared flush. Baselines track changes already queued, not disk acknowledgement.
/// Column and Values stages project over this same draft.
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
    /// Source at the same point as the field baseline. Compare it directly as well as
    /// fields: different scope selections can produce identical summary strings, so
    /// visible equality does not establish that the persisted object is unchanged.
    baseline_source: toml::Table,
    /// Cursor over [`Draft::visible_rows`] — the FILTERED list, not [`Draft::rows`] —
    /// the same convention `ObjectDialogState::selected` holds for the browse stage.
    /// One cursor per stage, never both live at once.
    pub selected: usize,
    /// The edit stage's filter, mirrored from the shared `Input` by
    /// `ObjectDialogState::set_query` exactly as the browse query is. Lives on the
    /// draft rather than beside it because `selected` indexes the FILTERED list and
    /// both must move together.
    pub query: String,
    /// Current adapter diagnostics, refreshed after changes. The header displays all of
    /// them; diagnostics with resolvable paths also mark their field rows. Error
    /// severity blocks value edits, while warnings remain editable.
    pub diagnostics: Vec<Diagnostic>,
    /// An open field makes `query` its typed buffer rather than the stage filter.
    /// Closing it clears that buffer; no previous filter is restored.
    pub text_entry: Option<TextEntry>,
    /// The typeahead list while a `Choice` row's field is open
    /// (`text_entry.completions == Completions::Choice`), `None`
    /// otherwise. Owns the ranking and the highlight; `selected` stays on
    /// the field's own row throughout, as it does for a plain field.
    pub choice: Option<crate::choice::ChoiceList>,
    /// The object's OWN fields, while [`Stage::Column`] has swapped `fields` out for
    /// one column's seven. `None` everywhere else.
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
    /// Which column [`Draft::parent_fields`] was stashed for by the VALUES stage.
    /// `Some` exactly when `parent_fields` is and `column` is `None`; the two stages
    /// share the stash and can never both be open.
    values: Option<String>,
    /// Column-stage destination, baseline layers, and fold target. Views edits its
    /// parent list item; Schema edits a scratch dataset item and overlay object.
    pub column_ctx: Option<ColumnContext>,
    /// Dataset presentation by column for a Views draft. The writer has no Config
    /// parameter, so this captured layer is needed to avoid copying inherited dataset
    /// values into a view overlay as new overrides. Stage entry refreshes it from the
    /// pending-aware config. Other domains leave this map empty; Schema carries its
    /// editable overlay on `ColumnContext`.
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

/// What a cleared column-stage key fell to: the desk view's own key, the dataset level,
/// or — from the Schema door — whatever each view says. `None` means nothing below sets
/// the key, so there is nothing to tell the trader.
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
    /// The rows the edit stage paints, in order: every field, each ordered list's own
    /// items directly under it, then that list's available catalogue.
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

    /// Rows matching the query, in original row order. Fuzzy ranking determines
    /// membership only: the order itself is editable data, and section boundaries must
    /// stay intact. Each returned `Ranked::row` indexes `Draft::rows`.
    pub fn visible_rows(&self) -> Vec<crate::listfilter::Ranked> {
        // while any field is open, `query` is the value being typed into IT, not a
        // filter over the rows — so the rows below must not be narrowed by it. The
        // chain field is the one case where the rows really are a search: its own
        // `query` is the chain being typed and the rows are its completions. A plain
        // field's rows stay every edit row, unfiltered and in row order, so the trader
        // sees the row they are editing highlighted in place ("the rows below stay the
        // edit rows with the edited one highlighted"). An edit-stage filter that was
        // applied before `i` is lost the moment the field opens (the seed overwrites
        // `query`) — the same rule the chain field already has — so there is nothing
        // left to apply here even if this branch tried to.
        if let Some(entry) = self.text_entry {
            return match entry.completions {
                Completions::Chain => groupings::chain_candidates(self),
                Completions::None | Completions::Choice => {
                    let labels: Vec<String> =
                        self.rows().into_iter().map(|r| self.row_label(r)).collect();
                    crate::listfilter::rank(&labels, "")
                }
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

    /// The row the cursor is on, if the cursor is in range — indexed through
    /// [`Draft::visible_rows`], so every verb acts on the row the trader is actually
    /// looking at, filtered or not.
    pub fn selected_row(&self) -> Option<EditRow> {
        let rows = self.rows();
        self.visible_rows()
            .get(self.selected)
            .and_then(|m| rows.get(m.row).copied())
    }

    /// What the row under the cursor answers to — see [`RowVocabulary`] for why the
    /// footer asks the row rather than the domain.
    ///
    /// Groupings is the one domain whose footer must NOT take `i` from this answer:
    /// there `i` reaches past the selected row to the slot's whole chain, so it is live
    /// on every row including the display-only `slot`. That exception lives at the two
    /// call sites in `render` — the edit footer and `actions()`, the `i` button — not
    /// here, because it is a fact about the domain's `i` and not about any row.
    pub fn selected_vocabulary(&self, domain: Domain) -> RowVocabulary {
        self.vocabulary_of(self.selected_row(), domain)
    }

    /// Row vocabulary for any displayed row, shared by selection hints and buttons.
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
                FieldKind::Choice { .. } => RowVocabulary::StepsAndTypes,
                FieldKind::Bool(_) => RowVocabulary::Steps,
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

    /// Column stage opened by Enter on the given row: a Views member names its column,
    /// and a Schema `columns.<name>` field names that column. An already-open column
    /// projection has no nested column target.
    ///
    /// Shared by activation and [`Self::is_cursor_stop`] so a row that opens a stage
    /// remains reachable even without value-editing commands.
    pub fn column_stage_target(&self, domain: Domain, row: EditRow) -> Option<String> {
        if self.column().is_some() {
            return None;
        }
        match (domain, row) {
            (Domain::Views, row @ EditRow::Item { .. }) => Some(self.row_label(row)),
            (Domain::Schema, EditRow::Field(i)) => self
                .fields
                .get(i)?
                .key
                .strip_prefix("columns.")
                .map(str::to_string),
            _ => None,
        }
    }

    /// Values-stage target for a Scopes `dimensions` member or candidate. Available
    /// only outside column and values projections. Activation and cursor-stop checks
    /// use the same row-addressed resolver.
    pub fn values_stage_target(&self, domain: Domain, row: EditRow) -> Option<String> {
        if domain != Domain::Scopes || self.column().is_some() || self.values().is_some() {
            return None;
        }
        match row {
            row @ (EditRow::Item { field, .. } | EditRow::Available { field, .. })
                if self
                    .fields
                    .get(field)
                    .is_some_and(|f| f.key == "dimensions") =>
            {
                Some(self.row_label(row))
            }
            _ => None,
        }
    }

    /// Whether a row offers a value command or opens a nested stage. Schema column
    /// fields qualify through their stage target even though their vocabulary is
    /// `Inert`.
    ///
    /// Display-only fields, list headers, `MultiChoice`, and choices with fewer than
    /// two options remain visible with their diagnostics but are skipped when a stop
    /// exists. If the filtered list contains no stops, snapping leaves selection
    /// unchanged; motion can still traverse those inert rows.
    ///
    /// Object-wide commands do not qualify a row, including Groupings' `i` for the
    /// whole chain. Row clicks reject non-stops even in a list with no stops.
    pub fn is_cursor_stop(&self, domain: Domain, row: EditRow) -> bool {
        self.vocabulary_of(Some(row), domain) != RowVocabulary::Inert
            || self.column_stage_target(domain, row).is_some()
            || self.values_stage_target(domain, row).is_some()
    }

    /// Apply navigation to the full visible-row list, then snap to a cursor stop. Step
    /// counts measure visible rows, including inert rows, rather than stops.
    ///
    /// A Move of ±1 wraps while searching in its direction. Larger moves, Top, and
    /// Bottom clamp; if no stop is found toward that end, search back toward the other
    /// end. If there are no stops, retain the position chosen by navigation.
    pub fn move_selection(&mut self, domain: Domain, nav: NavCommand) {
        let len = self.visible_rows().len();
        self.selected = vimnav::apply(self.selected, len, nav);
        let (forward, wrap) = match nav {
            NavCommand::Move(delta) => (delta > 0, delta.abs() == 1),
            NavCommand::Top => (true, false),
            NavCommand::Bottom => (false, false),
        };
        self.snap_selection(domain, forward, wrap);
    }

    /// Settle a reset selection after stage entry, filtering, or row changes. Search
    /// from the current position toward the end, then backward for the first stop.
    /// Leave selection unchanged if none exists.
    pub fn settle_selection(&mut self, domain: Domain) {
        self.snap_selection(domain, true, false);
    }

    /// Search for a stop beginning at the current position, bounded to the visible
    /// list. Wrapping searches circle in the requested direction; non-wrapping searches
    /// reach that end and then search the remaining rows in reverse.
    ///
    /// No match leaves selection unchanged. In a list containing only inert rows,
    /// [`Self::move_selection`] can therefore still move using `vimnav::apply`.
    fn snap_selection(&mut self, domain: Domain, forward: bool, wrap: bool) {
        // Build the row and ranking models before probing. Re-ranking for every probe
        // would repeat fuzzy matching and sorting across the same filtered list.
        let rows = self.rows();
        let visible = self.visible_rows();
        let len = visible.len();
        if len == 0 {
            return;
        }
        let at = self.selected.min(len - 1);
        let is_stop = |position: usize| {
            visible
                .get(position)
                .and_then(|m| rows.get(m.row).copied())
                .is_some_and(|row| self.is_cursor_stop(domain, row))
        };
        // Generate candidate positions lazily instead of collecting another vector.
        let order: Box<dyn Iterator<Item = usize>> = if wrap {
            Box::new((0..len).map(move |step| {
                if forward {
                    (at + step) % len
                } else {
                    (at + len - step) % len
                }
            }))
        } else if forward {
            Box::new((at..len).chain((0..at).rev()))
        } else {
            Box::new((0..=at).rev().chain(at + 1..len))
        };
        if let Some(position) = order.into_iter().find(|&p| is_stop(p)) {
            self.selected = position;
        }
    }

    /// Move selection to this row's position in the filtered list. Leave selection
    /// unchanged if it is no longer visible. Reorders follow the item; adds and
    /// removals separately keep the cursor at the next visible row.
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

    /// The column whose presentation is open, if [`Stage::Column`] is. The one question
    /// `render::revalidate` asks before folding, and the reason the fold is a property
    /// of the draft rather than of the stage the gpui side happens to be painting.
    pub fn column(&self) -> Option<&str> {
        self.column.as_deref()
    }

    /// The column whose values are open, if [`Stage::Values`] is.
    pub fn values(&self) -> Option<&str> {
        self.values.as_deref()
    }

    /// Open a column projection by stashing parent fields and installing the seven
    /// presentation fields with a fresh baseline. Refuse re-entry and names that are
    /// neither a Views member nor a declared Schema column. Otherwise the fold would
    /// have no target and silently lose edits. Reset draft input and selection; the
    /// render transition separately resets dialog mode and confirmation.
    pub fn enter_column(&mut self, column: &str, fields: Vec<Field>) -> bool {
        // Re-entry would be the end of the object: the membership test below passes
        // THROUGH `field_by_key`'s parent fallback, so from an already-open stage it
        // would stash the seven installed column fields as `parent_fields` and drop the
        // view's own list forever — after which `list_items("columns")` answers `None`
        // and the write path renders a view with no columns at all. Unreached today
        // (this stage installs no `EditRow::Item` rows, and `commit_selected_row` gates
        // on `column().is_none()` besides), so this line is what makes it
        // unrepresentable rather than merely unreached.
        //
        // The Values stage shares this same stash (`Draft::values`'s own
        // doc), so it is refused here too — entering over an open Values
        // stage would stash ITS installed fields as `parent_fields` and
        // drop the object's own list exactly as re-entry would.
        if self.column.is_some() || self.values.is_some() {
            return false;
        }
        // Membership is what the door lists: the Views door's `columns` list, or — the
        // Schema door — a parent field keyed `columns.<col>`, which is how
        // `schema::fields` names a column row.
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
        // Clear choice completion together with text entry so neither retains indices
        // into the parent fields after the column projection is installed.
        self.choice = None;
        true
    }

    /// Fold installed column fields before validation or rendering a write. Views
    /// updates the parent list item against definition plus dataset values; Schema
    /// updates its scratch item against kind defaults. No context means no fold.
    ///
    /// Cleared label and width fields inherit the baseline. Reseed them immediately so
    /// the draft shows the value that persistence will read back. Return the cleared
    /// key and its inherited layer for the caller's notice.
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
            // below the dataset level is the desk view's own key, which varies per view
            // — so the honest answer is "each view", not one layer's name.
            ColumnDoor::Dataset => Some(FellTo::EachView),
            ColumnDoor::View => {
                let set = |p: &ColumnPresentation| match key {
                    "label" => p.label.is_some(),
                    "width" => p.width.is_some(),
                    _ => false,
                };
                // the dataset level sits ABOVE the desk view, so a cleared view key
                // meets it first.
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

    /// Close the column stage: fold one last time, restore the object's fields, and
    /// leave the cursor on the column's own row.
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
        // Clear choice completion with text entry before restoring parent selection.
        self.choice = None;
        self.selected = 0;
        if let Some(column) = column {
            self.select_item_named(&column);
        }
    }

    /// Open the Values stage: stash the object's fields, install `fields` (the one
    /// values list) as a clean baseline. Refused while any projection is already open,
    /// for `enter_column`'s reason — a second stash would drop the object's own fields
    /// for good.
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
    /// The field fallback is exactly [`Draft::enter_column`]'s own membership rule read
    /// backwards: the Schema door's column rows are `Field`s keyed `columns.<col>`, not
    /// list items, so without it `escape` out of a column stage on a thirty-column
    /// dataset would land the cursor back at the top of the list rather than on the
    /// column just edited. No other domain can reach it — a view's own list field is
    /// keyed `columns`, never `columns.<something>`.
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

    /// Replace the object's own fields with a freshly derived set and treat them as
    /// applied.
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
    /// Read through [`Draft::field_by_key`], which falls back to the object's stashed
    /// fields, and that fallback is load-bearing rather than tidy: the column stage
    /// swaps `fields` out for one column's seven, while the write path still renders
    /// the WHOLE object on every keystroke — `views::presentation_table` and
    /// `views::doc_table` both read this — and the item being folded into lives in that
    /// stashed list. Without the fallback, a keystroke in the column stage would render
    /// a view with no columns at all.
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
        self.text_entry
            .is_some_and(|entry| entry.completions == Completions::Chain)
    }

    /// Mirror input text into the draft. Filtering resets selection to the first match;
    /// plain text entry keeps its edited row highlighted because the rows remain
    /// unfiltered. Chain and choice entry update their completion selection.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        match self.text_entry {
            Some(TextEntry {
                row: field,
                completions: Completions::None,
            }) => self.follow(field),
            Some(TextEntry {
                row: field,
                completions: Completions::Choice,
            }) => {
                // The choice list re-ranks against the field's own typed
                // text — the CURSOR still follows the field's row exactly
                // as a plain field's does, since typing here narrows the
                // option list below, not the cursor's position over it.
                let q = self.query.clone();
                if let Some(list) = self.choice.as_mut() {
                    list.set_query(&q);
                }
                self.follow(field);
            }
            _ => self.selected = 0,
        }
    }

    /// Open a text or numeric field seeded from its value. The caller checks domain
    /// permission and owns the input mode/focus transition. `Changed` here means the
    /// field opened, not that a value is ready for validation or persistence.
    ///
    /// Follow the row after installing `text_entry`: the displayed list becomes
    /// unfiltered, so its old filtered index can identify a different row.
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
            completions: Completions::None,
        });
        self.follow(row);
        Step::Changed
    }

    /// `i` on a `Choice` row: open the shared `Input` as a typeahead over the field's
    /// options — EMPTY, since the current value is already the highlighted row and a
    /// seed would have to be deleted before typing — with the highlight placed on the
    /// current option. `Step::Inert` off any other row, and on a one-option `Choice`
    /// (`step_selected`'s own guard, mirrored: there is nothing to choose between).
    pub fn begin_choice_entry(&mut self) -> Step {
        let Some(row @ EditRow::Field(index)) = self.selected_row() else {
            return Step::Inert;
        };
        let FieldKind::Choice { options, selected } = &self.fields[index].kind else {
            return Step::Inert;
        };
        if options.len() < 2 {
            return Step::Inert;
        }
        let mut list = crate::choice::ChoiceList::new(options.clone(), crate::choice::DEFAULT_CAP);
        list.place(options.get(*selected).map(String::as_str));
        self.choice = Some(list);
        self.query.clear();
        self.text_entry = Some(TextEntry {
            row,
            completions: Completions::Choice,
        });
        self.follow(row);
        Step::Changed
    }

    /// Whether the open field is a `Choice` row's typeahead.
    pub fn choice_entry(&self) -> bool {
        self.text_entry
            .is_some_and(|entry| entry.completions == Completions::Choice)
    }

    /// The nav keys in a choice field move the HIGHLIGHT, never the
    /// cursor (which stays on the field's row).
    pub fn choice_nav(&mut self, cmd: crate::vimnav::NavCommand) {
        if let Some(list) = self.choice.as_mut() {
            list.nav(cmd);
        }
    }

    /// The lit option's index into the ranked list while a choice field
    /// is open — what the dialog's scroll handle is pointed at after
    /// every key that can move it, so the row stays in the viewport.
    pub fn choice_ranked_highlighted(&self) -> Option<usize> {
        self.choice.as_ref().map(|l| l.ranked_highlighted())
    }

    /// Clicking a choice row selects it and performs the same completion as Tab.
    pub fn choice_click(&mut self, row: usize) -> bool {
        let Some(list) = self.choice.as_mut() else {
            return false;
        };
        if !list.set_ranked_highlighted(row) {
            return false;
        }
        self.complete_choice()
    }

    /// `tab`: the highlighted option's text becomes the query.
    pub fn complete_choice(&mut self) -> bool {
        let Some(list) = self.choice.as_mut() else {
            return false;
        };
        if !list.complete() {
            return false;
        }
        self.query = list.query().to_string();
        true
    }

    /// `enter`: the HIGHLIGHTED option becomes the field's value — never
    /// the typed text (a dropdown commits what is lit) — and the field
    /// closes. Refused with the field open when nothing is highlighted
    /// (the query matched no option). `Step::Inert` when the lit option
    /// is the one already selected: nothing to write, and the field
    /// still closes — closing is the visible answer.
    pub fn apply_choice(&mut self) -> Step {
        let Some(TextEntry {
            row: row @ EditRow::Field(index),
            completions: Completions::Choice,
        }) = self.text_entry
        else {
            return Step::Inert;
        };
        let Some(picked) = self.choice.as_ref().and_then(|l| l.pick()) else {
            return Step::Refused("no option matches — keep typing, or escape".to_string());
        };
        let outcome = match &mut self.fields[index].kind {
            FieldKind::Choice { selected, .. } if *selected == picked => Step::Inert,
            FieldKind::Choice { selected, .. } => {
                *selected = picked;
                Step::Changed
            }
            _ => Step::Inert,
        };
        self.text_entry = None;
        self.choice = None;
        self.query.clear();
        self.follow(row);
        outcome
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
        self.choice = None;
        self.query.clear();
        match entry {
            Some(TextEntry {
                completions: Completions::None | Completions::Choice,
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
            completions: Completions::None,
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

    /// Step the selected value, or return `Inert` when there is no change. Refuse the
    /// final untick of a definition list that cannot represent emptiness; removing its
    /// user key would inherit the lower object instead. Values-stage lists may become
    /// empty to drop that dimension's constraint. Presentation lists may hide every
    /// column.
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
                        // A hand-edited seed can be outside the editor's bounds. Refuse
                        // stepping it instead of clamping away the original value;
                        // typed entry can replace it with a deliberate valid value.
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
                // The cursor does NOT follow the item into the object's own list: a
                // trader adding several columns wants it on the next available row,
                // where their eye already is. The added item moved *earlier* in row
                // order and its label is unchanged, so the rows ahead of the next
                // visible one are the same set, merely reordered — its visible index is
                // the old cursor plus one. When the added item was the catalogue's last
                // row there is no next, and the same index now holds the row that
                // preceded it (the previous available column, or the object's own last
                // item when there is none left), which is where the cursor stays rather
                // than running off the end.
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
                // The Values stage may empty its list — that is "drop this dimension",
                // folded by the adapter; the guard is a Groupings/Views rule.
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

    /// The payload a list row drags; `None` for a field row, which is neither a drag
    /// source nor a drop target.
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

    /// Resolve both payloads by name, then apply the drop within their shared field.
    ///
    /// - Item to item: remove the source and insert at the target's old index.
    /// - Available to item: promote the candidate at that index, included.
    /// - Item to available: demote it; catalogue order is not persisted.
    /// - Available to available, same row, different fields, or missing names: inert.
    ///
    /// After a change, follow the moved item. A catalogue-less list cannot resolve an
    /// Available target, so it cannot reach a demotion.
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

    /// `shift+j` / `shift+k`: move the item under the cursor past the next VISIBLE item
    /// in that direction within the object's own list — under a filter that is what
    /// reordering means, and the count of hidden rows jumped over is returned so the
    /// notice can say so. `None` at either end of that list, and on a row that is not
    /// one of its items.
    ///
    /// An [`EditRow::Available`] row is one of those: the catalogue is unordered by
    /// construction, so there is no order there to change and a "move" would be painted
    /// and never written. It is declined here rather than represented — which is also
    /// why the object's own last item has nowhere further down to go, even with a
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

    /// Remove the selected member into its available catalogue. Refuse an available row
    /// because it is not a member, and refuse a list without a catalogue because
    /// ticking controls membership there. An empty existing catalogue still permits
    /// removal. Return a notice-bearing refusal rather than silently doing nothing.
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
        // The cursor does NOT follow the item to the end of the catalogue, for the same
        // reason `space`'s add leaves it behind: a trader removing several columns
        // wants it on the row that was next. The removed item moved *later* in row
        // order with its label unchanged, so the visible rows ahead of the next one
        // lost exactly one — the next row now sits at the old index and `selected` is
        // already right. The one exception is a removal with nothing visible after it:
        // the item lands at the end, which is where it already was, so the old index
        // would still be on it — step back to the previous row instead, the way `dd` on
        // a buffer's last line does.
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

    /// Group changed fields by persistence destination. Clean fields contribute
    /// nothing. A changed ordered-list member set additionally writes the definition;
    /// reordering, hiding, and resizing alone remain presentation changes.
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
            choice: None,
            parent_fields: None,
            column: None,
            values: None,
            column_ctx: None,
            // An object nothing defines yet has no columns for a dataset
            // to speak for; `Domain::draft` is where the layer arrives.
            dataset_layer: BTreeMap::new(),
        }
    }

    /// Map a diagnostic path to an installed row; unmatched paths remain header-only.
    /// `<doc>.<object>.<field>[.<index>...]` names a field or one of its members. List
    /// indices resolve through source names before locating the current item.
    ///
    /// A column stage accepts only paths for its open column and installed format,
    /// label, or width fields. Diagnostics for other columns or object-level fields
    /// remain in the header while those rows are hidden.
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

    /// Resolve a diagnostic's source-array index to the current member by name.
    /// Definition order and presentation order can differ, so using the raw index would
    /// flag another column after a reorder. Accept table entries with `name` or string
    /// entries. Return `None` when the source or current item cannot be resolved; the
    /// caller can fall back to its raw-index rule.
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
    /// Selected-field help by domain, stage, and key. List members use their list's
    /// key; column stages share the Views presentation-help table. Describe value
    /// meaning and grammar here; key hints are rendered separately. Unknown keys return
    /// an empty sentence while retaining the footer slot.
    pub fn help(self, stage: &Stage, key: &str) -> &'static str {
        if matches!(stage, Stage::Column { .. }) {
            return views::column_help(key);
        }
        // the Values stage's one row is always keyed `values`, so this answers the same
        // as the general `Domain::Scopes` arm below would — stated explicitly, ahead of
        // it, so a future domain that grows a Values-shaped stage of its own cannot
        // silently fall through to its OWN `help` table instead.
        if matches!(stage, Stage::Values { .. }) {
            return scopes::help("values");
        }
        match self {
            Domain::Views => views::help(key),
            Domain::Sources => sources::help(key),
            Domain::Groupings => groupings::help(key),
            Domain::Scopes => scopes::help(key),
            Domain::Colors => colours::help(key),
            Domain::Schema => schema::help(key),
        }
    }

    /// Whether a text field permits typed editing. Sources permits its supported
    /// free-text settings; Scopes permits text and expression; Views and Schema permit
    /// column label and width. Groupings' slot stays read-only, and Colors has no text
    /// field. Numeric and choice entry use their own field paths.
    pub fn text_editable(self, key: &str) -> bool {
        match self {
            Domain::Groupings | Domain::Colors => {
                let _ = key;
                false
            }
            Domain::Scopes => matches!(key, "text" | "expression"),
            Domain::Views | Domain::Schema => views::text_editable(key),
            Domain::Sources => sources::text_editable(key),
        }
    }

    /// Normalize committed text through the domain's parser, or return its refusal.
    /// This validates duration, regex, width, and expression grammars before
    /// committing.
    pub fn parse_text(self, key: &str, text: &str) -> Result<String, String> {
        match self {
            // Colors joins for the same reason `text_editable` gives
            // it no `true` above: no `Text` row for this door to ever
            // be called on.
            Domain::Groupings | Domain::Colors => {
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
            Domain::Colors => colours::fields(config, object),
        }
    }

    /// The fields a `c`-copied object opens with, built straight from the table
    /// `create_from_name` just copied rather than from a named object `config` has a
    /// row for yet — the copy has not been written when this runs. Only
    /// [`Domain::duplicable`] needs the real answer: every other domain falls back to
    /// `self.fields(config, None)`, its own empty-object shape, since nothing else can
    /// reach this door.
    pub fn fields_from_source(self, config: &Config, table: &toml::Table) -> Vec<Field> {
        match self {
            Domain::Scopes => scopes::fields_from_table(config, Some(table)),
            Domain::Views
            | Domain::Groupings
            | Domain::Schema
            | Domain::Sources
            | Domain::Colors => self.fields(config, None),
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
            choice: None,
            parent_fields: None,
            column: None,
            values: None,
            column_ctx: None,
            // Views alone. Reloading the views a second time here (`fields` above
            // already did once) is the price of the scaffold's
            // one-`Domain`-match-per-function rule — the alternative is a `fields` that
            // returns two things, which every other adapter would then have to answer
            // for.
            dataset_layer: match self {
                Domain::Views => views::dataset_layer_for(config, object),
                _ => BTreeMap::new(),
            },
        };
        draft.diagnostics = self.validate(&draft, config);
        draft
    }

    /// Create a validated draft from the adapter's default fields and empty source.
    /// Copying a scope or saving current frame scope replaces these initial fields
    /// through its separate naming seed before committing creation.
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
            Domain::Colors => colours::to_table(draft, dest),
        }
    }

    /// Everything wrong with the draft as it stands, run on every field change,
    /// synchronously, with no debounce — it is a parse of a few hundred bytes.
    pub fn validate(self, draft: &Draft, config: &Config) -> Vec<Diagnostic> {
        match self {
            Domain::Views => views::validate(draft, config),
            Domain::Groupings => groupings::validate(draft, config),
            Domain::Scopes => scopes::validate(draft, config),
            Domain::Schema => schema::validate(draft, config),
            Domain::Sources => sources::validate(draft, config),
            Domain::Colors => colours::validate(draft, config),
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
    /// Selection index in the filtered browse list, bounded by its visible length.
    pub selected: usize,
    /// The filter query, mirrored here from `ShellView::dialog_input` by
    /// that field's `InputEvent::Change` subscription. The `Input` owns
    /// the text; this is the pure copy the rows are ranked against. It
    /// survives leaving filter mode by `enter`, which applies the search
    /// and leaves you on the match; `escape` puts
    /// [`Self::filter_entry_query`] back instead.
    pub query: String,
    /// What the open stage's query ([`Self::effective_query`]) stood at
    /// when filter mode was last entered — written only by
    /// [`Self::enter_filter`] and read only by [`Self::exit_filter`], so
    /// the `escape` that backs out of a search cannot revert to some
    /// earlier visit's text.
    ///
    /// One field serves both query slots because a filter session can
    /// never span a stage change: every stage transition sets
    /// `DialogMode::Normal` explicitly ([`Self::enter_edit`],
    /// `render::enter_column_stage`, `render::enter_values_stage`), so
    /// the snapshot is always applied to the slot it was taken from. A
    /// future transition that preserved `Filter` would revert one
    /// stage's query to another stage's text.
    pub filter_entry_query: String,
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
    /// Set when a click opened a stage — the browse list's row click
    /// (`render::on_row_clicked`, the edit stage) or an edit-stage door
    /// row's (`render::on_edit_row_clicked`, the column stage) — and
    /// cleared by the next single click. A double-click's second half
    /// arrives at the same point one frame later, where the just-opened
    /// stage has painted a DIFFERENT row — without this flag it would
    /// open `i` on whatever field now sits under the pointer, a field
    /// the trader never aimed at (on Groupings, slot 3's chooser puts a
    /// row there whose click opens the chain field). With it, a
    /// double-click on a door row is "open the stage" and nothing more.
    pub click_opened_stage: bool,
    /// The object being edited. `None` in [`Stage::Browse`], and the only
    /// state this dialog stores rather than derives — deliberately, since
    /// it is also what the edit stage paints; see [`Draft`].
    pub draft: Option<Draft>,
    /// The dialog's pending destructive question, available in browse and edit. It
    /// replaces the action bar and blocks unrelated input until answered. Record its
    /// object separately in `confirm_target` so a reload cannot silently redirect the
    /// answer by reordering the browse list. Stage transitions disarm both.
    pub confirm: Option<Confirm>,
    /// The object [`Self::confirm`] was asked about, as `render:: target_object`
    /// resolved it at arming time. Meaningful only while `confirm` is `Some`, and
    /// written only beside it (`render:: arm_confirm`): the answer is carried out only
    /// if the target still resolves to this name, since a reload can re-rank the browse
    /// list under an index cursor.
    pub confirm_target: Option<String>,
    /// Source dataset captured from the selected browse row when naming starts. Clear
    /// it when naming ends; it seeds the new source's dataset choice.
    pub naming_dataset: Option<String>,
    /// Source of the object being named: defaults, a saved scope copy, or current frame
    /// scope. Stored separately from the name input.
    pub naming_seed: NameSeed,
    /// The tag of the latest distinct request the Values stage submitted
    /// (`render::enter_values_stage`); an outcome with any other tag is stale and
    /// dropped.
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
            filter_entry_query: String::new(),
            mode: DialogMode::Normal,
            notice: None,
            click_opened_stage: false,
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

    /// Pure stage entry, called through `render::enter_edit_stage` so scroll reset,
    /// repaint, and subsequent input synchronization stay together.
    ///
    /// Build the draft, clear both stage queries, and enter Normal mode. Browse and
    /// draft selection index different lists; carrying the old filter or mode across
    /// would misroute keys and Escape. The shared dialog input is synchronized by
    /// `dialog::sync_dialog_text` after the transition.
    pub(in crate::shell::objectdialog) fn enter_edit(&mut self, config: &Config, object: &str) {
        let mut draft = self.domain.draft(config, object);
        draft.query.clear();
        // Skip display-only rows on entry, such as Groupings' slot number and list
        // header, when the draft contains a cursor stop.
        draft.settle_selection(self.domain);
        // Every domain opens in normal mode with no field open — a Groupings slot lands
        // in the chooser and `i` opens its chain field. There is deliberately no domain
        // arm here: the one door every entry goes through has one answer.
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

    /// Enter editing with an already-built creation or copy draft. Its object may not
    /// yet be in active configuration, so deriving a fresh draft would discard its
    /// initial fields. Use the render stage-entry wrapper for scroll and repaint.
    pub(in crate::shell::objectdialog) fn enter_edit_with(&mut self, mut draft: Draft) {
        // Clear the inherited filter and apply the same cursor-stop rule as fresh stage
        // entry.
        draft.query.clear();
        draft.settle_selection(self.domain);
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

    /// Return to browsing and drop the draft. The caller restores selection by object
    /// name. Clear the browse query; retain mode for the shared input sync to settle
    /// focus consistently with the transition.
    pub fn leave_edit(&mut self) {
        self.draft = None;
        self.stage = Stage::Browse;
        self.query.clear();
        self.notice = None;
        self.disarm();
    }

    /// Mirror changed input text into the active stage's query, reset selection, and
    /// clear the row-specific notice. Browse/naming use `state.query`; edit, column,
    /// and values stages use the draft. Never write both query stores, which would
    /// leave an invisible stale filter when returning to browse.
    pub fn set_query(&mut self, query: String) {
        // Edit, Column, and Values share the draft's query and selection. Keep the
        // stage sets here, in `effective_query`/`effective_selected`, and in their
        // mutable counterparts equal;
        // otherwise the input can paint one filter while bulk changes use another.
        if matches!(
            self.stage,
            Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }
        ) && let Some(draft) = self.draft.as_mut()
        {
            draft.set_query(query);
            // Query changes reset and re-rank list selection, which may land on an
            // inert row. Preserve the edited row while a field is open: Groupings'
            // chain field, for example, belongs to an otherwise inert list header.
            if draft.text_entry.is_none() {
                draft.settle_selection(self.domain);
            }
        } else {
            self.query = query;
            self.selected = 0;
        }
        self.notice = None;
    }

    /// The query the open stage is filtering by — the draft's in
    /// `Stage::Edit`/`Column`/`Values`, the state's own otherwise. The **read half** of
    /// [`Self::set_query`]'s one-way mirror: `dialog::sync_dialog_text` writes the
    /// shared `Input` from this, so a query left sitting in the other stage's slot can
    /// never reach the screen. See [`Self::set_query`]'s doc for why this match must
    /// name exactly the same stages that one does.
    pub fn effective_query(&self) -> &str {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }, Some(draft)) => {
                draft.query.as_str()
            }
            _ => self.query.as_str(),
        }
    }

    /// The open stage's query slot itself, for the two transitions that
    /// rewrite it. Same slot rule as [`Self::effective_query`], written
    /// once here rather than at each call site, because a transition that
    /// picked the wrong slot would revert a query nobody was filtering
    /// by and leave the visible one standing.
    fn effective_query_mut(&mut self) -> &mut String {
        match (&self.stage, self.draft.as_mut()) {
            (Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }, Some(draft)) => {
                &mut draft.query
            }
            _ => &mut self.query,
        }
    }

    /// `/`: enter filter mode over the open stage's query, remembering
    /// what it stood at so `escape` can put it back. The one door for
    /// each stage's `/` and the filter row's click alike — a call site
    /// that assigned [`DialogMode::Filter`] itself would leave the
    /// snapshot from some earlier visit in place.
    ///
    /// The value field's own use of `DialogMode::Filter`
    /// (`render::open_text_field`) deliberately does NOT come through
    /// here: a field is text being typed, not a search, and its keys are
    /// claimed by `render`'s `text_entry` branch before the filter
    /// branch can read this snapshot at all.
    pub fn enter_filter(&mut self) {
        // Mode and snapshot are moved out and back so the query slot can
        // be read beside them, the same trick `exit_filter` uses —
        // rather than cloning the query on every `/`.
        let mut mode = self.mode;
        let mut entry = std::mem::take(&mut self.filter_entry_query);
        dialogmode::enter_filter(&mut mode, &mut entry, self.effective_query());
        self.mode = mode;
        self.filter_entry_query = entry;
    }

    /// Leave filter mode: `enter` keeps the query as typed, `escape`
    /// puts back the one [`Self::enter_filter`] recorded. Returns whether the query
    /// changed — with the open
    /// stage's cursor already moved to the top of the re-expanded list,
    /// leaving the caller only the viewport to scroll. An `escape` with
    /// nothing typed changes no text and so moves no cursor.
    pub fn exit_filter(&mut self, exit: dialogmode::FilterExit) -> bool {
        // The snapshot is moved out and back so the query slot can be
        // borrowed mutably beside it; both live on `self`.
        let entry = std::mem::take(&mut self.filter_entry_query);
        let mut mode = self.mode;
        let changed = dialogmode::exit_filter(&mut mode, &entry, self.effective_query_mut(), exit);
        self.mode = mode;
        self.filter_entry_query = entry;
        if changed {
            *self.effective_selected_mut() = 0;
        }
        changed
    }

    /// The cursor of the open stage, in the same slot rule as
    /// [`Self::effective_query`]: the draft's in the edit, column and
    /// values stages, the state's own otherwise. What the change
    /// subscription scrolls to after a keystroke — the top for a filter
    /// (the reset), the edited row for an open plain field, which
    /// `set_query` keeps. See [`Self::set_query`]'s doc for why this
    /// match must name exactly the same stages that one does.
    pub fn effective_selected(&self) -> usize {
        match (&self.stage, self.draft.as_ref()) {
            (Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }, Some(draft)) => {
                draft.selected
            }
            _ => self.selected,
        }
    }

    /// The open stage's cursor slot itself, for [`Self::exit_filter`]'s
    /// move to the top of a list that changed under it. The `_mut`
    /// sibling of [`Self::effective_selected`], written beside it rather
    /// than inlined at the call site so the stage list stays in one
    /// greppable place — see [`Self::set_query`] for what drifting apart
    /// costs.
    fn effective_selected_mut(&mut self) -> &mut usize {
        match (&self.stage, self.draft.as_mut()) {
            (Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }, Some(draft)) => {
                &mut draft.selected
            }
            _ => &mut self.selected,
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

    /// Whether Escape can return to a parent stage before closing the dialog.
    pub fn has_previous_stage(&self) -> bool {
        matches!(
            self.stage,
            Stage::Edit { .. } | Stage::Naming | Stage::Column { .. } | Stage::Values { .. }
        )
    }
}

/// Searchable browse text: the painted label, including a source's dataset prefix, plus
/// the visible summary. Matching unseen text would make highlights misleading.
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

    /// a cleared view-level key meets the DATASET level before the desk view's own,
    /// because that is the order they resolve in — so the notice names the layer the
    /// trader will actually see from here, not the one furthest down.
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

    /// A real dataset and two-column view prove parent-field access during a column
    /// projection and rejection of diagnostic paths belonging to the other column.
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

        // No recorded fork baseline: no drift can be established.
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

    /// An override recorded before `colours` became `colors` is keyed
    /// `colours.<name>`: it still measures drift for the colors object, a
    /// revert removes it under its own spelling, and beside a current-spelled
    /// entry it is stale so the next overrides write prunes it.
    #[test]
    fn an_override_recorded_under_the_old_colours_key_still_counts() {
        let desk_v1 = "[delta]\nhue = 240\n";
        let user = "[delta]\nhue = 10\n";
        let shadow_text = object_text(
            "delta",
            toml_value_to_item(&desk_v1.parse::<toml::Table>().unwrap()["delta"]),
        );
        let entry = |key: &str| {
            format!(
                "[\"{key}\"]\nshadowed_layer = \"desk\"\nshadowed_text = '''\n{shadow_text}'''\n"
            )
        };
        let legacy = entry("colours.delta");
        let desk_v2 = "[delta]\nhue = 250\n";
        let config = config_from(&[
            (Layer::Desk, "colors", desk_v2),
            (Layer::User, "colors", user),
            (Layer::User, "overrides", legacy.as_str()),
        ]);
        let rows = Domain::Colors.objects(&config);
        assert!(rows[0].overridden);
        assert!(rows[0].drifted, "the old-keyed baseline measures drift");
        assert_eq!(
            override_keys_of(&config, "colors", "delta"),
            vec!["colours.delta".to_string()]
        );
        assert!(stale_override_keys(&config).is_empty());

        let both = format!("{legacy}{}", entry("colors.delta"));
        let config = config_from(&[
            (Layer::Desk, "colors", desk_v1),
            (Layer::User, "colors", user),
            (Layer::User, "overrides", both.as_str()),
        ]);
        assert_eq!(
            stale_override_keys(&config),
            vec!["colours.delta".to_string()]
        );
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

    /// A presentation-only user override can be reverted even when the definition still
    /// comes from the desk layer.
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

    /// An orphaned user presentation reserves the name even after all definitions of
    /// that object disappear, preventing new objects from inheriting stale settings.
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

    /// Without a recorded override baseline, differing user and inherited values cannot
    /// establish drift.
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
        // No columns are the view's OWN yet — `npv` is only in the available catalogue,
        // for `space` to add.
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

    /// Every domain opens its edit stage in normal mode with no field open — Groupings
    /// included: a slot opens in the chooser, and `i` is the way into the chain field.
    /// The Views half pins that the rule has no domain arm at all.
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

    /// `effective_query` is the read half of `set_query`'s one-way mirror: each stage's
    /// own keystrokes land in — and are read back from — that stage's own slot, never
    /// the other one.
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
            choice: None,
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

    /// Wrapping numeric fields cross either endpoint and remain within their bounds.
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

    /// Out-of-range loaded values are refused by stepping instead of silently clamped.
    /// The user must type a valid replacement deliberately.
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

    /// `i` on a `Choice` row opens the field EMPTY (the current value is already lit),
    /// with the highlight placed on the current option; `enter` picks the lit row and
    /// closes; the cursor stays on the row throughout.
    #[test]
    fn i_on_a_choice_row_opens_an_empty_field_placed_on_the_current_option() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["none".into(), "danger".into(), "accent".into()],
            selected: 2,
        });
        assert_eq!(draft.begin_choice_entry(), Step::Changed);
        assert!(draft.choice_entry());
        assert_eq!(draft.query, "", "opens empty");
        assert_eq!(
            draft.choice.as_ref().unwrap().highlighted_text(),
            Some("accent")
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(0)),
            "the cursor stays on the row"
        );
        draft.set_query("dan".into());
        assert_eq!(
            draft.choice.as_ref().unwrap().highlighted_text(),
            Some("danger")
        );
        assert_eq!(
            draft.selected_row(),
            Some(EditRow::Field(0)),
            "typing does not move the cursor"
        );
        assert_eq!(draft.apply_choice(), Step::Changed);
        assert!(!draft.choice_entry());
        assert!(draft.choice.is_none());
        assert_eq!(draft.query, "");
        assert!(matches!(
            &draft.fields[0].kind,
            FieldKind::Choice { selected: 1, .. }
        ));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)));
    }

    #[test]
    fn picking_the_option_already_selected_is_inert_and_still_closes() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["normal".into(), "light".into()],
            selected: 1,
        });
        draft.begin_choice_entry();
        assert_eq!(draft.apply_choice(), Step::Inert);
        assert!(!draft.choice_entry(), "closing is the visible answer");
    }

    #[test]
    fn a_query_matching_nothing_is_refused_with_the_field_open() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["normal".into(), "light".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.set_query("zzz".into());
        assert!(
            matches!(draft.apply_choice(), Step::Refused(r) if r.contains("no option matches"))
        );
        assert!(draft.choice_entry(), "the field stays open for a retype");
    }

    #[test]
    fn tab_completes_the_lit_option_and_nav_moves_the_highlight() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["estimated".into(), "declared".into(), "paid".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.choice_nav(crate::vimnav::NavCommand::Move(1));
        assert_eq!(
            draft.choice.as_ref().unwrap().highlighted_text(),
            Some("declared")
        );
        assert!(draft.complete_choice());
        assert_eq!(draft.query, "declared");
        assert!(draft.choice_click(0));
        assert_eq!(
            draft.query, "declared",
            "a click on the only ranked row completes it"
        );
    }

    #[test]
    fn escape_cancels_a_choice_field_with_the_value_untouched() {
        let mut draft = single_field_draft(FieldKind::Choice {
            options: vec!["a".into(), "b".into()],
            selected: 0,
        });
        draft.begin_choice_entry();
        draft.set_query("b".into());
        draft.cancel_text_entry();
        assert!(!draft.choice_entry());
        assert!(draft.choice.is_none());
        assert!(matches!(
            &draft.fields[0].kind,
            FieldKind::Choice { selected: 0, .. }
        ));
        assert_eq!(draft.selected_row(), Some(EditRow::Field(0)));
    }

    #[test]
    fn a_one_option_choice_does_not_open_and_neither_does_a_text_row() {
        let mut one = single_field_draft(FieldKind::Choice {
            options: vec!["only".into()],
            selected: 0,
        });
        assert_eq!(one.begin_choice_entry(), Step::Inert);
        let mut text = single_field_draft(FieldKind::Text("x".into()));
        assert_eq!(text.begin_choice_entry(), Step::Inert);
    }

    /// The footer's `i` chip: a two-option `Choice` is `StepsAndTypes` now, a
    /// one-option one still `Inert`.
    #[test]
    fn a_choice_row_steps_and_types() {
        let draft = single_field_draft(FieldKind::Choice {
            options: vec!["a".into(), "b".into()],
            selected: 0,
        });
        assert_eq!(
            draft.selected_vocabulary(Domain::Colors),
            RowVocabulary::StepsAndTypes
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

    /// An absent object name builds fields for a new object without panicking.
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

    /// Validation runs against the DRAFT alone. Validating the merged doc instead would
    /// report every other broken view in the config against the one object the user is
    /// editing.
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

    /// Entering Edit clears the separate browse and draft query stores and gives Escape
    /// a parent stage to return to.
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

    /// Destructive prompts name their object and consequence.
    #[test]
    fn a_confirm_names_the_object_and_the_consequence() {
        assert!(Confirm::Delete.prompt("tree").contains("tree"));
        assert!(Confirm::Revert.prompt("tree").contains("tree"));
    }

    /// Overwrite confirmation describes replacement of a user-owned scope's values.
    /// Inherited scopes instead take the announced-fork path.
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

    // ---- Membership and available candidates ----

    /// A Groupings list has no available catalogue at ALL — ticking IS membership there
    /// — so `remove_selected` refuses and names the verb that does work here (`space`)
    /// rather than silently unticking. Decided by the catalogue's absence, never by a
    /// `dest` and never by whether some catalogue happens to be empty right now.
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

    /// An empty available catalogue still permits demotion. Catalogue existence, not
    /// its current length, determines whether the list supports removal.
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

    // ---- Dropping list rows by name ----

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

    /// A Groupings slot with `book`, `lhu` already chosen — no catalogue at all, the
    /// same shape `a_groupings_list_has_no_available_block_and_x_refuses` builds.
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

    /// "drop on a row" means take that row's index. Downward lands after the target's
    /// old position, upward before it.
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

    // ---- Edit-stage filtering ----

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
        // Row order, not fuzzy score, decides the filtered index now: after the move,
        // `rows()` reads [.., charlie (row 3), alpha (row 4)], and since "al" still
        // scores alpha far higher than charlie, a score-ordered `visible_rows` would
        // have put alpha BACK at index 0 — the identity check below would pass either
        // way, which is exactly why this index is asserted explicitly too.
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

    /// Filtering retains data order even when a later row has a better fuzzy score.
    /// Otherwise reordering would not control painted order and headers could split the
    /// wrong blocks.
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

    /// Typing in a plain field keeps its row highlighted. Its list is unfiltered, so a
    /// query change must not reset selection as list filtering would.
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
                completions: Completions::None
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

    /// Diagnostic indices name source-definition columns, not their reordered
    /// presentation positions. Resolve through the source name to flag the same column
    /// after a presentation reorder.
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

    /// Multiple diagnostics on one row use the highest severity for its glyph.
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

    /// Cursor stops follow row commands and nested-stage targets.
    #[test]
    fn cursor_stops_are_the_rows_that_answer_to_something() {
        // Views members and available candidates are stops; this fixture's single
        // Dataset choice and Columns header are inert.
        let list = two_column_draft_with_one_available();
        let stops: Vec<bool> = list
            .rows()
            .into_iter()
            .map(|row| list.is_cursor_stop(Domain::Views, row))
            .collect();
        assert_eq!(
            stops,
            vec![false, false, true, true, true],
            "dataset and the list header are not stops; the two members and the \
             available candidate are"
        );

        // Groupings skips the read-only Slot field and Dimensions header, leaving
        // member and candidate dimension rows as cursor stops.
        let slot = groupings_draft();
        let stops: Vec<bool> = slot
            .rows()
            .into_iter()
            .map(|row| slot.is_cursor_stop(Domain::Groupings, row))
            .collect();
        assert_eq!(
            stops.first(),
            Some(&false),
            "the slot number answers to nothing"
        );
        assert_eq!(stops.get(1), Some(&false), "nor does the Dimensions header");
        assert!(
            stops[2..].iter().all(|stop| *stop),
            "every dimension row does"
        );
    }

    /// Schema columns remain stops through their nested-stage targets despite inert
    /// value vocabulary. Derived dimensions have no such target.
    #[test]
    fn a_row_that_opens_a_stage_is_a_stop_even_when_every_value_verb_refuses_it() {
        let field = |key: &str| Field {
            key: key.to_string(),
            label: key.to_string(),
            kind: FieldKind::Text("f64 · measure".to_string()),
            dest: Destination::Doc,
            layer: None,
        };
        let draft = Draft::new_object(
            "risk_snapshot",
            vec![field("columns.npv"), field("derived.desk")],
            toml::Table::new(),
        );
        assert_eq!(
            draft.vocabulary_of(Some(EditRow::Field(0)), Domain::Schema),
            RowVocabulary::Inert,
            "sanity: a Schema column row has no value verb at all"
        );
        assert!(
            draft.is_cursor_stop(Domain::Schema, EditRow::Field(0)),
            "and is still a stop, because enter opens its column stage"
        );
        assert!(
            !draft.is_cursor_stop(Domain::Schema, EditRow::Field(1)),
            "a derived dimension opens nothing and answers to nothing"
        );
    }

    /// Single-step motion wraps between stops, skipping inert header rows at either end
    /// of the visible list.
    #[test]
    fn motion_skips_the_rows_that_answer_to_nothing() {
        let mut list = two_column_draft_with_one_available();
        list.settle_selection(Domain::Views);
        assert_eq!(list.selected, 2, "the stage settles onto the first member");

        list.move_selection(Domain::Views, NavCommand::Move(-1));
        assert_eq!(
            list.selected, 4,
            "k off the top wraps past the two field rows to the last stop"
        );
        list.move_selection(Domain::Views, NavCommand::Move(1));
        assert_eq!(
            list.selected, 2,
            "and j off the bottom wraps back to the first stop, not to row 0"
        );
        list.move_selection(Domain::Views, NavCommand::Top);
        assert_eq!(
            list.selected, 2,
            "`g` goes to the top of what the cursor can reach"
        );
        list.move_selection(Domain::Views, NavCommand::Bottom);
        assert_eq!(list.selected, 4, "and `shift+g` to the bottom of it");
        // Larger steps clamp and snap back from an inert edge without wrapping.
        list.selected = 2;
        list.move_selection(Domain::Views, NavCommand::Move(10));
        assert_eq!(list.selected, 4, "a clamped step lands on the last stop");
        list.move_selection(Domain::Views, NavCommand::Move(-10));
        assert_eq!(list.selected, 2, "and back on the first");
    }

    /// A filter can leave only inert rows. Settling then preserves selection; motion
    /// still applies vimnav first. This one-row fixture makes both results index zero.
    #[test]
    fn a_list_with_no_stop_at_all_moves_the_cursor_nowhere() {
        let mut list = two_column_draft_with_one_available();
        list.selected = 1;
        // The filter leaves only the inert Columns header.
        list.set_query("Columns".to_string());
        assert_eq!(
            list.visible_rows().len(),
            1,
            "sanity: the query left one row"
        );
        list.selected = 0;
        list.settle_selection(Domain::Views);
        assert_eq!(list.selected, 0);
        list.move_selection(Domain::Views, NavCommand::Move(1));
        assert_eq!(
            list.selected, 0,
            "a motion with nowhere to go leaves the cursor put"
        );
    }

    /// Selected-row vocabulary controls both stepping hints and typed-entry hints.
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

        // Booleans step only. Multi-option choices step and open typeahead; a
        // single-option choice has no alternate value to select.
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
            RowVocabulary::StepsAndTypes
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

    /// the column stage is a PROJECTION over the same draft — the view's fields are
    /// stashed, the column's seven installed, and the view's own list stays reachable
    /// underneath (which is what lets the overlay writer keep rendering from it
    /// mid-stage). A step there folds onto the item, writes presentation alone, and
    /// leaving restores the view with the cursor back on the column.
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
        // The context the door installs: without it the stage opens but folds nothing.
        // Through the door's OWN builder, so this mirror of
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

    /// A second column entry must not overwrite the stashed parent fields with the
    /// installed presentation fields. Refuse it and retain the parent's column list.
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

    /// Leaving Column while Values owns the shared parent stash is inert. Check the
    /// projection discriminant before taking the stash.
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

    /// A path for the open column maps to its installed format field by name. Paths for
    /// another column or the view itself stay in the header.
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
