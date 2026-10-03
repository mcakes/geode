//! Setting a value's color from the row menu: the user-layer write behind
//! the pick list, and what the status bar says about it.

use std::path::Path;

use geode_core::colour::{NamedColours, ValuePick, ValueWrite, value_color_state, value_write};
use geode_core::config::{Layer, VALUE_COLORS_DOC};
use gpui::Context;

use super::ShellView;

/// Status notice: a pick with no user configuration directory to write.
pub const NO_USER_DIR: &str = "no user configuration directory: the color was not saved";

/// Apply `write` to `dimension.value` in the user layer's `value_colors`
/// document, preserving everything else in it (a `[dimension]` table or an
/// inline one alike). A `dimension` entry that is not a table is refused
/// and the file is left byte-for-byte untouched: overwriting it would drop
/// what the user wrote there.
pub(crate) fn persist(
    user_dir: &Path,
    dimension: &str,
    value: &str,
    write: &ValueWrite,
) -> Result<(), String> {
    let not_a_table =
        || format!("{VALUE_COLORS_DOC}.{dimension} is not a table (file left untouched)");
    crate::config_write::try_edit(user_dir, Layer::User, VALUE_COLORS_DOC, |doc| {
        match write {
            ValueWrite::Nothing => {}
            ValueWrite::Set(name) => {
                let entry = doc.entry(dimension).or_insert(toml_edit::table());
                let Some(table) = entry.as_table_like_mut() else {
                    return Err(not_a_table());
                };
                // One key whatever the value's text: the key is quoted when
                // it is not bare, never split on a dot into nested tables.
                table.insert(value, toml_edit::value(name.as_str()));
            }
            ValueWrite::Remove => {
                let emptied = match doc.get_mut(dimension) {
                    None => false,
                    Some(item) => {
                        let Some(table) = item.as_table_like_mut() else {
                            return Err(not_a_table());
                        };
                        table.remove(value);
                        table.is_empty()
                    }
                };
                if emptied {
                    doc.remove(dimension);
                }
            }
        }
        Ok(())
    })
}

/// What the status bar says once `pick` is saved for `value`.
pub(crate) fn notice(value: &str, pick: &ValuePick) -> String {
    match pick {
        ValuePick::Color(name) => format!("{value} colored {name}"),
        ValuePick::None => format!("{value} color cleared"),
        ValuePick::FollowDesk => format!("{value} follows the desk"),
    }
}

impl ShellView {
    /// Make `pick` the color of `value` of `dimension` in the user layer.
    /// The write is decided against the layers as loaded, queued in UI
    /// order through the directory FIFO, and runs off the UI thread; the
    /// notice follows its result (the writer's error on failure, the file
    /// unchanged), and the ordinary reload repaints. A pick that changes
    /// nothing writes nothing and says nothing.
    pub(crate) fn set_value_color(
        &mut self,
        dimension: String,
        value: String,
        pick: ValuePick,
        cx: &mut Context<Self>,
    ) {
        let mut state = value_color_state(
            self.services.config.layered_docs(VALUE_COLORS_DOC),
            &dimension,
            &value,
        );
        // Decided against the color as painted, from the mapping and the
        // `colors.toml` definitions as the config holds them now, at the
        // pick (a reload while the list was open is honoured): a name
        // `colors.toml` no longer defines paints nothing, so `None` over it
        // is no change. Without this an untouched
        // enter would write `none` over a desk entry, masking it even after
        // the desk defines the name again.
        let (named, _) = NamedColours::from_config(&self.services.config);
        state.effective = state.effective.filter(|name| named.get(name).is_some());
        let write = value_write(&state, &pick);
        if write == ValueWrite::Nothing {
            return;
        }
        let Some(dir) = self.user_dir.clone() else {
            self.notice = Some(NO_USER_DIR.into());
            cx.notify();
            return;
        };
        let done = notice(&value, &pick);
        let task = crate::config_write::submit(&dir.clone(), cx.background_executor(), move || {
            persist(&dir, &dimension, &value, &write)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |shell, cx| {
                shell.notice = Some(match result {
                    Ok(()) => done.into(),
                    Err(e) => e.into(),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{ValueColors, ValueWrite};
    use geode_core::config::{Layer, LayerDoc, VALUE_COLORS_DOC, merge_docs};

    fn read(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("value_colors.toml")).unwrap()
    }

    fn colors(text: &str) -> ValueColors {
        let layer = LayerDoc {
            layer: Layer::User,
            name: VALUE_COLORS_DOC.into(),
            file: "value_colors.toml".into(),
            table: text.parse().unwrap(),
        };
        ValueColors::from_doc(&merge_docs(VALUE_COLORS_DOC, &[layer])).0
    }

    #[test]
    fn a_set_creates_the_file_and_keeps_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        persist(
            dir.path(),
            "underlying_ref",
            "SPX",
            &ValueWrite::Set("blue".into()),
        )
        .unwrap();
        let text = read(dir.path());
        assert!(text.starts_with("config_version = 1"), "{text}");
        persist(
            dir.path(),
            "underlying_ref",
            "NDX",
            &ValueWrite::Set("amber".into()),
        )
        .unwrap();
        persist(dir.path(), "book", "BK1", &ValueWrite::Set("teal".into())).unwrap();
        let read = colors(&read(dir.path()));
        assert_eq!(
            read.get("underlying_ref", "SPX").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(
            read.get("underlying_ref", "NDX").map(|c| &**c),
            Some("amber")
        );
        assert_eq!(read.get("book", "BK1").map(|c| &**c), Some("teal"));
    }

    #[test]
    fn a_remove_drops_the_key_and_an_emptied_dimension() {
        let dir = tempfile::tempdir().unwrap();
        persist(
            dir.path(),
            "underlying_ref",
            "SPX",
            &ValueWrite::Set("blue".into()),
        )
        .unwrap();
        persist(
            dir.path(),
            "underlying_ref",
            "NDX",
            &ValueWrite::Set("amber".into()),
        )
        .unwrap();
        persist(dir.path(), "underlying_ref", "SPX", &ValueWrite::Remove).unwrap();
        let text = read(dir.path());
        assert!(!text.contains("SPX"), "{text}");
        assert!(text.contains("NDX"), "{text}");
        persist(dir.path(), "underlying_ref", "NDX", &ValueWrite::Remove).unwrap();
        let text = read(dir.path());
        assert!(
            !text.contains("underlying_ref"),
            "the emptied table goes: {text}"
        );
        // Removing what is not there is not an error.
        persist(dir.path(), "underlying_ref", "DAX", &ValueWrite::Remove).unwrap();
    }

    #[test]
    fn a_dotted_value_is_written_as_one_quoted_key() {
        let dir = tempfile::tempdir().unwrap();
        for value in ["BRK.B", "SX5E Index"] {
            persist(
                dir.path(),
                "underlying_ref",
                value,
                &ValueWrite::Set("blue".into()),
            )
            .unwrap();
        }
        let read = colors(&read(dir.path()));
        assert_eq!(
            read.get("underlying_ref", "BRK.B").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(
            read.get("underlying_ref", "SX5E Index").map(|c| &**c),
            Some("blue")
        );
        assert_eq!(
            read.get("underlying_ref", "BRK"),
            None,
            "not a nested table"
        );
    }

    #[test]
    fn a_write_into_a_non_table_dimension_is_refused_and_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value_colors.toml");
        let before = "config_version = 1\n# mine\nunderlying_ref = \"blue\"\n";
        std::fs::write(&path, before).unwrap();
        let err = persist(
            dir.path(),
            "underlying_ref",
            "SPX",
            &ValueWrite::Set("blue".into()),
        )
        .unwrap_err();
        assert!(
            err.contains("value_colors.underlying_ref is not a table"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // A remove under it is refused too: nothing to remove from.
        let err = persist(dir.path(), "underlying_ref", "SPX", &ValueWrite::Remove).unwrap_err();
        assert!(err.contains("is not a table"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn an_inline_dimension_table_is_written_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("value_colors.toml");
        std::fs::write(&path, "config_version = 1\nbook = { BK1 = \"teal\" }\n").unwrap();
        persist(dir.path(), "book", "BK2", &ValueWrite::Set("blue".into())).unwrap();
        let read = colors(&read(dir.path()));
        assert_eq!(read.get("book", "BK1").map(|c| &**c), Some("teal"));
        assert_eq!(read.get("book", "BK2").map(|c| &**c), Some("blue"));
    }

    #[test]
    fn the_notice_says_what_the_pick_did() {
        use geode_core::colour::ValuePick as P;
        assert_eq!(notice("SPX", &P::Color("blue".into())), "SPX colored blue");
        assert_eq!(notice("SPX", &P::None), "SPX color cleared");
        assert_eq!(notice("SPX", &P::FollowDesk), "SPX follows the desk");
    }
}
