//! Dialog-free definition writes: `apply::queue_definition`,
//! `remove_definition`, `definition_owner` and `refresh_definitions_now`,
//! driven against a desk layer that defines a saved scope and a writable
//! user directory, with no object dialog open.

use super::*;
use crate::shell::objectdialog::apply::{self, Owner};

/// A builtin `risk` dataset with a `book` dimension, a desk `scopes.toml`
/// defining `desk_eu`, and a user directory the writes land in.
struct Fixture {
    _desk: tempfile::TempDir,
    user: tempfile::TempDir,
    shell: Entity<ShellView>,
    cx: gpui::VisualTestContext,
}

fn fixture(cx: &mut gpui::TestAppContext) -> Fixture {
    let desk = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    std::fs::write(
        desk.path().join("scopes.toml"),
        "config_version = 1\n[desk_eu]\n[desk_eu.dimensions]\nbook = [\"BK001\"]\n",
    )
    .unwrap();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n",
    )
    .unwrap();
    let mut services = test_services();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            datasets,
        ],
        desk: Some(desk.path().to_path_buf()),
        user: Some(user.path().to_path_buf()),
    });
    let (window, mut vcx) = open_shell_with_user_dir(cx, services, user.path());
    let shell = shell_of(&window, &mut vcx);
    Fixture {
        _desk: desk,
        user,
        shell,
        cx: vcx,
    }
}

/// A saved scope selecting `books` of `book`, as the scopes document spells it.
fn scope_value(books: &[&str]) -> toml::Value {
    let mut dims = toml::Table::new();
    dims.insert(
        "book".to_string(),
        toml::Value::Array(
            books
                .iter()
                .map(|b| toml::Value::String((*b).to_string()))
                .collect(),
        ),
    );
    let mut object = toml::Table::new();
    object.insert("dimensions".to_string(), toml::Value::Table(dims));
    toml::Value::Table(object)
}

fn queue(f: &mut Fixture, name: &str, value: toml::Value) -> Result<Option<String>, String> {
    f.cx.update(|_, cx| {
        f.shell.update(cx, |shell, cx| {
            apply::queue_definition(shell, "scopes", name, value, cx)
        })
    })
}

fn flush(cx: &mut gpui::VisualTestContext) {
    cx.executor()
        .advance_clock(apply::WRITE_DEBOUNCE + std::time::Duration::from_millis(10));
    cx.run_until_parked();
}

fn sidecar(f: &Fixture) -> String {
    std::fs::read_to_string(f.user.path().join("overrides.toml")).unwrap_or_default()
}

/// A new name lands in the user layer, and the frame resolves it the moment
/// `refresh_definitions_now` runs — before the flush timer has fired.
#[gpui::test]
fn queue_definition_creates_a_user_entry_and_refresh_resolves_it_at_once(
    cx: &mut gpui::TestAppContext,
) {
    let mut f = fixture(cx);
    let notice = queue(&mut f, "mine", scope_value(&["BK002"]));
    assert_eq!(notice, Ok(None), "a new name forks nothing");
    f.cx.update(|_, cx| f.shell.update(cx, apply::refresh_definitions_now));
    let has_mine = f.cx.update(|_, cx| {
        f.shell
            .read(cx)
            .target_frame()
            .read(cx)
            .saved_scopes()
            .contains_key("mine")
    });
    assert!(
        has_mine,
        "the frame resolves the queued scope before the flush"
    );
    assert!(
        !f.user.path().join("scopes.toml").exists(),
        "nothing was written yet: the refresh came from the pending batch"
    );
    flush(&mut f.cx);
    let text = std::fs::read_to_string(f.user.path().join("scopes.toml")).unwrap();
    assert!(text.contains("[mine"), "{text}");
    assert!(text.contains("BK002"), "{text}");
}

/// Writing over the desk's entry forks it, records the sidecar entry in the
/// same batch, and announces the copy with the shadowed layer's name.
#[gpui::test]
fn queue_definition_over_an_inherited_entry_forks_and_says_so(cx: &mut gpui::TestAppContext) {
    let mut f = fixture(cx);
    let notice = queue(&mut f, "desk_eu", scope_value(&["BK002"]));
    assert_eq!(
        notice,
        Ok(Some(
            "copied 'desk_eu' to your config — r restores the desk copy".to_string()
        ))
    );
    flush(&mut f.cx);
    let overrides = sidecar(&f);
    assert!(overrides.contains("[\"scopes.desk_eu\"]"), "{overrides}");
    assert!(
        overrides.contains("shadowed_layer = \"desk\""),
        "{overrides}"
    );
    // A second write to the fork is the user's own: no second notice.
    let again = queue(&mut f, "desk_eu", scope_value(&["BK003"]));
    assert_eq!(again, Ok(None));
}

/// The owner read counts the pending batch: a name queued a moment ago is
/// already the user's, before the flush.
#[gpui::test]
fn definition_owner_counts_the_pending_batch(cx: &mut gpui::TestAppContext) {
    let mut f = fixture(cx);
    let owner = |f: &mut Fixture, name: &str| {
        let name = name.to_string();
        f.cx.update(|_, cx| apply::definition_owner(f.shell.read(cx), "scopes", &name))
    };
    assert_eq!(owner(&mut f, "mine"), Owner::Absent);
    assert_eq!(owner(&mut f, "desk_eu"), Owner::Inherited(Layer::Desk));
    queue(&mut f, "mine", scope_value(&["BK002"])).unwrap();
    queue(&mut f, "desk_eu", scope_value(&["BK002"])).unwrap();
    assert_eq!(owner(&mut f, "mine"), Owner::User { over: None });
    assert_eq!(
        owner(&mut f, "desk_eu"),
        Owner::User {
            over: Some(Layer::Desk)
        }
    );
}

/// Removing a fork reverts to the desk copy and takes the sidecar key with it.
#[gpui::test]
fn remove_definition_reverts_a_fork_to_the_lower_copy(cx: &mut gpui::TestAppContext) {
    let mut f = fixture(cx);
    queue(&mut f, "desk_eu", scope_value(&["BK002"])).unwrap();
    flush(&mut f.cx);
    assert!(sidecar(&f).contains("scopes.desk_eu"));
    let removed = f.cx.update(|_, cx| {
        f.shell.update(cx, |shell, cx| {
            apply::remove_definition(shell, "scopes", "desk_eu", cx)
        })
    });
    assert_eq!(removed, Ok(()));
    flush(&mut f.cx);
    let books = f.shell.read_with(&f.cx, |shell, _| {
        shell
            .services
            .config
            .doc("scopes")
            .and_then(|d| d.value.get("desk_eu"))
            .and_then(|v| v.get("dimensions"))
            .and_then(|v| v.get("book"))
            .cloned()
    });
    assert_eq!(
        books,
        Some(toml::Value::Array(vec![toml::Value::String(
            "BK001".to_string()
        )])),
        "the desk copy wins again"
    );
    let overrides = sidecar(&f);
    assert!(!overrides.contains("scopes.desk_eu"), "{overrides}");
    let owner =
        f.cx.update(|_, cx| apply::definition_owner(f.shell.read(cx), "scopes", "desk_eu"));
    assert_eq!(owner, Owner::Inherited(Layer::Desk));
}
