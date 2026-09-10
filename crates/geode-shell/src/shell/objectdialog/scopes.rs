//! The `Domain::Scopes` adapter (spec §8.4, as amended by a design review
//! ruling recorded on this branch — see this crate's Part 2a Task 5
//! report): the scopes a trader saves with `:scope save <name>` and
//! recalls with `:scope load <name>` or the palette's `scope::<name>`
//! actions.
//!
//! ## The thinnest adapter on purpose
//!
//! A scope's *values* are authored by the dimension picker and
//! `:scope save`, not here — a `MultiChoice` editor per dimension would
//! be a second surface that could drift from that one. So every field
//! this domain has is [`FieldKind::Text`], painted from the saved
//! scope's own content and never stepped: [`Draft::step_selected`] has
//! no arm that changes a `Text` value, so `space` on either row is
//! correctly "nothing changes with space", the same property
//! `groupings.rs`'s `slot` field leans on for the same reason.
//!
//! Two fields: `Selects` (the scope's dimension selections and its
//! validated expression, in the same "column ∈ values" spelling
//! `scopebar::build_model` uses for the toolbar's own chips) and
//! `Text filter` (the scope's plain-text filter, or `(none)`). The
//! design spec's own sketch (§8.4) has only `name`, a read-only summary
//! of what the scope selects; splitting the text filter into its own
//! row is this adapter's one addition, not a departure — it is still
//! read-only, still derived, and it is what makes the browse row's own
//! `summary` (which shows only what the scope *selects*) and the edit
//! stage's text filter row both legible on their own.
//!
//! **No `as-of` field.** A saved [`Scope`] carries no as-of at all —
//! `Frame::save_scope` saves `self.scope.clone()` alone
//! (`geode-shell/src/frame.rs`), and `scope_to_table`'s own three keys
//! (`dimensions`, `text`, `expression`) have no fourth. As-of is frame
//! state, not scope state; showing one here would be inventing a field
//! nothing writes.
//!
//! ## `o`: the one new verb
//!
//! `render.rs`'s `Verb('o')` arm (`arm_overwrite`) only *arms* the
//! confirm — it decides at that moment whether the write will also fork
//! the object (whether the user layer already owns this name), so the
//! prompt can disclose it, but it reads no `Frame`. The frame is read a
//! keystroke later, in `run_confirmed`'s `Confirm::Overwrite` arm: `shell.
//! frame.read(cx).scope().clone()` — the only place a `Frame` is read
//! for *this dialog* at all (narrower than "the only entity": the shared
//! filter field's `Entity<InputState>` is read elsewhere same as in every
//! other dialog). [`overwrite_with`] takes the resulting `Scope` value,
//! already read out, so this module, like every other adapter, never
//! touches a `Frame` or `gpui` itself.
//!
//! It replaces both `draft.source` (what [`to_table`] renders) and
//! `draft.fields` (the read-only summary painted above it) with the
//! frame's own scope. Only `source` is what `Draft::is_dirty` and
//! `Draft::writes_by_destination` actually key the write decision on —
//! comparing the two *painted* fields alone would make "did anything
//! change" a question about whether `selects_summary` happens to render
//! two different scopes identically, which is not a property it
//! promises to keep as it grows (see `Draft::baseline_source`'s own doc
//! in `mod.rs`). Rebuilding `fields` here is still necessary — nothing
//! else repaints them — it just is not what decides whether `o` writes.

use geode_core::config::{Config, Diagnostic, Layer, LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::scopes::{saved_scopes_from_doc, scope_to_table};

use super::{Destination, Draft, Field, FieldKind};

/// The config doc name (file stem), as `Config::layered_docs` keys it.
pub const DOC: &str = "scopes";

/// The muted second line of a scope's browse row: what it selects, in
/// the same "column ∈ values" spelling the scope bar's own chips use
/// (`scopebar::build_model`) — except never collapsed to a bare count
/// the way a chip is past two values, because a browse row has the
/// whole width of the dialog and nothing else competing for it.
///
/// Read straight off the raw TOML value rather than through
/// `saved_scopes_from_doc`, for the reason `views::summary` and
/// `groupings::summary` both give for doing the same: a malformed scope
/// is exactly the one a user opens this dialog to fix, and the reader
/// drops such a scope from the merged result entirely, which would leave
/// the row with nothing to show.
pub fn summary(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return "not a table".to_string();
    };
    selects_summary(table)
}

/// What a scope's raw table selects: one "column ∈ values" clause per
/// non-empty dimension, plus the expression's own source text when it
/// has one. Shared by [`summary`] (the browse row) and [`fields`] (the
/// edit stage's own `Selects` row), so the two can never describe the
/// same object differently.
fn selects_summary(table: &toml::Table) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(dims) = table.get("dimensions").and_then(|v| v.as_table()) {
        for (column, values) in dims {
            let values: Vec<&str> = values
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if values.is_empty() {
                continue;
            }
            parts.push(format!("{column} ∈ {}", values.join(", ")));
        }
    }
    if let Some(expr) = table
        .get("expression")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        parts.push(expr.to_string());
    }
    if parts.is_empty() {
        "everything".to_string()
    } else {
        parts.join("; ")
    }
}

/// The two read-only fields of one scope (this module's own doc has the
/// full reasoning), or of no scope at all when `object` names nothing —
/// "everything" selected and "(none)" for the text filter, which is what
/// a `Config` with no `scopes` doc, or a slot nothing defines, has to
/// produce rather than panicking (spec §4 has `fields` serve the create
/// path too, though Scopes has no create verb of its own — see this
/// module's own doc comment).
pub fn fields(config: &Config, object: Option<&str>) -> Vec<Field> {
    let table = object
        .and_then(|name| config.doc(DOC).and_then(|doc| doc.value.get(name)))
        .and_then(|value| value.as_table());
    fields_from_table(table)
}

/// The shared body of [`fields`] and [`overwrite_with`]: both need the
/// same two read-only rows built from a raw scope table, one read off
/// `Config`, the other freshly rendered from the frame's own scope.
fn fields_from_table(table: Option<&toml::Table>) -> Vec<Field> {
    let selects = table
        .map(selects_summary)
        .unwrap_or_else(|| "everything".to_string());
    let text = table
        .and_then(|t| t.get("text"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(none)")
        .to_string();
    vec![
        Field {
            key: "selects".to_string(),
            label: "Selects".to_string(),
            kind: FieldKind::Text(selects),
            dest: Destination::Doc,
        },
        Field {
            key: "text".to_string(),
            label: "Text filter".to_string(),
            kind: FieldKind::Text(text),
            dest: Destination::Doc,
        },
    ]
}

/// The draft rendered as `scopes.toml`'s own value for this object:
/// `draft.source`, unchanged, wrapped for [`super::Domain::to_table`]'s
/// sake.
///
/// Never derived from `draft.fields` — both fields are
/// [`FieldKind::Text`], painted summaries with nothing in the field
/// vocabulary that could reconstruct a `dimensions` sub-table or an
/// `expression` string from them. `draft.source` is the actual object,
/// exactly as `Domain::draft` first read it off the merged doc, and the
/// **only** thing that ever changes it is [`overwrite_with`] — the same
/// "the field vocabulary does not model this, so render from `source`"
/// shape `views::doc_table`'s own doc comment describes, taken all the
/// way to its limit: here, nothing but `source` is ever rendered.
pub fn to_table(draft: &Draft, _dest: Destination) -> toml_edit::Item {
    toml_edit::Item::Table(super::toml_table_to_edit(&draft.source))
}

/// Everything wrong with the draft as it stands (spec §7.2): the
/// rendered table, parsed back and read by exactly the reader that
/// decides which saved scopes `:scope load` and the palette's
/// `scope::<name>` actions can reach (`saved_scopes_from_doc`) — on the
/// object being edited alone, wrapped in a document of its own, for the
/// reason `views::validate` and `groupings::validate` both give for doing
/// the same: validating the whole merged doc would report every other
/// scope's problems against this one object.
///
/// `saved_scopes_from_doc` only ever produces `Severity::Warning`
/// diagnostics (a scope naming an unknown column, or an unparseable
/// expression, is dropped with a warning, never an error) — so this
/// adapter's `o` can never be refused by `apply::blocking_diagnostic`,
/// which only gates on `Severity::Error`.
pub fn validate(draft: &Draft, config: &Config) -> Vec<Diagnostic> {
    let table = rendered_doc_table(draft);
    let doc = merge_docs(
        DOC,
        &[LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: std::path::PathBuf::from("<draft>"),
            table,
        }],
    );
    let (schema, _) = config
        .doc("datasets")
        .map(SchemaSpec::from_doc)
        .unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    let (_saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
    diags
}

/// The draft's `scopes.toml` entry, rendered and parsed back the way the
/// loader would read it off disk.
fn rendered_doc_table(draft: &Draft) -> toml::Table {
    super::object_text(&draft.name, to_table(draft, Destination::Doc))
        .parse::<toml::Table>()
        .unwrap_or_default()
}

/// `o`, confirmed: overwrite `draft` with `scope`'s own contents.
///
/// `scope` is a plain value, not a `Frame` — `render.rs`'s
/// `run_confirmed` reads `shell.frame.read(cx).scope().clone()` at the
/// call site and hands the result in here, which is what keeps a gpui
/// `Entity` out of this module (and out of `Domain`'s whole surface)
/// entirely. Both `draft.source` (what [`to_table`] renders) and
/// `draft.fields` (the summary painted above it) are replaced —
/// `draft.source` is what `Draft::is_dirty` and
/// [`super::Draft::writes_by_destination`] key the actual write decision
/// on, so `apply::commit_edit` sees a change regardless of what the
/// painted summary says, and goes through that same door every other
/// field edit already does.
pub fn overwrite_with(draft: &mut Draft, scope: &Scope) {
    draft.source = scope_table_as_toml(scope);
    draft.fields = fields_from_table(Some(&draft.source));
}

/// `scope_to_table` (geode-core) as the `toml::Table` `Draft::source`
/// holds: an object's own inner table, not the object wrapped under its
/// own name.
///
/// Round-tripped through [`super::object_text`] — the one spelling of
/// what a flush actually produces — rather than hand-converted, for the
/// same reason `groupings::rendered_doc_table` goes through a real
/// parse instead of trusting a bespoke conversion: the one honest check
/// of what `saved_scopes_from_doc` will read back is the text a write
/// would really contain.
fn scope_table_as_toml(scope: &Scope) -> toml::Table {
    let text = super::object_text("scope", toml_edit::Item::Table(scope_to_table(scope)));
    text.parse::<toml::Table>()
        .ok()
        .and_then(|parsed| parsed.get("scope").and_then(|v| v.as_table()).cloned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::{Domain, EditRow};
    use super::*;
    use geode_core::config::ConfigSources;
    use geode_core::scope::DimensionSelection;

    /// A whole-document fixture (`[dimensions]\n...`), wrapped as the
    /// `toml::Value::Table` a scope's row actually is — `toml::Value`'s
    /// own `FromStr` parses one bare value expression, not a document
    /// with table headers, so this goes through `toml::Table` the same
    /// way `views`'s own test fixtures do.
    fn value(text: &str) -> toml::Value {
        toml::Value::Table(text.parse::<toml::Table>().expect("fixture value parses"))
    }

    #[test]
    fn the_summary_names_every_dimension_selection() {
        assert_eq!(
            summary(&value(
                "[dimensions]\nbook = [\"BK000\", \"BK003\", \"BK007\"]\n"
            )),
            "book ∈ BK000, BK003, BK007"
        );
    }

    /// A malformed scope is exactly the one a user opens the dialog to
    /// fix, so it still gets a row and the row still says what is wrong
    /// — the same rule `views::summary`'s and `groupings::summary`'s own
    /// tests pin.
    #[test]
    fn a_malformed_scope_still_describes_itself() {
        assert_eq!(summary(&toml::Value::String("oops".into())), "not a table");
    }

    /// An empty scope (no dimensions, no text, no expression) selects
    /// everything, and says so rather than showing a blank row.
    #[test]
    fn an_empty_scope_says_everything() {
        assert_eq!(summary(&value("[dimensions]\n")), "everything");
    }

    fn config_with_scope(scopes: &str) -> Config {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n",
        )
        .unwrap();
        let scopes = LayerDoc::builtin("scopes", scopes).unwrap();
        Config::load(&ConfigSources {
            builtin: vec![datasets, scopes],
            desk: None,
            user: None,
        })
    }

    /// `fields` reads both rows straight off the raw table: `Selects`
    /// matches `summary`'s own wording, and `Text filter` shows the
    /// scope's own text — or `(none)` when it has none, never a blank
    /// row a trader could mistake for "not loaded yet".
    #[test]
    fn fields_shows_selects_and_the_text_filter_read_only() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\"]\n\n[bare]\n",
        );
        let fields = Domain::Scopes.fields(&config, Some("mine"));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].key, "selects");
        assert_eq!(fields[0].kind, FieldKind::Text("book ∈ BK001".to_string()));
        assert_eq!(fields[1].key, "text");
        assert_eq!(fields[1].kind, FieldKind::Text("spx".to_string()));

        let bare = Domain::Scopes.fields(&config, Some("bare"));
        assert_eq!(bare[0].kind, FieldKind::Text("everything".to_string()));
        assert_eq!(bare[1].kind, FieldKind::Text("(none)".to_string()));
    }

    /// `fields` takes `Option<&str>` because `Domain::fields`'s own
    /// signature does; a scope nothing defines has to come back as
    /// "everything"/"(none)" rather than a panic.
    #[test]
    fn an_object_that_does_not_exist_has_the_empty_fields_rather_than_panicking() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        for object in [None, Some("nonesuch")] {
            let fields = Domain::Scopes.fields(&config, object);
            assert_eq!(fields[0].kind, FieldKind::Text("everything".to_string()));
            assert_eq!(fields[1].kind, FieldKind::Text("(none)".to_string()));
        }
    }

    /// Neither field's value changes with `space` — both are painted,
    /// never stepped, the same property `groupings.rs`'s `slot` field
    /// pins for the same reason (this module's own doc comment has the
    /// full story).
    #[test]
    fn neither_field_steps_with_space() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        for row in draft.rows() {
            let EditRow::Field(i) = row else {
                panic!("Scopes has no ordered-list rows, got {row:?}");
            };
            draft.selected = i;
            assert!(
                !draft.toggle_selected(),
                "field {i} must not change with space"
            );
        }
    }

    /// `to_table` renders `draft.source` verbatim — nothing in the field
    /// vocabulary feeds it, so a draft opened straight off a scope's own
    /// table round-trips through `saved_scopes_from_doc` unchanged (the
    /// round trip `Domain::validate` leans on).
    #[test]
    fn to_table_round_trips_through_saved_scopes_from_doc() {
        let config = config_with_scope(
            "[mine]\ntext = \"spx\"\n[mine.dimensions]\nbook = [\"BK001\", \"BK002\"]\n",
        );
        let draft = Domain::Scopes.draft(&config, "mine");
        assert!(draft.diagnostics.is_empty(), "{:?}", draft.diagnostics);
        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("mine", item);
        let table: toml::Table = text.parse().unwrap();
        let doc = merge_docs(
            DOC,
            &[LayerDoc {
                layer: Layer::User,
                name: DOC.to_string(),
                file: std::path::PathBuf::from("<test>"),
                table,
            }],
        );
        let (schema, _) = config
            .doc("datasets")
            .map(SchemaSpec::from_doc)
            .unwrap_or_default();
        let (dims, _) = config
            .doc("dimensions")
            .map(DerivedDimensions::from_doc)
            .unwrap_or_default();
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["mine"].text.as_deref(), Some("spx"));
        assert_eq!(
            saved["mine"].dimensions[0].values,
            vec!["BK001".to_string(), "BK002".to_string()]
        );
    }

    /// [`overwrite_with`]: the frame's scope replaces both the source
    /// `to_table` renders and the fields painted above it, and the two
    /// stay consistent with each other — the same object `fields_from_
    /// table` would build if this scope had been on disk all along.
    #[test]
    fn overwrite_with_replaces_source_and_fields_together() {
        let config = config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        let frame_scope = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".to_string(),
                values: vec!["BK002".to_string(), "BK003".to_string()],
            }],
            text: Some("spx".to_string()),
            ..Scope::default()
        };
        overwrite_with(&mut draft, &frame_scope);

        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Text("book ∈ BK002, BK003".to_string())
        );
        assert_eq!(draft.fields[1].kind, FieldKind::Text("spx".to_string()));

        let item = to_table(&draft, Destination::Doc);
        let text = super::super::object_text("mine", item);
        let table: toml::Table = text.parse().unwrap();
        let doc = merge_docs(
            DOC,
            &[LayerDoc {
                layer: Layer::User,
                name: DOC.to_string(),
                file: std::path::PathBuf::from("<test>"),
                table,
            }],
        );
        let (schema, _) = config
            .doc("datasets")
            .map(SchemaSpec::from_doc)
            .unwrap_or_default();
        let (dims, _) = config
            .doc("dimensions")
            .map(DerivedDimensions::from_doc)
            .unwrap_or_default();
        let (saved, diags) = saved_scopes_from_doc(&doc, &schema, &dims);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(saved["mine"], frame_scope);
    }

    /// The review's own named collision (Task 5 review round 1, MINOR):
    /// `selects_summary` joins a dimension's values with `", "`, so a
    /// single value that itself contains `", "` paints identically to
    /// two separate values. If dirtiness were judged from the painted
    /// `Selects` field alone, overwriting `["BK001", "BK002"]` with the
    /// single value `"BK001, BK002"` would look like no change at all
    /// and `o` would silently fail to write. Pins that `Draft::is_dirty`
    /// (and so `apply::commit_edit`) still sees it, because dirtiness is
    /// judged from `source` — the actual object — not its summary.
    #[test]
    fn overwrite_with_is_seen_even_when_the_painted_summary_collides() {
        let config =
            config_with_scope("[mine]\n[mine.dimensions]\nbook = [\"BK001\", \"BK002\"]\n");
        let mut draft = Domain::Scopes.draft(&config, "mine");
        assert!(!draft.is_dirty(), "a freshly opened draft starts clean");

        let colliding = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".to_string(),
                values: vec!["BK001, BK002".to_string()],
            }],
            ..Scope::default()
        };
        overwrite_with(&mut draft, &colliding);

        // The painted summary really does collide — otherwise this test
        // would not be exercising the branch it claims to.
        assert_eq!(
            draft.fields[0].kind,
            FieldKind::Text("book ∈ BK001, BK002".to_string()),
            "the two scopes must paint identically for this test to mean anything"
        );
        // ...but the draft is still dirty, and still queues a write,
        // because `source` — the actual object — is not the same table.
        assert!(
            draft.is_dirty(),
            "a genuinely different scope must register as dirty even when its summary collides"
        );
        assert!(
            draft
                .writes_by_destination()
                .contains_key(&Destination::Doc)
        );
    }

    /// The shared walk every domain gets for free (`Domain::objects`,
    /// `derive_rows`): a scope's key IS its name, and the provenance
    /// markers work over it exactly as they do over a view's or a
    /// grouping slot's.
    #[test]
    fn domain_scopes_lists_scopes_with_their_owning_layer() {
        let builtin = LayerDoc::builtin(
            "scopes",
            "[eu]\n[eu.dimensions]\nbook = [\"BK001\"]\n[us]\n[us.dimensions]\nbook = [\"BK002\"]\n",
        )
        .unwrap();
        let user = LayerDoc {
            layer: Layer::User,
            name: DOC.to_string(),
            file: "<test:user>".into(),
            table: "[eu]\n[eu.dimensions]\nbook = [\"BK099\"]\n"
                .parse()
                .unwrap(),
        };
        let config = Config::load(&ConfigSources {
            builtin: vec![builtin, user],
            desk: None,
            user: None,
        });
        let rows = Domain::Scopes.objects(&config);
        let eu = rows.iter().find(|r| r.name == "eu").expect("eu");
        let us = rows.iter().find(|r| r.name == "us").expect("us");
        assert_eq!(eu.layer, Layer::User);
        assert!(eu.overridden);
        assert_eq!(us.layer, Layer::Builtin);
        assert!(!us.overridden);
    }
}
