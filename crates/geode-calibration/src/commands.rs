//! Pure parsing and completion vocabulary for the tile's `:` line.
//! Commands come back as data; the tile owns every change they make.
//!
//! Vocabulary: `run [stage]` runs the whole pipeline, or `stage` and its
//! consumers; `publish` asks the y/n question `p` asks; `pipeline <name>`
//! shows another pipeline of the document; `layout` places the graph
//! again from the home scroll.

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `Run { from }`: every stage with `None`.
    Run(Option<String>),
    Publish,
    Pipeline(String),
    Layout,
}

/// Completion verbs, in the order offered.
const VERBS: [&str; 4] = ["run", "publish", "pipeline", "layout"];

/// Parse a line without its leading colon.
pub fn parse(line: &str) -> Result<Command, String> {
    let line = line.trim();
    let (verb, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let words: Vec<&str> = rest.split_whitespace().collect();
    match (verb, words.as_slice()) {
        ("run", []) => Ok(Command::Run(None)),
        ("run", [stage]) => Ok(Command::Run(Some((*stage).to_string()))),
        ("run", _) => Err("usage: :run [stage]".into()),
        ("publish", []) => Ok(Command::Publish),
        ("publish", _) => Err("usage: :publish".into()),
        ("pipeline", [name]) => Ok(Command::Pipeline((*name).to_string())),
        ("pipeline", _) => Err("usage: :pipeline <name>".into()),
        ("layout", []) => Ok(Command::Layout),
        ("layout", _) => Err("usage: :layout".into()),
        (other, _) => Err(format!("unknown command '{other}'")),
    }
}

/// Unranked candidates for the word under `cursor` (a byte offset): the
/// verbs, then the shown pipeline's stage names after `run` and the
/// document's pipeline names after `pipeline`. The word under the cursor
/// is the shell's to rank; the words before it decide which position is
/// being completed.
pub fn completions(
    line: &str,
    cursor: usize,
    stages: &[String],
    pipelines: &[String],
) -> Vec<String> {
    let mut cursor = cursor.min(line.len());
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let mut words: Vec<&str> = line[..cursor].split(char::is_whitespace).collect();
    words.pop();
    words.retain(|w| !w.is_empty());
    match words.as_slice() {
        [] => VERBS.iter().map(|v| (*v).to_string()).collect(),
        ["run"] => stages.to_vec(),
        ["pipeline"] => pipelines.to_vec(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_every_command() {
        assert_eq!(parse("run"), Ok(Command::Run(None)));
        assert_eq!(
            parse(" run  parity "),
            Ok(Command::Run(Some("parity".into())))
        );
        assert_eq!(parse("publish"), Ok(Command::Publish));
        assert_eq!(
            parse("pipeline implied_forwards"),
            Ok(Command::Pipeline("implied_forwards".into()))
        );
        assert_eq!(parse("layout"), Ok(Command::Layout));
    }

    #[test]
    fn parse_refuses_with_the_documented_words() {
        let err = |l: &str| parse(l).unwrap_err();
        assert_eq!(err("run a b"), "usage: :run [stage]");
        assert_eq!(err("publish now"), "usage: :publish");
        assert_eq!(err("pipeline"), "usage: :pipeline <name>");
        assert_eq!(err("pipeline a b"), "usage: :pipeline <name>");
        assert_eq!(err("layout x"), "usage: :layout");
        assert_eq!(err("bogus"), "unknown command 'bogus'");
        assert_eq!(err(""), "unknown command ''");
    }

    #[test]
    fn completions_offer_stage_names_after_run_and_pipelines_after_pipeline() {
        let stages: Vec<String> = ["chain", "parity", "publish"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let pipelines: Vec<String> = ["implied_forwards", "funding_only"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(
            completions("", 0, &stages, &pipelines),
            ["run", "publish", "pipeline", "layout"]
        );
        assert_eq!(
            completions("pu", 2, &stages, &pipelines),
            ["run", "publish", "pipeline", "layout"],
            "the verb under the cursor is the shell's to rank"
        );
        assert_eq!(completions("run ", 4, &stages, &pipelines), stages);
        assert_eq!(completions("run par", 7, &stages, &pipelines), stages);
        assert_eq!(completions("pipeline ", 9, &stages, &pipelines), pipelines);
        assert!(completions("publish ", 8, &stages, &pipelines).is_empty());
        assert!(completions("layout ", 7, &stages, &pipelines).is_empty());
        assert!(
            completions("run parity ", 11, &stages, &pipelines).is_empty(),
            "one stage at most"
        );
        // A cursor inside a multibyte character falls back to its start.
        assert_eq!(
            completions("run \u{e9}", 5, &stages, &pipelines),
            stages,
            "the word under the cursor is left to the shell"
        );
    }
}
