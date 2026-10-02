//! Position-system commands: the vocabulary a tile sends (`MoveLhuParams`),
//! the answer it gets back (`CommandOutcome`), the `positions.toml` reader
//! (`PositionsSpec`), and the notice wording the action and the shell share.
//!
//! `positions.toml` names the one position service as `[service] adapter =
//! "<name>"`. It is read at startup; the reader performs no I/O, and the app
//! resolves the adapter's capability before the data service starts. A
//! change to the file takes effect only after a restart.

use crate::config::{Diagnostic, MergedDoc, Severity};

/// Move every `positions` row to LHU `lhu` in the position system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveLhuParams {
    /// The requester's command counter, echoed in the outcome.
    pub tag: u64,
    pub positions: Vec<String>,
    pub lhu: String,
}

/// The position system's answer to one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub tag: u64,
    /// How many positions the command named.
    pub count: usize,
    pub lhu: String,
    pub result: Result<(), String>,
}

/// `positions.toml`: the one position service (`[service] adapter = "…"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionsSpec {
    pub adapter: String,
}

/// "position" or "positions" for `n`.
pub fn noun(n: usize) -> &'static str {
    if n == 1 { "position" } else { "positions" }
}

/// The notice for a command sent: the move is on its way, not yet accepted.
pub fn sent_notice(count: usize, lhu: &str) -> String {
    format!("moving {count} {} to LHU {lhu} \u{b7} sent", noun(count))
}

/// The notice for the position system's answer to a command.
pub fn outcome_notice(o: &CommandOutcome) -> String {
    match &o.result {
        Ok(()) => format!(
            "moving {} {} to LHU {} \u{b7} accepted",
            o.count,
            noun(o.count),
            o.lhu
        ),
        Err(reason) => format!("move to LHU {} refused: {reason}", o.lhu),
    }
}

fn diag(severity: Severity, path: &str, message: impl std::fmt::Display) -> Diagnostic {
    Diagnostic {
        severity,
        layer: None,
        file: None,
        message: format!("positions: {message}"),
        path: Some(path.to_string()),
    }
}

/// Read `positions.toml`. No document, or no `[service]` table, configures
/// no service and says nothing. A `[service]` without a string `adapter` is
/// an error and configures nothing. Unknown keys, at the top level or in
/// `[service]`, are warnings and are ignored. `config_version` is the
/// document's version stamp and is skipped.
pub fn from_doc(doc: &MergedDoc) -> (Option<PositionsSpec>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut service = None;
    for (key, value) in &doc.value {
        match key.as_str() {
            "config_version" => {}
            "service" => service = Some(value),
            other => diags.push(diag(
                Severity::Warning,
                &format!("positions.{other}"),
                format!("unknown key '{other}' ignored"),
            )),
        }
    }
    let Some(service) = service else {
        return (None, diags);
    };
    let Some(table) = service.as_table() else {
        diags.push(diag(
            Severity::Error,
            "positions.service",
            "'service' must be a table",
        ));
        return (None, diags);
    };
    for key in table.keys().filter(|k| k.as_str() != "adapter") {
        diags.push(diag(
            Severity::Warning,
            &format!("positions.service.{key}"),
            format!("unknown key '{key}' ignored"),
        ));
    }
    match table.get("adapter").and_then(|v| v.as_str()) {
        Some(adapter) => (
            Some(PositionsSpec {
                adapter: adapter.to_string(),
            }),
            diags,
        ),
        None => {
            diags.push(diag(
                Severity::Error,
                "positions.service.adapter",
                "'service' needs a string 'adapter'",
            ));
            (None, diags)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};

    fn from(text: &str) -> (Option<PositionsSpec>, Vec<Diagnostic>) {
        let doc = merge_docs(
            "positions",
            &[LayerDoc::builtin("positions", text).unwrap()],
        );
        from_doc(&doc)
    }

    #[test]
    fn no_document_configures_nothing() {
        assert_eq!(from_doc(&merge_docs("positions", &[])), (None, Vec::new()));
        assert_eq!(from("config_version = 1\n"), (None, Vec::new()));
    }

    #[test]
    fn a_service_names_its_adapter() {
        let (spec, diags) = from("config_version = 1\n[service]\nadapter = \"sim_positions\"\n");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            spec,
            Some(PositionsSpec {
                adapter: "sim_positions".into()
            })
        );
    }

    #[test]
    fn a_service_without_an_adapter_is_refused() {
        let (spec, diags) = from("[service]\nadapter = 5\n");
        assert_eq!(spec, None);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("positions.service.adapter"));

        let (spec, diags) = from("[service]\n");
        assert_eq!(spec, None);
        assert_eq!(diags[0].path.as_deref(), Some("positions.service.adapter"));
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let (spec, diags) = from("stray = 3\n[service]\nadapter = \"a\"\nretries = 2\n");
        assert_eq!(
            spec,
            Some(PositionsSpec {
                adapter: "a".into()
            })
        );
        let paths: Vec<_> = diags.iter().map(|d| d.path.as_deref().unwrap()).collect();
        assert_eq!(paths, ["positions.stray", "positions.service.retries"]);
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
    }

    #[test]
    fn notices_name_one_position_and_many_positions() {
        assert_eq!(
            sent_notice(1, "L7"),
            "moving 1 position to LHU L7 \u{b7} sent"
        );
        assert_eq!(
            sent_notice(3, "L7"),
            "moving 3 positions to LHU L7 \u{b7} sent"
        );
        let accepted = |count| CommandOutcome {
            tag: 1,
            count,
            lhu: "L7".into(),
            result: Ok(()),
        };
        assert_eq!(
            outcome_notice(&accepted(1)),
            "moving 1 position to LHU L7 \u{b7} accepted"
        );
        assert_eq!(
            outcome_notice(&accepted(3)),
            "moving 3 positions to LHU L7 \u{b7} accepted"
        );
    }

    #[test]
    fn a_refusal_names_the_target_and_reason() {
        let o = CommandOutcome {
            tag: 1,
            count: 2,
            lhu: "L7".into(),
            result: Err("unknown position P99".into()),
        };
        assert_eq!(
            outcome_notice(&o),
            "move to LHU L7 refused: unknown position P99"
        );
    }
}
