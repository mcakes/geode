# Scope Expression Suggestions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Suggest columns, operators, values and connectives while the user
types a scope expression. This covers the frame scope expression dialog (all
three modes) and the Scopes object dialog's `expression` field. Both surfaces
also gain live schema warnings and refuse an unknown column when the user
presses Enter.

**Architecture:**

- **Core reader.** A forgiving tokenizer and a grammar replay in `geode-core`
  (`scope::complete`) say what may come next at the caret. An `ExprVocab`
  built from the schema supplies column names, roles and types, and it
  drives the warnings.
- **Suggestion state.** A pure `ExprCompletion` in `geode-shell` turns that
  answer into ranked rows, a hint line, a warning and a per-column values
  cache.
- **Controller.** One gpui controller, `shell/expr_suggest.rs`:
  - observes the shared dialog input;
  - claims tab, shift+tab and the navigation keys;
  - writes accepted rows as a single, undoable range replace;
  - requests distinct values under a new reserved query key;
  - renders the hint, rows and warning for both surfaces.

**Tech Stack:** Rust, GPUI and gpui-component 0.6.2 (`gpui-base` input), and
DuckDB for the existing distinct query. The code uses no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-26-scope-expression-suggestions-design.md`

## Global Constraints

- Work in a worktree branch (`worktree-scope-expr-suggestions`). Do not
  commit feature work on main.
- Every commit message ends with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- `geode-core` stays pure: no I/O, no gpui.
- Never mutate state, perform I/O or allocate unbounded work during render.
  Rows are built in `refresh` and the renderer only paints them.
- Use theme tokens and the rem scale (`scale::design`) only. No literal
  colours, radii or unexplained pixels.
- Stable element IDs are derived from the row label, never from its
  position.
- User-facing copy says "color", not "colour", and describes behaviour
  plainly.
- The spec's operator sets are the source of truth:

  | Column kind | Operators |
  |---|---|
  | text (categorical or not) | `= != in like` |
  | number, date, timestamp | `= != < <= > >= in` |
  | bool | `= !=` |
  | derived | `= != in` |
  | unknown column | all eight |

  `<>` is never offered.
- Value lists come from the data only for **categorical** text columns. A
  derived dimension lists its labels, and bool lists `true`/`false`. Every
  other kind shows a hint and no list.
- Keep at most **50** ranked rows. The hint states the total count.
- Values are narrowed by the scope the finished expression will be ANDed
  with, never by the text in progress:

  | Surface | Scope used |
  |---|---|
  | Whole mode | The frame scope with its expression removed. |
  | Add mode | The whole frame scope. |
  | Term mode | The frame scope minus the edited term. |
  | Scopes dialog | The edited scope's selections and text, with its expression removed. |

- An empty `ExprVocab` (no `datasets` doc) disables the schema checks. They
  refuse nothing and warn about nothing, so fixtures without a schema keep
  working.
- Run commands from the worktree root. Do not run
  `cargo bench --workspace --no-run` locally. It is slow and CI covers it.
  `cargo check -p geode-shell --benches` covers the one bench edit.

## Review Focus

These are the five failure modes most likely to bite a user and that the
spec's own tests don't obviously cover. Each one's test lives in the task
named.

1. **Multibyte text before the caret** (`'Zürich'`, an em dash in a value).
   Every range written must land on character boundaries, and no slice may
   panic. Covered by tests in Tasks 1 and 3.
2. **A caret moved by a click or an arrow, without typing.** The list must
   follow the caret. Tab must never write at a stale range computed before
   the move. Covered by a test in Task 4.
3. **A values reply arriving after the dialog has closed or the column has
   changed.** It must be dropped silently. It must never fill another
   dialog's rows or panic. Covered by a test in Task 4.
4. **A config hot reload while the dialog is open.** The vocab must be
   rebuilt so a newly added column is suggested, not flagged unknown.
   Covered by a test in Task 4.
5. **Accepting a suggestion in the Scopes dialog.** The draft query must be
   updated with the inserted text before `sync_dialog_text` runs. Otherwise
   the text snaps back to the pre-insert value. Covered by a test in Task 5.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/geode-core/src/scope/complete.rs` (new) | `lex`, `context_at`, `ExprVocab`, `check`. Pure. |
| `crates/geode-core/src/scope/expr.rs` (modify) | `derived_op_error`, shared with `Scope::validate`. |
| `crates/geode-core/src/scope/mod.rs` (modify) | `pub mod complete;`. `validate` uses `derived_op_error`. |
| `crates/geode-shell/src/exprcomplete.rs` (new) | `ExprCompletion`: rows, hint, warning, values cache, writes. Pure. |
| `crates/geode-shell/src/lib.rs` (modify) | `pub mod exprcomplete;` |
| `crates/geode-shell/src/shell/expr_suggest.rs` (new) | The gpui controller and renderer shared by both surfaces. |
| `crates/geode-shell/src/shell/mod.rs` (modify) | `EXPR_KEY`, `expr_vocab` field plus builder, `expr_scroll`, the observe hook, and the `deliver_distinct` arm. |
| `crates/geode-shell/src/shell/hot_reload.rs` (modify) | Rebuild `expr_vocab` on a datasets or dimensions change. |
| `crates/geode-shell/src/shell/scope_expr_view.rs` (modify) | `completion` in state, `request_scope`, keys, render, Enter schema refusal. |
| `crates/geode-shell/src/shell/objectdialog/mod.rs` (modify) | An `expr: Option<ExprCompletion>` field on `ObjectDialogState`. |
| `crates/geode-shell/src/shell/objectdialog/render.rs` (modify) | Claim keys in `handle_text_key`, render under the open field, Enter schema refusal, footer hints. |
| `crates/geode-shell/src/shell/objectdialog/scopes.rs` (modify) | `expression_scope(draft, config)` for value narrowing. |
| `crates/geode-shell/benches/shell_cores.rs` (modify) | `bench_expr_complete`. |
| `scripts/mutation-check.sh` (modify) | Targeted entries (Tasks 1–5). |
| Docs (Task 6) | The current guides, READMEs, the perf log and the stale blotter comment. |

---

### Task 1: Core tokenizer and caret context (`geode-core`)

**Files:**
- Create: `crates/geode-core/src/scope/complete.rs`
- Modify: `crates/geode-core/src/scope/mod.rs` (add `pub mod complete;` next
  to `pub mod expr;`)
- Test: unit tests inside `complete.rs`

**Interfaces:**
- Produces:
  - `pub enum TokenKind { Word, Keyword(&'static str), Op(&'static str), Str { value: String, closed: bool }, Num, LParen, RParen, Comma, Other }`
  - `pub struct Token { pub kind: TokenKind, pub span: Range<usize> }`
  - `pub fn lex(text: &str) -> Vec<Token>`
  - `pub enum Position { Column, Operator { column: String }, OpenList { column: String }, Value { column: String, listed: Vec<String> }, Connective { open_parens: usize, in_list: bool }, Invalid }`
  - `pub struct Context { pub position: Position, pub token: Range<usize>, pub typed: String }`
  - `pub fn context_at(text: &str, caret: usize) -> Context`
  - `pub(crate) fn walk(text: &str, tokens: &[Token], on_term: &mut impl FnMut(Term)) -> Position`,
    where `pub(crate) struct Term { pub column: String, pub column_span: Range<usize>, pub op: Option<(&'static str, Range<usize>)> }`.
    Task 2's `check` uses it.

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-core/src/scope/complete.rs` with only the test module
and a stub module doc. Add `pub mod complete;` to `scope/mod.rs`.

```rust
//! Caret-aware reading of a partially typed scope expression, for the
//! suggestion lists. (Filled in by Step 3.)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::parse_expr;

    fn ctx(text: &str, caret: usize) -> (Position, std::ops::Range<usize>, String) {
        let c = context_at(text, caret);
        (c.position, c.token, c.typed)
    }

    fn col(c: &str) -> String {
        c.to_string()
    }

    #[test]
    fn empty_and_operand_positions_want_a_column() {
        assert_eq!(ctx("", 0), (Position::Column, 0..0, String::new()));
        assert_eq!(ctx("bo", 2), (Position::Column, 0..2, "bo".into()));
        assert_eq!(ctx("not (", 5).0, Position::Column);
        assert_eq!(ctx("a = 1 and ", 10).0, Position::Column);
    }

    #[test]
    fn after_a_column_an_operator() {
        assert_eq!(
            ctx("book ", 5),
            (Position::Operator { column: col("book") }, 5..5, String::new())
        );
        assert_eq!(
            ctx("book l", 6),
            (Position::Operator { column: col("book") }, 5..6, "l".into())
        );
        assert_eq!(ctx("book in ", 8).0, Position::OpenList { column: col("book") });
    }

    #[test]
    fn after_an_operator_or_inside_a_list_a_value() {
        let value = |listed: &[&str]| Position::Value {
            column: col("book"),
            listed: listed.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(ctx("book =", 6), (value(&[]), 6..6, String::new()));
        assert_eq!(ctx("book = 'E", 9), (value(&[]), 7..9, "E".into()));
        assert_eq!(ctx("book = EM", 9), (value(&[]), 7..9, "EM".into()));
        assert_eq!(ctx("book in (", 9).0, value(&[]));
        assert_eq!(ctx("book in ('A', ", 14).0, value(&["A"]));
        assert_eq!(ctx("book = 'O''N", 12).2, "O'N");
    }

    #[test]
    fn a_finished_term_wants_a_connective() {
        let conn = |open_parens, in_list| Position::Connective { open_parens, in_list };
        assert_eq!(ctx("book = 'E'", 10).0, conn(0, false), "a closed string is complete");
        assert_eq!(ctx("book = 'E' ", 11).0, conn(0, false));
        assert_eq!(ctx("(a = 1 ", 7).0, conn(1, false));
        assert_eq!(ctx("book in ('A' ", 13).0, conn(0, true));
        assert_eq!(ctx("a = 1 an", 8), (conn(0, false), 6..8, "an".into()));
    }

    #[test]
    fn a_caret_mid_word_replaces_the_whole_word_but_ranks_the_part_before_it() {
        assert_eq!(
            ctx("book = 'EMEA' and region = 'X'", 3),
            (Position::Column, 0..4, "boo".into())
        );
    }

    #[test]
    fn text_that_cannot_continue_is_invalid() {
        assert_eq!(ctx("a ! ", 4).0, Position::Invalid);
        assert_eq!(ctx("a = 1 )", 7).0, Position::Invalid, "no open paren to close");
    }

    #[test]
    fn multibyte_text_keeps_every_range_on_char_boundaries() {
        let text = "city = 'Zürich' and né";
        for caret in 0..=text.len() {
            if !text.is_char_boundary(caret) {
                continue;
            }
            let c = context_at(text, caret);
            assert!(text.is_char_boundary(c.token.start), "start at caret {caret}");
            assert!(text.is_char_boundary(c.token.end), "end at caret {caret}");
        }
        assert_eq!(ctx(text, text.len()), (Position::Column, 21..text.len(), "né".into()));
    }

    /// Every prefix of a valid expression, followed by a space, must be read
    /// the way the parser reads it. Where the parser stops at the end of the
    /// input wanting X, `context_at` must say X.
    #[test]
    fn context_agrees_with_the_parser_on_every_prefix() {
        let corpus = [
            "book = 'EQ-DERIV'",
            "book = 'EQ-DERIV' and region = 'EMEA'",
            "not (a = 1 or b != 2) and c in ('x', 'y''z')",
            "delta01 >= -1.5",
            "flag = true or flag = false",
            "name like 'sp%'",
            "(a = 1)",
            "x <> 3 and not y <= 4",
            "a in (1, 2, 3)",
        ];
        for full in corpus {
            for end in 0..=full.len() {
                if !full.is_char_boundary(end) {
                    continue;
                }
                let text = format!("{} ", &full[..end]);
                let caret = text.len();
                let Err(e) = parse_expr(&text) else {
                    assert!(
                        matches!(context_at(&text, caret).position, Position::Connective { in_list: false, .. }),
                        "{text:?} parses, so a connective must come next"
                    );
                    continue;
                };
                if e.caret != caret {
                    continue; // the parser stopped before the end
                }
                let got = context_at(&text, caret).position;
                let ok = match e.message.as_str() {
                    "expected a column name" => got == Position::Column,
                    "expected a comparison operator" => matches!(got, Position::Operator { .. }),
                    "expected a value" => matches!(got, Position::Value { .. }),
                    "expected '(' after 'in'" => matches!(got, Position::OpenList { .. }),
                    "expected ',' or ')'" => matches!(got, Position::Connective { in_list: true, .. }),
                    "expected ')'" => matches!(got, Position::Connective { open_parens: 1.., in_list: false }),
                    _ => true,
                };
                assert!(ok, "{text:?}: parser says {:?}, context says {got:?}", e.message);
            }
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core scope::complete`
Expected: a compile error, because `context_at` and `Position` are undefined.

- [ ] **Step 3: Implement**

Replace the stub doc and put the following above the test module:

```rust
//! Caret-aware reading of a partially typed scope expression, for the
//! suggestion lists. [`lex`] tokenizes what [`super::expr`]'s parser
//! accepts, but it never fails. An unterminated string, a lone `!` and an
//! open `in (` are tokens too. [`context_at`] replays the grammar over the
//! tokens before the caret and names what may come next. Tokens after the
//! caret are ignored, so a half-edited middle still gets suggestions.
//!
//! One known divergence from the parser: a word starting with a digit
//! reads as a number here. The parser would accept it as a column name, but
//! no schema column starts with a digit.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// A run of alphanumerics or `_` that is not a keyword.
    Word,
    /// `and`, `or`, `not`, `in`, `like`, `true` or `false`, lower-cased.
    Keyword(&'static str),
    /// `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=`.
    Op(&'static str),
    /// A quoted string, decoded (`''` is one quote). `closed` is false when
    /// the input ends inside it.
    Str { value: String, closed: bool },
    /// A run of `[0-9.+-]`, the parser's own number class.
    Num,
    LParen,
    RParen,
    Comma,
    /// Any other single character.
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// Byte range in the lexed text, on character boundaries.
    pub span: Range<usize>,
}

const KEYWORDS: [&str; 7] = ["and", "or", "not", "in", "like", "true", "false"];
/// Longest first, so `<=` is not read as `<` followed by `=`.
const OPS: [&str; 7] = ["!=", "<>", "<=", ">=", "=", "<", ">"];

fn run_len(s: &str, class: impl Fn(char) -> bool) -> usize {
    s.char_indices()
        .find(|&(_, c)| !class(c))
        .map_or(s.len(), |(i, _)| i)
}

/// Scan a string opened at byte `open`: its decoded value, whether it
/// closed, and the byte just past it.
fn scan_string(text: &str, open: usize) -> (String, bool, usize) {
    let mut value = String::new();
    let mut pos = open + 1;
    while let Some(c) = text[pos..].chars().next() {
        pos += c.len_utf8();
        if c == '\'' {
            if text[pos..].starts_with('\'') {
                value.push('\'');
                pos += 1;
                continue;
            }
            return (value, true, pos);
        }
        value.push(c);
    }
    (value, false, pos)
}

pub fn lex(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while let Some(c) = text[pos..].chars().next() {
        let start = pos;
        if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        }
        let kind = if c == '\'' {
            let (value, closed, end) = scan_string(text, pos);
            pos = end;
            TokenKind::Str { value, closed }
        } else if c.is_ascii_digit() || matches!(c, '.' | '+' | '-') {
            pos += run_len(&text[pos..], |c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-'));
            TokenKind::Num
        } else if c.is_alphanumeric() || c == '_' {
            pos += run_len(&text[pos..], |c| c.is_alphanumeric() || c == '_');
            let word = &text[start..pos];
            match KEYWORDS.iter().find(|k| k.eq_ignore_ascii_case(word)) {
                Some(k) => TokenKind::Keyword(k),
                None => TokenKind::Word,
            }
        } else if let Some(op) = OPS.iter().find(|op| text[pos..].starts_with(**op)) {
            pos += op.len();
            TokenKind::Op(op)
        } else {
            pos += c.len_utf8();
            match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                ',' => TokenKind::Comma,
                _ => TokenKind::Other,
            }
        };
        tokens.push(Token { kind, span: start..pos });
    }
    tokens
}

/// What may come next at the caret.
#[derive(Debug, Clone, PartialEq)]
pub enum Position {
    /// A column, `not` or `(`.
    Column,
    /// An operator for `column`: a comparison, `like` or `in`.
    Operator { column: String },
    /// The `(` that opens `column`'s `in` list.
    OpenList { column: String },
    /// A value for `column`. `listed` holds the values already in its `in`
    /// list, decoded.
    Value { column: String, listed: Vec<String> },
    /// A finished term. Outside a list: `and`, `or`, and `)` while
    /// `open_parens > 0`. Inside a list: `,` or `)`.
    Connective { open_parens: usize, in_list: bool },
    /// The text before the caret cannot continue into a valid expression.
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub position: Position,
    /// The byte range a suggestion replaces: the partial word, number or
    /// string under the caret, or an empty range at the caret.
    pub token: Range<usize>,
    /// The part of that token before the caret that ranking matches
    /// against. For a string it is decoded and has no quotes.
    pub typed: String,
}

/// A term `walk` found: its column, and its operator when it has one
/// (`in` is not an operator here).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Term {
    pub column: String,
    pub column_span: Range<usize>,
    pub op: Option<(&'static str, Range<usize>)>,
}

/// A word, keyword, number or unclosed string can still grow under the
/// caret. An operator or a bracket is already complete.
fn is_partial(t: &Token, caret: usize) -> bool {
    match &t.kind {
        TokenKind::Word | TokenKind::Keyword(_) | TokenKind::Num | TokenKind::Other => true,
        TokenKind::Str { closed, .. } => !(*closed && caret == t.span.end),
        _ => false,
    }
}

fn literal_value(text: &str, t: &Token) -> Option<String> {
    match &t.kind {
        TokenKind::Str { value, closed: true } => Some(value.clone()),
        TokenKind::Num => Some(text[t.span.clone()].to_string()),
        TokenKind::Keyword(k @ ("true" | "false")) => Some((*k).to_string()),
        _ => None,
    }
}

enum St {
    Operand,
    Column(String, Range<usize>),
    In(String),
    Op(String),
    List(String, Vec<String>),
    ListValue(String, Vec<String>),
    Term,
}

/// Replay the grammar over `tokens`, reporting each term's column and
/// operator to `on_term`, and return what may come next.
pub(crate) fn walk(text: &str, tokens: &[Token], on_term: &mut impl FnMut(Term)) -> Position {
    let mut st = St::Operand;
    let mut depth = 0usize;
    for t in tokens {
        let lit = literal_value(text, t);
        st = match (st, &t.kind, lit) {
            (St::Operand, TokenKind::Keyword("not"), _) => St::Operand,
            (St::Operand, TokenKind::LParen, _) => {
                depth += 1;
                St::Operand
            }
            (St::Operand, TokenKind::Word, _) => {
                St::Column(text[t.span.clone()].to_string(), t.span.clone())
            }
            (St::Column(c, span), TokenKind::Op(op), _) => {
                on_term(Term { column: c.clone(), column_span: span, op: Some((op, t.span.clone())) });
                St::Op(c)
            }
            (St::Column(c, span), TokenKind::Keyword("like"), _) => {
                on_term(Term { column: c.clone(), column_span: span, op: Some(("like", t.span.clone())) });
                St::Op(c)
            }
            (St::Column(c, span), TokenKind::Keyword("in"), _) => {
                on_term(Term { column: c.clone(), column_span: span, op: None });
                St::In(c)
            }
            (St::In(c), TokenKind::LParen, _) => St::List(c, Vec::new()),
            (St::Op(_), _, Some(_)) => St::Term,
            (St::List(c, mut listed), _, Some(v)) => {
                listed.push(v);
                St::ListValue(c, listed)
            }
            (St::ListValue(c, listed), TokenKind::Comma, _) => St::List(c, listed),
            (St::ListValue(..), TokenKind::RParen, _) => St::Term,
            (St::Term, TokenKind::Keyword("and" | "or"), _) => St::Operand,
            (St::Term, TokenKind::RParen, _) if depth > 0 => {
                depth -= 1;
                St::Term
            }
            _ => return Position::Invalid,
        };
    }
    match st {
        St::Operand => Position::Column,
        St::Column(column, span) => {
            on_term(Term { column: column.clone(), column_span: span, op: None });
            Position::Operator { column }
        }
        St::In(column) => Position::OpenList { column },
        St::Op(column) => Position::Value { column, listed: Vec::new() },
        St::List(column, listed) => Position::Value { column, listed },
        St::ListValue(..) => Position::Connective { open_parens: depth, in_list: true },
        St::Term => Position::Connective { open_parens: depth, in_list: false },
    }
}

/// What may come next at byte `caret` of `text` (clamped to its length),
/// and the token a suggestion replaces.
pub fn context_at(text: &str, caret: usize) -> Context {
    let caret = caret.min(text.len());
    let tokens = lex(text);
    let partial = tokens
        .iter()
        .find(|t| t.span.start < caret && caret <= t.span.end && is_partial(t, caret));
    let token = partial.map_or(caret..caret, |t| t.span.clone());
    let typed = match partial {
        Some(t @ Token { kind: TokenKind::Str { .. }, .. }) => {
            text[t.span.start + 1..caret].replace("''", "'")
        }
        Some(t) => text[t.span.start..caret].to_string(),
        None => String::new(),
    };
    let before: Vec<Token> = tokens
        .into_iter()
        .take_while(|t| t.span.end <= token.start)
        .collect();
    let position = walk(text, &before, &mut |_| {});
    Context { position, token, typed }
}
```

`Keyword(k)` in `lex` receives `&&'static str`. If the compiler rejects
`TokenKind::Keyword(k)`, write `TokenKind::Keyword(*k)`, and likewise
`TokenKind::Op(*op)` and `pos += op.len()`. `Term` is `pub(crate)`, so add
`#[allow(dead_code)]` on its fields only if Clippy flags them before Task 2
reads them.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core scope::complete`
Expected: all eight tests pass. If the agreement test fails, the failing
message names the prefix. Fix `walk`, not the test.

- [ ] **Step 5: Add a mutation entry, then commit**

In `scripts/mutation-check.sh`, add this just before the
`if [[ -n "$changed_ref" ]]; then` line near the end:

```zsh
# ---- Scope expression suggestions: the caret reader.
# After an operator the caret wants a value; reading it as a finished term
# would offer and/or where values belong.
run_mutation "expr suggest: after an operator comes a value" \
  crates/geode-core/src/scope/complete.rs \
  '            (St::Op(_), _, Some(_)) => St::Term,' \
  '            (St::Op(_), _, Some(_)) => St::Operand,' \
  geode-core \
  context_agrees_with_the_parser_on_every_prefix
```

Run: `zsh scripts/mutation-check.sh --anchors-only`. Expected: exit 0.
Run: `zsh scripts/mutation-check.sh "expr suggest"`. Expected: `KILLED`.

```bash
git add crates/geode-core/src/scope/complete.rs crates/geode-core/src/scope/mod.rs scripts/mutation-check.sh
git commit -m "feat(core): caret-aware reader for scope expressions

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Vocab, live schema warnings, shared derived-operator rule (`geode-core`)

**Files:**
- Modify: `crates/geode-core/src/scope/complete.rs`
- Modify: `crates/geode-core/src/scope/expr.rs` (add `derived_op_error`)
- Modify: `crates/geode-core/src/scope/mod.rs` (`validate` calls
  `derived_op_error`, and re-exports it)
- Test: unit tests in `complete.rs`

**Interfaces:**
- Consumes: `lex`, `walk` and `Term` (Task 1).
- Produces:
  - `pub enum ValueKind { Categorical, Text, Number, Bool, Date, Timestamp, Derived(Vec<String>) }`
  - `pub struct VocabColumn { pub name: String, pub role: &'static str, pub kind: ValueKind }`
  - `pub struct ExprVocab` (`Default`, `Clone`, `PartialEq`, `Debug`), with
    these methods:
    - `new(schema: &SchemaSpec, dims: &DerivedDimensions) -> Self`
    - `columns(&self) -> &[VocabColumn]`
    - `get(&self, name: &str) -> Option<&VocabColumn>`
    - `is_empty(&self) -> bool`
    - `nearest(&self, name: &str) -> Option<&str>`
  - `impl VocabColumn { pub fn detail(&self) -> String }` returns
    `"dimension · text"`, `"measure · number"`, `"derived"` and so on.
  - `pub struct Warning { pub span: Range<usize>, pub message: String }`
  - `pub fn check(text: &str, vocab: &ExprVocab, caret: Option<usize>) -> Vec<Warning>`
  - `geode_core::scope::derived_op_error(column: &str, op: &str) -> Option<String>`

- [ ] **Step 1: Write the failing tests**

Append to the test module in `complete.rs`:

```rust
    use crate::config::{LayerDoc, merge_docs};
    use crate::dimensions::DerivedDimensions;
    use crate::schema::SchemaSpec;

    fn vocab() -> ExprVocab {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [risk.columns.live]\ntype = \"bool\"\nrole = \"dimension\"\n\
             [risk.columns.expiry]\ntype = \"date\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let dims = LayerDoc::builtin(
            "dimensions",
            "[desk]\nfrom = \"book\"\n[desk.values]\nEQ = [\"BK000\"]\nRATES = [\"BK001\", \"BK002\"]\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs(&[datasets]));
        let (dims, _) = DerivedDimensions::from_doc(&merge_docs(&[dims]));
        ExprVocab::new(&schema, &dims)
    }

    #[test]
    fn the_vocab_reads_role_and_kind_from_the_schema_then_derived_labels() {
        let v = vocab();
        let names: Vec<&str> = v.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["book", "position_ref", "npv", "live", "expiry", "desk"]);
        assert_eq!(v.get("book").unwrap().kind, ValueKind::Categorical);
        assert_eq!(v.get("position_ref").unwrap().kind, ValueKind::Text, "keys have no dictionary");
        assert_eq!(v.get("npv").unwrap().detail(), "measure · number");
        assert_eq!(v.get("live").unwrap().kind, ValueKind::Bool);
        assert_eq!(v.get("expiry").unwrap().kind, ValueKind::Date);
        assert_eq!(
            v.get("desk").unwrap().kind,
            ValueKind::Derived(vec!["EQ".into(), "RATES".into()])
        );
        assert_eq!(v.get("desk").unwrap().detail(), "derived");
    }

    #[test]
    fn did_you_mean_proposes_only_within_two_edits() {
        let v = vocab();
        assert_eq!(v.nearest("bokk"), Some("book"));
        assert_eq!(v.nearest("BOOK"), Some("book"));
        assert_eq!(v.nearest("zzzzzz"), None);
    }

    #[test]
    fn check_flags_unknown_columns_and_derived_ordering() {
        let v = vocab();
        let w = check("bokk = 'A' and desk < 'EQ'", &v, None);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].span, 0..4);
        assert_eq!(w[0].message, "unknown column 'bokk'; did you mean 'book'?");
        assert!(w[1].message.starts_with("'desk' is a derived dimension, so '<'"), "{}", w[1].message);
        assert!(check("book = 'A' and desk in ('EQ')", &v, None).is_empty());
    }

    #[test]
    fn check_stays_quiet_about_the_word_under_the_caret() {
        let v = vocab();
        assert!(check("bok", &v, Some(3)).is_empty(), "still typing");
        assert_eq!(check("bok = 'A'", &v, Some(9)).len(), 1, "caret has left it");
    }

    #[test]
    fn an_empty_vocab_checks_nothing() {
        assert!(check("anything = 1", &ExprVocab::default(), None).is_empty());
    }
```

`LayerDoc::builtin` and `merge_docs` are the names the shell fixtures use,
imported via `geode_core::config`. If `merge_docs` takes a different
argument shape, adapt the helper, not the assertions.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core scope::complete`
Expected: a compile error, because `ExprVocab` is undefined.

- [ ] **Step 3: Implement**

In `expr.rs`, beside `CompareOp`:

```rust
/// The refusal for `op` (its grammar text) on derived dimension `column`,
/// or `None` when the operator is allowed. Derived dimensions are mapped
/// labels, so only `=`, `!=` and `in` mean anything on them. Both
/// `Scope::validate` and the expression dialogs' live check use this.
pub fn derived_op_error(column: &str, op: &str) -> Option<String> {
    (!matches!(op, "=" | "!=" | "<>")).then(|| {
        format!("'{column}' is a derived dimension, so '{op}' has no meaning on it; use =, != or in")
    })
}
```

In `scope/mod.rs`:
- Re-export it with `pub use expr::{..., derived_op_error};`.
- Replace the body of the closure inside `validate`'s `for_each_comparison`
  with:

```rust
            e.for_each_comparison(&mut |column, op| {
                if dims.get(column).is_some()
                    && let Some(message) = derived_op_error(column, op.grammar())
                {
                    diags.push(bad(message));
                }
            });
```

That message now names `like` rather than `ilike`. Run
`grep -rn "has no meaning" crates` and update any test that pinned `ilike`.

In `complete.rs`, above the tests:

```rust
use crate::dimensions::DerivedDimensions;
use crate::schema::{ColumnRole, ColumnType, SchemaSpec};

/// What a column can hold, as far as suggestions care.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueKind {
    /// Text with a dictionary: values are listed from the data.
    Categorical,
    /// Text without one (keys, free text): typed, not listed.
    Text,
    Number,
    Bool,
    Date,
    Timestamp,
    /// A derived dimension's labels, sorted.
    Derived(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VocabColumn {
    pub name: String,
    /// `dimension`, `key`, `measure`, `attribute`, `axis`, `value` or `derived`.
    pub role: &'static str,
    pub kind: ValueKind,
}

impl VocabColumn {
    /// The row detail: role and type, or `derived`.
    pub fn detail(&self) -> String {
        let ty = match self.kind {
            ValueKind::Derived(_) => return "derived".to_string(),
            ValueKind::Categorical | ValueKind::Text => "text",
            ValueKind::Number => "number",
            ValueKind::Bool => "bool",
            ValueKind::Date => "date",
            ValueKind::Timestamp => "timestamp",
        };
        format!("{} · {ty}", self.role)
    }
}

/// Every column an expression may name, across all datasets, then every
/// derived dimension. The first dataset to declare a column decides its
/// role and kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExprVocab {
    columns: Vec<VocabColumn>,
}

impl ExprVocab {
    pub fn new(schema: &SchemaSpec, dims: &DerivedDimensions) -> Self {
        let mut columns: Vec<VocabColumn> = Vec::new();
        for dataset in &schema.datasets {
            for c in &dataset.columns {
                if columns.iter().any(|v| v.name == c.name) {
                    continue;
                }
                let role = match c.role {
                    ColumnRole::Key => "key",
                    ColumnRole::Dimension { .. } => "dimension",
                    ColumnRole::Measure { .. } => "measure",
                    ColumnRole::Attribute { .. } => "attribute",
                    ColumnRole::Axis => "axis",
                    ColumnRole::Value => "value",
                };
                let kind = match c.ty {
                    ColumnType::Utf8 if c.categorical => ValueKind::Categorical,
                    ColumnType::Utf8 => ValueKind::Text,
                    ColumnType::F64 | ColumnType::I64 => ValueKind::Number,
                    ColumnType::Bool => ValueKind::Bool,
                    ColumnType::Date => ValueKind::Date,
                    ColumnType::Timestamp => ValueKind::Timestamp,
                };
                columns.push(VocabColumn { name: c.name.clone(), role, kind });
            }
        }
        for d in dims.all() {
            if columns.iter().any(|v| v.name == d.name) {
                continue;
            }
            let labels: std::collections::BTreeSet<&String> = d.values.values().collect();
            columns.push(VocabColumn {
                name: d.name.clone(),
                role: "derived",
                kind: ValueKind::Derived(labels.into_iter().cloned().collect()),
            });
        }
        Self { columns }
    }

    pub fn columns(&self) -> &[VocabColumn] {
        &self.columns
    }

    pub fn get(&self, name: &str) -> Option<&VocabColumn> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// The closest column name within two edits, ignoring case. On a tie,
    /// the earliest column wins.
    pub fn nearest(&self, name: &str) -> Option<&str> {
        let name = name.to_lowercase();
        self.columns
            .iter()
            .map(|c| (edit_distance(&name, &c.name.to_lowercase()), c.name.as_str()))
            .filter(|(d, _)| *d <= 2)
            .min_by_key(|(d, _)| *d)
            .map(|(_, n)| n)
    }
}

/// Levenshtein distance over chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// A schema problem in expression text, with the byte range it concerns.
#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    pub span: Range<usize>,
    pub message: String,
}

/// Schema problems in `text`, in text order: unknown columns (with a
/// did-you-mean suggestion) and ordering or `like` on a derived dimension.
/// With `caret`, a problem whose range touches the caret is left out,
/// because that word is still being typed. An empty vocab checks nothing.
/// The walk stops where the text stops being a valid prefix, so a syntax
/// error hides later warnings, and Enter reports the syntax error first.
pub fn check(text: &str, vocab: &ExprVocab, caret: Option<usize>) -> Vec<Warning> {
    if vocab.is_empty() {
        return Vec::new();
    }
    let tokens = lex(text);
    let mut out = Vec::new();
    walk(text, &tokens, &mut |term: Term| match vocab.get(&term.column) {
        None => out.push(Warning {
            span: term.column_span.clone(),
            message: match vocab.nearest(&term.column) {
                Some(near) => format!("unknown column '{}'; did you mean '{near}'?", term.column),
                None => format!("unknown column '{}'", term.column),
            },
        }),
        Some(VocabColumn { kind: ValueKind::Derived(_), .. }) => {
            if let Some((op, span)) = &term.op
                && let Some(message) = crate::scope::derived_op_error(&term.column, op)
            {
                out.push(Warning { span: span.clone(), message });
            }
        }
        Some(_) => {}
    });
    out.retain(|w| caret.is_none_or(|c| !(w.span.start <= c && c <= w.span.end)));
    out
}
```

`walk` reports a trailing column (`St::Column` at the end), which is how
`"bok"` produces a term at all. The caret filter then hides it while it is
being typed.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core scope`
Expected: all pass, including the existing `validate` tests.

- [ ] **Step 5: Add a mutation entry, then commit**

```zsh
# A derived dimension refuses ordering; accepting it would let `desk < 'EQ'`
# through to a query the compiler then rejects.
run_mutation "expr suggest: derived ordering is flagged" \
  crates/geode-core/src/scope/expr.rs \
  '    (!matches!(op, "=" | "!=" | "<>")).then(|| {' \
  '    (false).then(|| {' \
  geode-core \
  check_flags_unknown_columns_and_derived_ordering
```

Run `--anchors-only`, then `zsh scripts/mutation-check.sh "derived ordering"`.
Expected: `KILLED`.

```bash
git add crates/geode-core scripts/mutation-check.sh
git commit -m "feat(core): expression vocab and live schema warnings

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Pure suggestion state `ExprCompletion` (`geode-shell`)

**Files:**
- Create: `crates/geode-shell/src/exprcomplete.rs`
- Modify: `crates/geode-shell/src/lib.rs` (`pub mod exprcomplete;`,
  alphabetical, after `pub mod dialogmode;`)
- Modify: `crates/geode-shell/benches/shell_cores.rs` (add
  `bench_expr_complete` to the group)
- Test: unit tests in `exprcomplete.rs`

**Interfaces:**
- Consumes: `context_at`, `Context`, `Position`, `ExprVocab`, `ValueKind`
  and `check` from `geode_core::scope::complete`; `listfilter::rank`;
  `vimnav::{apply, NavCommand}`.
- Produces:
  - `pub const MAX_ROWS: usize = 50;`
  - `pub enum Values { Loading { tag: u64 }, Ready(Vec<(String, u64)>), Failed(String) }`
  - `pub struct Row { pub label: String, pub insert: String, pub detail: String, pub indices: Vec<usize> }`
  - `pub struct Write { pub range: Range<usize>, pub text: String }` with
    `pub fn apply(&self, line: &str) -> (String, usize)`
  - `pub enum Refresh { Unchanged, Changed, Request(String) }`
  - `pub struct ExprCompletion`, `Default`, with these methods:
    - `refresh(&mut self, text: &str, caret: usize, vocab: &ExprVocab) -> Refresh`
    - `mark_loading(&mut self, column: &str, tag: u64, vocab: &ExprVocab)`
    - `deliver(&mut self, column: &str, tag: u64, values: Result<Vec<(String, u64)>, String>, vocab: &ExprVocab) -> bool`
    - `rebuild(&mut self, vocab: &ExprVocab)`, which re-ranks against the
      last text and caret (used after a vocab reload)
    - `step(&mut self, delta: i64)`
    - `accept(&self, i: usize) -> Option<Write>`
    - `rows(&self) -> &[Row]`
    - `highlighted(&self) -> usize`
    - `hint(&self) -> &str`
    - `warning(&self) -> Option<&str>`
    - `no_matches(&self) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
//! (Filled in by Step 3.)

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::SchemaSpec;

    fn vocab() -> ExprVocab {
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
             [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
             [risk.columns.live]\ntype = \"bool\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs(&[datasets]));
        ExprVocab::new(&schema, &DerivedDimensions::default())
    }

    fn labels(c: &ExprCompletion) -> Vec<&str> {
        c.rows().iter().map(|r| r.label.as_str()).collect()
    }

    #[test]
    fn an_empty_field_lists_columns_then_not_and_paren() {
        let mut c = ExprCompletion::default();
        assert_eq!(c.refresh("", 0, &vocab()), Refresh::Changed);
        assert_eq!(labels(&c), ["book", "npv", "live", "not", "("]);
        assert_eq!(c.rows()[1].detail, "measure · number");
        assert_eq!(c.hint(), "column");
        assert_eq!(c.refresh("", 0, &vocab()), Refresh::Unchanged, "same text and caret");
    }

    #[test]
    fn operators_follow_the_column_type() {
        let mut c = ExprCompletion::default();
        c.refresh("npv ", 4, &vocab());
        assert_eq!(labels(&c), ["=", "!=", "<", "<=", ">", ">=", "in"]);
        c.refresh("live ", 5, &vocab());
        assert_eq!(labels(&c), ["=", "!="]);
        c.refresh("book ", 5, &vocab());
        assert_eq!(labels(&c), ["=", "!=", "in", "like"]);
        assert_eq!(c.hint(), "operator for book");
    }

    #[test]
    fn a_categorical_value_position_requests_once_and_lists_after_delivery() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        assert_eq!(c.refresh("book = ", 7, &v), Refresh::Request("book".into()));
        c.mark_loading("book", 7, &v);
        assert_eq!(c.hint(), "value for book · loading values…");
        assert_eq!(c.refresh("book = '", 8, &v), Refresh::Changed, "no second request");
        assert!(!c.deliver("book", 6, Ok(vec![("X".into(), 1)]), &v), "stale tag dropped");
        assert!(c.deliver("book", 7, Ok(vec![("EMEA".into(), 12), ("O'Neil".into(), 3)]), &v));
        assert_eq!(labels(&c), ["'EMEA'", "'O''Neil'"]);
        assert_eq!(c.rows()[0].detail, "12");
        assert_eq!(c.hint(), "value for book · 2 values");
    }

    #[test]
    fn a_failed_request_says_why() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = ", 7, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Err("pool busy".into()), &v);
        assert_eq!(c.hint(), "value for book · values unavailable: pool busy");
        assert!(c.rows().is_empty());
    }

    #[test]
    fn listed_values_are_left_out() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book in ('A', ", 14, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("A".into(), 1), ("B".into(), 2)]), &v);
        assert_eq!(labels(&c), ["'B'"]);
    }

    #[test]
    fn accept_writes_over_the_token_with_its_insert_text() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("bo", 2, &v);
        let w = c.accept(0).unwrap();
        assert_eq!(w.apply("bo"), ("book ".to_string(), 5));
        c.refresh("book i", 6, &v);
        assert_eq!(labels(&c), ["in"]);
        assert_eq!(c.accept(0).unwrap().apply("book i"), ("book in (".to_string(), 9));
    }

    #[test]
    fn a_value_with_a_quote_is_escaped_when_inserted() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = 'O", 9, &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("O'Neil".into(), 3)]), &v);
        assert_eq!(
            c.accept(0).unwrap().apply("book = 'O"),
            ("book = 'O''Neil'".to_string(), 16)
        );
    }

    #[test]
    fn a_multibyte_value_writes_on_char_boundaries() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        let text = "book = 'Zü";
        c.refresh(text, text.len(), &v);
        c.mark_loading("book", 1, &v);
        c.deliver("book", 1, Ok(vec![("Zürich".into(), 1)]), &v);
        assert_eq!(c.accept(0).unwrap().apply(text).0, "book = 'Zürich'");
    }

    #[test]
    fn connectives_after_a_term_and_close_paren_only_when_open() {
        let mut c = ExprCompletion::default();
        c.refresh("npv > 1 ", 8, &vocab());
        assert_eq!(labels(&c), ["and", "or"]);
        c.refresh("(npv > 1 ", 9, &vocab());
        assert_eq!(labels(&c), ["and", "or", ")"]);
        assert_eq!(c.hint(), "and / or, or enter to apply");
    }

    #[test]
    fn typed_text_ranks_and_no_matches_is_reported() {
        let mut c = ExprCompletion::default();
        c.refresh("np", 2, &vocab());
        assert_eq!(labels(&c)[0], "npv");
        c.refresh("zz", 2, &vocab());
        assert!(c.rows().is_empty());
        assert!(c.no_matches());
    }

    #[test]
    fn step_moves_and_clamps_the_highlight() {
        let mut c = ExprCompletion::default();
        c.refresh("", 0, &vocab());
        c.step(1);
        assert_eq!(c.highlighted(), 1);
        c.step(-5);
        assert_eq!(c.highlighted(), 0);
    }

    #[test]
    fn the_warning_is_the_first_schema_problem_off_the_caret() {
        let mut c = ExprCompletion::default();
        c.refresh("bokk = 'A' ", 11, &vocab());
        assert_eq!(c.warning(), Some("unknown column 'bokk'; did you mean 'book'?"));
    }

    #[test]
    fn rows_are_capped() {
        let v = vocab();
        let mut c = ExprCompletion::default();
        c.refresh("book = ", 7, &v);
        c.mark_loading("book", 1, &v);
        let many: Vec<(String, u64)> = (0..500).map(|i| (format!("V{i:03}"), 1)).collect();
        c.deliver("book", 1, Ok(many), &v);
        assert_eq!(c.rows().len(), MAX_ROWS);
        assert_eq!(c.hint(), "value for book · 500 values");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --lib exprcomplete`
Expected: a compile error.

- [ ] **Step 3: Implement**

Replace the stub doc and put the following above the tests:

```rust
//! Suggestion state for a scope expression field. It is pure: the gpui
//! controller in `shell::expr_suggest` feeds it the field's text and
//! caret and paints what it holds.
//!
//! `refresh` re-reads the caret position (`geode_core::scope::complete::
//! context_at`), then rebuilds the rows, hint and warning. Rows are ranked
//! with the shared fuzzy matcher and capped at [`MAX_ROWS`]. Categorical
//! values come from an async distinct query: `refresh` asks for a column
//! once (`Refresh::Request`), and `deliver` accepts only the latest tag
//! for that column, so a reply from a superseded request is never shown.

use std::collections::HashMap;
use std::ops::Range;

use geode_core::scope::complete::{Context, ExprVocab, Position, ValueKind, check, context_at};

use crate::listfilter;
use crate::vimnav::{self, NavCommand};

/// At most this many ranked rows are kept. The hint still names the total.
pub const MAX_ROWS: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Values {
    Loading { tag: u64 },
    Ready(Vec<(String, u64)>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// What the row shows and what ranking matches against.
    pub label: String,
    /// What accepting the row writes over the token.
    pub insert: String,
    /// Right-aligned detail: a role and type, a count, or what an operator does.
    pub detail: String,
    /// Matched char offsets within `label`.
    pub indices: Vec<usize>,
}

/// One accepted row: `text` over the byte `range` of the line it was
/// computed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub range: Range<usize>,
    pub text: String,
}

impl Write {
    /// The line with the write applied, and the caret after it.
    pub fn apply(&self, line: &str) -> (String, usize) {
        crate::commandline::accept(line, self.range.clone(), &self.text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Same text and caret as last time: nothing to repaint.
    Unchanged,
    Changed,
    /// Changed, and `column`'s values should be requested now.
    Request(String),
}

#[derive(Debug, Default)]
pub struct ExprCompletion {
    last: Option<(String, usize)>,
    context: Option<Context>,
    rows: Vec<Row>,
    candidates: usize,
    highlighted: usize,
    hint: String,
    warning: Option<String>,
    values: HashMap<String, Values>,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn op_detail(op: &str) -> &'static str {
    match op {
        "=" => "equals",
        "!=" => "differs from",
        "<" => "less than",
        "<=" => "at most",
        ">" => "greater than",
        ">=" => "at least",
        "in" => "one of a list",
        "like" => "contains text",
        _ => "",
    }
}

fn operators(kind: Option<&ValueKind>) -> &'static [&'static str] {
    match kind {
        Some(ValueKind::Categorical | ValueKind::Text) => &["=", "!=", "in", "like"],
        Some(ValueKind::Number | ValueKind::Date | ValueKind::Timestamp) => {
            &["=", "!=", "<", "<=", ">", ">=", "in"]
        }
        Some(ValueKind::Bool) => &["=", "!="],
        Some(ValueKind::Derived(_)) => &["=", "!=", "in"],
        None => &["=", "!=", "<", "<=", ">", ">=", "in", "like"],
    }
}

impl ExprCompletion {
    pub fn refresh(&mut self, text: &str, caret: usize, vocab: &ExprVocab) -> Refresh {
        if self.last.as_ref().is_some_and(|(t, c)| t == text && *c == caret) {
            return Refresh::Unchanged;
        }
        self.last = Some((text.to_string(), caret));
        self.highlighted = 0;
        self.rebuild(vocab);
        match &self.context {
            Some(Context { position: Position::Value { column, .. }, .. })
                if matches!(vocab.get(column).map(|c| &c.kind), Some(ValueKind::Categorical))
                    && !self.values.contains_key(column) =>
            {
                Refresh::Request(column.clone())
            }
            _ => Refresh::Changed,
        }
    }

    pub fn mark_loading(&mut self, column: &str, tag: u64, vocab: &ExprVocab) {
        self.values.insert(column.to_string(), Values::Loading { tag });
        self.rebuild(vocab);
    }

    pub fn deliver(
        &mut self,
        column: &str,
        tag: u64,
        values: Result<Vec<(String, u64)>, String>,
        vocab: &ExprVocab,
    ) -> bool {
        if self.values.get(column) != Some(&Values::Loading { tag }) {
            return false;
        }
        let state = match values {
            Ok(v) => Values::Ready(v),
            Err(e) => Values::Failed(e),
        };
        self.values.insert(column.to_string(), state);
        self.rebuild(vocab);
        true
    }

    /// Rebuild the context, rows, hint and warning from the last text and
    /// caret. The highlight is kept, clamped to the new rows.
    pub fn rebuild(&mut self, vocab: &ExprVocab) {
        let Some((text, caret)) = self.last.clone() else {
            return;
        };
        let context = context_at(&text, caret);
        let candidates = self.candidates(&context, vocab);
        let texts: Vec<String> = candidates.iter().map(|r| r.label.clone()).collect();
        self.candidates = candidates.len();
        self.rows = listfilter::rank(&texts, &context.typed)
            .into_iter()
            .take(MAX_ROWS)
            .map(|r| Row { indices: r.indices, ..candidates[r.row].clone() })
            .collect();
        self.highlighted = self.highlighted.min(self.rows.len().saturating_sub(1));
        self.hint = self.hint_for(&context, vocab);
        self.warning = check(&text, vocab, Some(caret)).into_iter().next().map(|w| w.message);
        self.context = Some(context);
    }

    fn candidates(&self, context: &Context, vocab: &ExprVocab) -> Vec<Row> {
        let row = |label: &str, insert: String, detail: &str| Row {
            label: label.to_string(),
            insert,
            detail: detail.to_string(),
            indices: Vec::new(),
        };
        match &context.position {
            Position::Column => {
                let mut rows: Vec<Row> = vocab
                    .columns()
                    .iter()
                    .map(|c| row(&c.name, format!("{} ", c.name), &c.detail()))
                    .collect();
                rows.push(row("not", "not ".into(), "negate what follows"));
                rows.push(row("(", "(".into(), "start a group"));
                rows
            }
            Position::Operator { column } => operators(vocab.get(column).map(|c| &c.kind))
                .iter()
                .map(|op| {
                    let insert = if *op == "in" { "in (".to_string() } else { format!("{op} ") };
                    row(op, insert, op_detail(op))
                })
                .collect(),
            Position::OpenList { .. } => vec![row("(", "(".into(), "start the list")],
            Position::Value { column, listed } => {
                let unlisted = |v: &&str| !listed.iter().any(|l| l == v);
                match vocab.get(column).map(|c| &c.kind) {
                    Some(ValueKind::Categorical) => match self.values.get(column) {
                        Some(Values::Ready(values)) => values
                            .iter()
                            .filter(|(v, _)| unlisted(&v.as_str()))
                            .map(|(v, n)| row(&quote(v), quote(v), &n.to_string()))
                            .collect(),
                        _ => Vec::new(),
                    },
                    Some(ValueKind::Derived(labels)) => labels
                        .iter()
                        .map(String::as_str)
                        .filter(unlisted)
                        .map(|l| row(&quote(l), quote(l), "label"))
                        .collect(),
                    Some(ValueKind::Bool) => ["true", "false"]
                        .into_iter()
                        .filter(unlisted)
                        .map(|b| row(b, b.to_string(), ""))
                        .collect(),
                    _ => Vec::new(),
                }
            }
            Position::Connective { in_list: true, .. } => vec![
                row(",", ", ".into(), "another value"),
                row(")", ") ".into(), "end the list"),
            ],
            Position::Connective { open_parens, in_list: false } => {
                let mut rows = vec![
                    row("and", "and ".into(), "both must hold"),
                    row("or", "or ".into(), "either may hold"),
                ];
                if *open_parens > 0 {
                    rows.push(row(")", ") ".into(), "close the group"));
                }
                rows
            }
            Position::Invalid => Vec::new(),
        }
    }

    fn hint_for(&self, context: &Context, vocab: &ExprVocab) -> String {
        match &context.position {
            Position::Column => "column".into(),
            Position::Operator { column } => format!("operator for {column}"),
            Position::OpenList { column } => format!("( starts the list for {column}"),
            Position::Value { column, .. } => {
                let tail = match vocab.get(column).map(|c| &c.kind) {
                    None => return format!("value for {column}"),
                    Some(ValueKind::Categorical) => match self.values.get(column) {
                        Some(Values::Loading { .. }) | None => "loading values…".to_string(),
                        Some(Values::Ready(v)) => format!("{} values", v.len()),
                        Some(Values::Failed(e)) => format!("values unavailable: {e}"),
                    },
                    Some(ValueKind::Text) => "text in quotes, e.g. 'ABC'".into(),
                    Some(ValueKind::Number) => "a number, e.g. 1000".into(),
                    Some(ValueKind::Bool) => "true or false".into(),
                    Some(ValueKind::Date | ValueKind::Timestamp) => {
                        "a date in quotes, e.g. '2026-09-26'".into()
                    }
                    Some(ValueKind::Derived(l)) => format!("{} labels", l.len()),
                };
                format!("value for {column} · {tail}")
            }
            Position::Connective { in_list: true, .. } => ", adds a value · ) ends the list".into(),
            Position::Connective { .. } => "and / or, or enter to apply".into(),
            Position::Invalid => "this can't continue; enter says what is wrong".into(),
        }
    }

    pub fn step(&mut self, delta: i64) {
        self.highlighted = vimnav::apply(self.highlighted, self.rows.len(), NavCommand::Move(delta));
    }

    /// The write for ranked row `i`, or `None` when there is no such row
    /// or the cached range no longer fits the last text.
    pub fn accept(&self, i: usize) -> Option<Write> {
        let row = self.rows.get(i)?;
        let range = self.context.as_ref()?.token.clone();
        let (text, _) = self.last.as_ref()?;
        let fits = range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end);
        fits.then(|| Write { range, text: row.insert.clone() })
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    pub fn hint(&self) -> &str {
        &self.hint
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// There were candidates, and the typed text matched none of them.
    pub fn no_matches(&self) -> bool {
        self.rows.is_empty() && self.candidates > 0
    }
}
```

`vimnav::apply(selected, len, NavCommand::Move(i64))` wraps on ±1 and
clamps larger moves, so shift+tab on the first row wraps to the last.
`commandline::accept` returns `(line, cursor after the insert)`.

`Row { indices, ..candidates[r.row].clone() }` clones each ranked row once
per refresh. That cost is bounded by the candidate count and happens per
keystroke, not per frame.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --lib exprcomplete`
Expected: all 13 pass.

- [ ] **Step 5: Add the bench**

In `benches/shell_cores.rs`, add the function below and add
`bench_expr_complete` to the `criterion_group!` list:

```rust
/// One keystroke's refresh with 20,000 categorical values loaded: lex,
/// context, rank and cap. A categorical column's dictionary bounds its
/// size, and 20,000 is well past the demo's largest.
fn bench_expr_complete(c: &mut Criterion) {
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::schema::SchemaSpec;
    use geode_core::scope::complete::ExprVocab;
    use geode_shell::exprcomplete::ExprCompletion;

    let mut group = c.benchmark_group("expr_complete");
    group.sample_size(30);
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let (schema, _) = SchemaSpec::from_doc(&merge_docs(&[datasets]));
    let vocab = ExprVocab::new(&schema, &DerivedDimensions::default());
    let values: Vec<(String, u64)> = (0..20_000).map(|i| (format!("BK{i:05}"), 1)).collect();
    group.bench_function("refresh_20k_values", |b| {
        let mut done = ExprCompletion::default();
        done.refresh("book = '", 8, &vocab);
        done.mark_loading("book", 1, &vocab);
        done.deliver("book", 1, Ok(values.clone()), &vocab);
        let mut flip = false;
        b.iter(|| {
            // Alternate the typed text so `refresh` never short-circuits.
            flip = !flip;
            let text = if flip { "book = 'BK1" } else { "book = 'BK12" };
            black_box(done.refresh(black_box(text), text.len(), &vocab))
        })
    });
    group.finish();
}
```

Run: `cargo check -p geode-shell --benches`, then
`cargo bench -p geode-shell --bench shell_cores -- expr_complete`. Note the
median; Task 6 records it. If it exceeds 8 ms, stop and report. Do not add
a data-layer limit in this plan.

- [ ] **Step 6: Add a mutation entry, then commit**

```zsh
# A reply from a superseded request must never fill the list.
run_mutation "expr suggest: a stale values reply is dropped" \
  crates/geode-shell/src/exprcomplete.rs \
  '        if self.values.get(column) != Some(&Values::Loading { tag }) {' \
  '        if self.values.get(column).is_none() {' \
  geode-shell \
  a_categorical_value_position_requests_once_and_lists_after_delivery

# Quotes inside a value are doubled; a bare quote would end the string early.
run_mutation "expr suggest: an inserted value escapes its quotes" \
  crates/geode-shell/src/exprcomplete.rs \
  "    format!(\"'{}'\", value.replace('\\'', \"''\"))" \
  "    format!(\"'{}'\", value)" \
  geode-shell \
  a_value_with_a_quote_is_escaped_when_inserted

# Operators follow the column type: a bool column offers no ordering.
run_mutation "expr suggest: operators follow the column type" \
  crates/geode-shell/src/exprcomplete.rs \
  '        Some(ValueKind::Bool) => &["=", "!="],' \
  '        Some(ValueKind::Bool) => &["=", "!=", "<"],' \
  geode-shell \
  operators_follow_the_column_type
```

The quote-escape anchor mixes quote kinds. If `--anchors-only` reports it
missing, copy the exact source line into a zsh `$'...'` string instead.
Run `--anchors-only`, then `zsh scripts/mutation-check.sh "expr suggest"`.
Expected: all entries `KILLED`.

```bash
git add crates/geode-shell/src/exprcomplete.rs crates/geode-shell/src/lib.rs crates/geode-shell/benches/shell_cores.rs scripts/mutation-check.sh
git commit -m "feat(shell): pure suggestion state for scope expressions

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Frame expression dialog: controller, render, keys, values, Enter refusal

**Files:**
- Create: `crates/geode-shell/src/shell/expr_suggest.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs`
- Modify: `crates/geode-shell/src/shell/hot_reload.rs`
- Modify: `crates/geode-shell/src/shell/scope_expr_view.rs`
- Test: `crates/geode-shell/src/shell/tests/scope_expr.rs`, plus unit tests
  in `scope_expr_view.rs`

**Interfaces:**
- Consumes: `ExprCompletion`, `Refresh`, `Write` and `MAX_ROWS` (Task 3);
  `ExprVocab` and `check` (Task 2).
- Produces:
  - `pub const EXPR_KEY: QueryKey = QueryKey(u64::MAX - 4);` in
    `shell/mod.rs`
  - `pub fn expr_vocab(config: &Config) -> ExprVocab` in `shell/mod.rs`,
    beside `pickable_columns`
  - these `ShellView` fields: `expr_vocab: std::rc::Rc<ExprVocab>` and
    `expr_scroll: ScrollHandle`
  - `pub struct ScopeExprState { pub mode, pub error, pub completion: ExprCompletion }`
  - `pub fn request_scope(mode: &Mode, current: &Scope) -> Scope` in
    `scope_expr_view.rs`
  - `pub fn commit_text(text: &str, vocab: &ExprVocab) -> Result<Option<Expr>, String>`
  - `pub fn apply(frame: &mut Frame, mode: &Mode, text: &str, vocab: &ExprVocab) -> Result<bool, String>`
  - in `expr_suggest.rs`:
    - `pub(crate) fn refresh(view: &mut ShellView, cx: &mut Context<ShellView>)`
    - `pub(crate) fn handle_key(view: &mut ShellView, ks: &Keystroke, window: &mut Window, cx: &mut Context<ShellView>) -> bool`
    - `pub(crate) fn accept(view: &mut ShellView, i: usize, window: &mut Window, cx: &mut Context<ShellView>)`
    - `pub(crate) fn deliver(view: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>)`
    - `pub(crate) fn render(c: &ExprCompletion, scroll: &ScrollHandle, theme: &Theme, on_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static) -> AnyElement`
    - `pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion>`,
      the surface lookup that Task 5 extends

- [ ] **Step 1: Write the failing tests**

In `scope_expr_view.rs`'s `#[cfg(test)]` module:
- Update every existing `apply(...)` and `commit_text(...)` call to pass
  `&ExprVocab::default()`.
- Add these tests:

```rust
    #[test]
    fn request_scope_narrows_by_what_the_new_text_is_anded_with() {
        use geode_core::scope::{DimensionSelection, Scope};
        let current = Scope {
            dimensions: vec![DimensionSelection { column: "book".into(), values: vec!["A".into()] }],
            expression: Some(parse_expr("x = 'a' and y = 'b'").unwrap()),
            ..Scope::default()
        };
        assert_eq!(request_scope(&Mode::Whole, &current).expression, None);
        assert_eq!(request_scope(&Mode::Whole, &current).dimensions, current.dimensions);
        assert_eq!(request_scope(&Mode::Add, &current).expression, current.expression);
        let term = Mode::term(0, current.expression.as_ref()).unwrap();
        assert_eq!(
            request_scope(&term, &current).expression.map(|e| e.to_string()),
            Some("y = 'b'".to_string())
        );
    }

    #[test]
    fn commit_refuses_an_unknown_column_when_a_schema_exists() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::schema::SchemaSpec;
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs(&[datasets]));
        let vocab = ExprVocab::new(&schema, &DerivedDimensions::default());
        assert_eq!(
            commit_text("bokk = 'A'", &vocab),
            Err("unknown column 'bokk'; did you mean 'book'?".to_string())
        );
        assert!(commit_text("book = 'A'", &vocab).is_ok());
        assert!(commit_text("bokk = 'A'", &ExprVocab::default()).is_ok(), "no schema, no check");
    }
```

In `shell/tests/scope_expr.rs`, add a fixture and GPUI tests:

```rust
use crate::shell::EXPR_KEY;
use geode_core::query::{DistinctOutcome, DistinctParams};

/// `test_services` with a schema: `book` (categorical), `npv` (measure),
/// `live` (bool), plus the derived `desk`. The keymap doc is re-added
/// because the config is rebuilt from these layers alone.
fn services_with_schema() -> ShellServices {
    let mut services = test_services();
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.position_ref]\ntype = \"utf8\"\nrole = \"key\"\n\
         [risk.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"position\"\n\
         [risk.columns.live]\ntype = \"bool\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let dims = LayerDoc::builtin(
        "dimensions",
        "[desk]\nfrom = \"book\"\n[desk.values]\nEQ = [\"BK000\"]\n",
    )
    .unwrap();
    (services.config, services.builtin) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), datasets, dims],
        desk: None,
        user: None,
    });
    services
}

fn requests(
    shell: &Entity<ShellView>,
    vcx: &mut gpui::VisualTestContext,
) -> std::rc::Rc<std::cell::RefCell<Vec<DistinctParams>>> {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    vcx.update(|_, cx| {
        let seen = seen.clone();
        cx.subscribe(shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                seen.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    seen
}

fn field(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> String {
    shell.read_with(vcx, |s, cx| s.dialog_input.read(cx).value().to_string())
}

/// The empty field lists columns; tab inserts the highlighted one and the
/// list moves on to its operators.
#[gpui::test]
fn tab_inserts_a_column_and_the_list_moves_to_operators(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-book").is_some(), "columns listed");
    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    assert!(vcx.debug_bounds("scope-expr-row->=").is_some(), "number operators");
    assert!(vcx.debug_bounds("scope-expr-row-like").is_none());
    assert!(dialog_filter_is_focused(&shell, &mut vcx), "tab never leaves the field");
}

/// down moves the highlight; tab then inserts that row.
#[gpui::test]
fn arrows_move_the_highlight_that_tab_inserts(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("npv ");
    vcx.simulate_keystrokes("down down tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv < ");
    vcx.simulate_keystrokes("shift-tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv < ", "shift-tab moves, never writes");
}

/// A value position requests the column's values once, scoped as the
/// mode says. A delivery through `deliver_distinct` fills the rows, and a
/// stale tag is ignored.
#[gpui::test]
fn values_arrive_through_the_distinct_path_and_stale_replies_are_dropped(
    cx: &mut gpui::TestAppContext,
) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    let seen = requests(&shell, &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    vcx.simulate_input("'");
    vcx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    assert_eq!(seen.borrow().len(), 1, "asked once");
    assert_eq!((req.key, req.column.as_str()), (EXPR_KEY, "book"));
    let deliver = |shell: &Entity<ShellView>, vcx: &mut gpui::VisualTestContext, tag| {
        shell.update(vcx, |s, cx| {
            s.deliver_distinct(
                DistinctOutcome {
                    key: EXPR_KEY,
                    tag,
                    column: "book".into(),
                    values: Ok(vec![("EMEA".into(), 12)]),
                },
                cx,
            )
        });
        vcx.run_until_parked();
    };
    deliver(&shell, &mut vcx, req.tag.wrapping_sub(1));
    assert!(vcx.debug_bounds("scope-expr-row-'EMEA'").is_none(), "stale reply dropped");
    deliver(&shell, &mut vcx, req.tag);
    assert!(vcx.debug_bounds("scope-expr-row-'EMEA'").is_some());
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "book = 'EMEA'");
}

/// A reply that arrives after the dialog closed does nothing.
#[gpui::test]
fn a_reply_after_close_is_ignored(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    let seen = requests(&shell, &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let tag = seen.borrow().last().unwrap().tag;
    vcx.simulate_keystrokes("escape");
    shell.update(&mut vcx, |s, cx| {
        s.deliver_distinct(
            DistinctOutcome { key: EXPR_KEY, tag, column: "book".into(), values: Ok(vec![]) },
            cx,
        )
    });
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

/// Add mode narrows by the frame's current expression.
#[gpui::test]
fn add_mode_requests_values_under_the_current_expression(cx: &mut gpui::TestAppContext) {
    let (window, mut vcx) = open_shell(cx, services_with_schema());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    frame.update(&mut vcx, |f, cx| {
        f.set_scope(expr_scope("live = true"));
        cx.notify();
    });
    let seen = requests(&shell, &mut vcx);
    dispatch_action(&shell, "frame::add_expression", &mut vcx);
    vcx.simulate_input("book = ");
    vcx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    assert_eq!(req.scope.expression.map(|e| e.to_string()), Some("live = true".to_string()));
}

/// A caret moved by a key, with no typing, re-reads the position. Tab
/// then writes at the new caret, not at the stale range.
#[gpui::test]
fn a_moved_caret_is_followed_before_tab_writes(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("npv > 1 and live ");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-=").is_some(), "bool operators at the end");
    assert!(vcx.debug_bounds("scope-expr-row-book").is_none());
    vcx.simulate_keystrokes("home");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-book").is_some(), "the column list at the start");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "book npv > 1 and live ", "written at the new caret");
}

/// cmd-z takes an insertion back, which proves the write kept undo.
#[gpui::test]
fn undo_takes_an_insertion_back(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("np");
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    vcx.simulate_keystrokes("cmd-z");
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "np");
}

/// A row click inserts it and the field keeps the keyboard.
#[gpui::test]
fn clicking_a_row_inserts_it_and_typing_continues(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    let bounds = vcx.debug_bounds("scope-expr-row-npv").expect("row painted");
    vcx.simulate_click(bounds.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(field(&shell, &vcx), "npv ");
    vcx.simulate_input(">");
    assert_eq!(field(&shell, &vcx), "npv >");
}

/// Enter refuses an unknown column inline, and the text stays.
#[gpui::test]
fn enter_refuses_an_unknown_column(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.simulate_input("bokk = 'A'");
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
    assert_eq!(
        shell.read_with(&vcx, |s, _| s.scope_expr_dialog.as_ref().and_then(|d| d.error.clone())),
        Some("unknown column 'bokk'; did you mean 'book'?".to_string())
    );
    assert!(vcx.debug_bounds("scope-expr-warning").is_some(), "live warning painted too");
}

/// A hot reload that adds a column while the dialog is open is suggested
/// straight away.
#[gpui::test]
fn a_reload_rebuilds_the_vocab_under_an_open_dialog(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) =
        dialog_test_shell_with(cx, services_with_schema(), "frame::scope_expression");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-region").is_none());
    let datasets = LayerDoc::builtin(
        "datasets",
        "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n\
         [risk.columns.region]\ntype = \"utf8\"\nrole = \"dimension\"\n",
    )
    .unwrap();
    let (config, _) = ShellServices::config_and_builtin(ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(), datasets],
        desk: None,
        user: None,
    });
    shell.update(&mut vcx, |s, cx| s.apply_reload(config, cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("scope-expr-row-region").is_some());
}
```

`apply_reload(&mut self, new_config: Config, cx)` is `pub(super)` in
`shell/hot_reload.rs`. The call above is the one `shell/tests/reload.rs`
uses.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --lib scope_expr`
Expected: a compile error (`EXPR_KEY`, `request_scope` and the new
signatures are undefined).

- [ ] **Step 3: Implement the shell plumbing in `shell/mod.rs`**

1. **Declare the module.** Add `pub(crate) mod expr_suggest;` beside
   `pub mod scope_expr_view;`.
2. **Add the key constant** after `SCOPES_KEY`:

```rust
/// The scope expression suggestions' distinct-values requests (both the
/// frame dialog and the Scopes dialog's `expression` field). Routed by
/// [`ShellView::deliver_distinct`] to `expr_suggest::deliver`, which drops
/// any reply whose tag is not its column's latest.
pub const EXPR_KEY: QueryKey = QueryKey(u64::MAX - 4);
```

3. **Add the vocab builder** beside `pickable_columns`:

```rust
/// Every column a scope expression may name, for the expression
/// suggestions and their schema check. Cached on the shell and rebuilt
/// when datasets or dimensions reload.
pub fn expr_vocab(config: &Config) -> geode_core::scope::complete::ExprVocab {
    let (schema, _) = config.doc("datasets").map(SchemaSpec::from_doc).unwrap_or_default();
    let (dims, _) = config
        .doc("dimensions")
        .map(DerivedDimensions::from_doc)
        .unwrap_or_default();
    geode_core::scope::complete::ExprVocab::new(&schema, &dims)
}
```

4. **Add the fields.** Beside `pickable`, add
   `expr_vocab: std::rc::Rc<geode_core::scope::complete::ExprVocab>`, with a
   doc line saying it is cached `expr_vocab`. Beside `choice_dialog_scroll`,
   add `expr_scroll: ScrollHandle`. Initialise them in the constructor:
   `expr_vocab: std::rc::Rc::new(expr_vocab(&services.config))` (use the
   same config variable the constructor passes to `pickable_columns`) and
   `expr_scroll: ScrollHandle::new()`.

5. **Hook the input.** Right after the `dialog_input` `subscribe_in(...)
   .detach();`, add:

```rust
        // Expression suggestions follow the caret as well as the text. A
        // caret moved by an arrow or a click emits no `Change`, but the
        // input notifies, so observe it. `expr_suggest::refresh` skips
        // unchanged text and caret, so the cursor blink's notify costs one
        // comparison.
        cx.observe(&dialog_input, |view, _input, cx| expr_suggest::refresh(view, cx))
            .detach();
```

6. **Route the delivery.** In `deliver_distinct`, before the `SCOPES_KEY`
   arm:

```rust
        if outcome.key == EXPR_KEY {
            expr_suggest::deliver(self, outcome, cx);
            return;
        }
```

7. **Rebuild on reload.** In `hot_reload.rs`, inside `if pickable_changed {`,
   add:

```rust
                self.expr_vocab = std::rc::Rc::new(super::expr_vocab(&self.services.config));
                let vocab = self.expr_vocab.clone();
                if let Some(c) = super::expr_suggest::completion_mut(self) {
                    c.rebuild(&vocab);
                }
```

The observe closure's exact signature for `Context<ShellView>::observe` is
`FnMut(&mut ShellView, Entity<InputState>, &mut Context<ShellView>)`. If the
pinned gpui wants a window, use `cx.observe_in(&dialog_input, window, |view,
_input, _window, cx| ...)`.

- [ ] **Step 4: Implement `shell/expr_suggest.rs`**

```rust
//! Suggestions for the scope expression fields: the frame's expression
//! dialog and the Scopes dialog's open `expression` field. Both edit the
//! shared `dialog_input`. The pure state is `crate::exprcomplete`; this
//! module feeds it the live text and caret, claims its keys, requests
//! categorical values under [`super::EXPR_KEY`] and paints it.
//!
//! An accepted row is written as one range replace (select, then
//! replace). Unlike `set_value`, that stays in the input's undo history.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, MouseButton, ScrollHandle, SharedString, Window, div};
use gpui_component::{Theme, h_flex, v_flex};

use geode_core::query::{DistinctOutcome, DistinctParams};
use geode_core::scope::Scope;

use crate::exprcomplete::{ExprCompletion, MAX_ROWS, Refresh};
use crate::keymap::Keystroke;
use crate::listfilter;
use crate::vimnav::NavCommand;

use super::{EXPR_KEY, ShellEvent, ShellView, chip, scale, scope_expr_view};

/// The open expression field's completion, whichever surface holds it.
pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion> {
    if let Some(state) = view.scope_expr_dialog.as_mut() {
        return Some(&mut state.completion);
    }
    None
}

/// The scope the finished expression will be ANDed with, which is what
/// the values request is narrowed by.
fn values_scope(view: &ShellView, cx: &App) -> Option<Scope> {
    let current = view.frame.read(cx).scope();
    if let Some(state) = view.scope_expr_dialog.as_ref() {
        return Some(scope_expr_view::request_scope(&state.mode, current));
    }
    None
}

/// Re-read the field's text and caret. Runs from the input observer, on
/// open, after an accept and before a claimed key. It never runs in
/// render.
pub(crate) fn refresh(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let (text, caret) = {
        let input = view.dialog_input.read(cx);
        (input.value().to_string(), input.cursor())
    };
    let vocab = view.expr_vocab.clone();
    let Some(c) = completion_mut(view) else {
        return;
    };
    match c.refresh(&text, caret, &vocab) {
        Refresh::Unchanged => return,
        Refresh::Changed => {}
        Refresh::Request(column) => request_values(view, column, cx),
    }
    view.expr_scroll.scroll_to_item(0);
    cx.notify();
}

fn request_values(view: &mut ShellView, column: String, cx: &mut Context<ShellView>) {
    let Some(scope) = values_scope(view, cx) else {
        return;
    };
    let as_of = view.frame.read(cx).as_of().clone();
    view.next_picker_tag += 1;
    let tag = view.next_picker_tag;
    let vocab = view.expr_vocab.clone();
    if let Some(c) = completion_mut(view) {
        c.mark_loading(&column, tag, &vocab);
    }
    cx.emit(ShellEvent::DistinctRequested(DistinctParams {
        key: EXPR_KEY,
        tag,
        column,
        scope,
        as_of,
    }));
}

/// An `EXPR_KEY` reply. It is dropped when no expression field is open
/// or the tag is not the column's latest.
pub(crate) fn deliver(view: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let Some(c) = completion_mut(view) else {
        return;
    };
    if c.deliver(&outcome.column, outcome.tag, outcome.values, &vocab) {
        cx.notify();
    }
}

/// The suggestion keys, ahead of the field. A bare `tab` inserts the
/// highlighted row; `shift-tab`, `up`/`down` and `ctrl-p`/`ctrl-n` move
/// the highlight. Both tabs are claimed even with nothing to insert, so
/// focus never leaves the field. Every other key is the field's.
pub(crate) fn handle_key(
    view: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if completion_mut(view).is_none() {
        return false;
    }
    let delta = if ks.key == "tab" && !ks.mods.is_chord() {
        if !ks.mods.shift {
            // A caret moved since the last observe must be read before
            // the write, or the write would land on a stale range.
            refresh(view, cx);
            let i = completion_mut(view).map_or(0, |c| c.highlighted());
            accept(view, i, window, cx);
            return true;
        }
        -1
    } else {
        match listfilter::nav_command(ks) {
            Some(NavCommand::Move(d)) => d,
            _ => return false,
        }
    };
    if let Some(c) = completion_mut(view) {
        c.step(delta);
        let h = c.highlighted();
        view.expr_scroll.scroll_to_item(h);
    }
    cx.notify();
    true
}

/// Write ranked row `i` over its token, keep the keyboard in the field,
/// and re-read the new position.
pub(crate) fn accept(view: &mut ShellView, i: usize, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(write) = completion_mut(view).and_then(|c| c.accept(i)) else {
        return;
    };
    view.dialog_input.update(cx, |s, cx| {
        s.set_selected_range(write.range.clone(), cx);
        s.replace(write.text.clone(), window, cx);
        s.focus(window, cx);
    });
    refresh(view, cx);
}

/// Row height at the design rem; the viewport shows at most this many rows.
const ROW_HEIGHT: f32 = 26.0;
const VISIBLE_ROWS: usize = 8;

/// The hint line, the ranked rows (or "no matches") and the warning line.
/// Selectors: `scope-expr-hint`, `scope-expr-row-{label}`,
/// `scope-expr-no-matches` and `scope-expr-warning`.
pub(crate) fn render(
    c: &ExprCompletion,
    scroll: &ScrollHandle,
    theme: &Theme,
    on_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let paint = super::listrow::row_paint(theme);
    let mut column = v_flex().gap_1().w_full().child(
        div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .debug_selector(|| "scope-expr-hint".to_string())
            .child(SharedString::from(c.hint().to_string())),
    );
    if !c.rows().is_empty() {
        let shown = c.rows().len().min(VISIBLE_ROWS);
        let mut rows = v_flex()
            .id("scope-expr-rows")
            .w_full()
            .h(scale::design(shown as f32 * ROW_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(scroll);
        for (position, row) in c.rows().iter().enumerate().take(MAX_ROWS) {
            let selector = format!("scope-expr-row-{}", row.label);
            let on_click = on_click.clone();
            let element = h_flex()
                .id(SharedString::from(selector.clone()))
                .w_full()
                .h(scale::design(ROW_HEIGHT))
                .flex_shrink_0()
                .px_3()
                .items_center()
                .justify_between()
                .rounded(theme.radius)
                .debug_selector(move || selector.clone())
                .child(
                    div()
                        .font_family(crate::fonts::MONO)
                        .text_sm()
                        .child(super::keybindings_view::highlighted_text(
                            &row.label,
                            &row.indices,
                            paint.accent,
                        )),
                )
                .child(
                    div()
                        .font_family(crate::fonts::MONO)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(row.detail.clone())),
                )
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    on_click(position, window, cx);
                });
            rows = rows.child(super::listrow::paint_row(element, paint, position == c.highlighted()));
        }
        column = column.child(rows);
    } else if c.no_matches() {
        column = column.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "scope-expr-no-matches".to_string())
                .child("no matches"),
        );
    }
    if let Some(warning) = c.warning() {
        column = column.child(
            div()
                .text_sm()
                .text_color(chip::chip_paint(theme, chip::Tone::WarningText).text)
                .debug_selector(|| "scope-expr-warning".to_string())
                .child(SharedString::from(warning.to_string())),
        );
    }
    column.into_any_element()
}
```

`render` clones label strings into `SharedString` per row per paint. That
is bounded by `MAX_ROWS` and matches `choice_rows`. If Clippy or review
objects, keep `SharedString` labels on `Row` instead. The rows use IDs made
from their labels, per the stable-ID rule. `debug_selector` values match the
tests.

- [ ] **Step 5: Wire the dialog in `scope_expr_view.rs`**

1. **State.** Add `pub completion: crate::exprcomplete::ExprCompletion` to
   `ScopeExprState`. It is initialised with `Default` in `new`. Change the
   derive to `#[derive(Debug)]`, because `ExprCompletion` is not `Clone` or
   `PartialEq`. Fix any caller that cloned or compared the whole state; the
   compiler names them.
2. **`request_scope`** in the pure core:

```rust
/// The scope a new expression will be ANDed with, which narrows its value
/// suggestions. Whole replaces the expression, so it is dropped. Add joins
/// it, so it is kept. Term keeps the other terms.
pub fn request_scope(mode: &Mode, current: &Scope) -> Scope {
    let mut scope = current.clone();
    scope.expression = match mode {
        Mode::Whole => None,
        Mode::Add => current.expression.clone(),
        Mode::Term { index, .. } => current.expression.as_ref().and_then(|e| {
            Expr::from_conjuncts(
                e.conjuncts()
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| i != index)
                    .map(|(_, t)| t.clone()),
            )
        }),
    };
    scope
}
```

   Import `geode_core::scope::Scope` and `geode_core::scope::complete::ExprVocab`.
3. **Commit check.** `commit_text` gains `vocab: &ExprVocab`. After a
   successful parse:

```rust
    let expr = parse_expr(text).map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
    if let Some(w) = geode_core::scope::complete::check(text, vocab, None).into_iter().next() {
        return Err(w.message);
    }
    Ok(Some(expr))
```

   `apply` gains `vocab: &ExprVocab` and passes it through. `handle_key`
   calls `apply(f, &mode, &text, &vocab)`, with `let vocab =
   shell.expr_vocab.clone();` taken before `frame.update`.
4. **Keys.** At the top of `handle_key`, before the enter check, add:

```rust
    if super::expr_suggest::handle_key(shell, ks, window, cx) {
        return true;
    }
```

5. **Open.** At the end of `open`, after the seed `set_value`, add
   `super::expr_suggest::refresh(view, cx);`.
6. **Render.** In `build`, add the suggestions directly after the
   `filter_row` child. `build` receives `&ShellView`, not an entity, so get
   one through the modal's entity-capturing form: change
   `open_shell_dialog_with_key(..., build, ...)` to pass
   `move |shell, window, cx| build(shell, &entity, window, cx)` with
   `let entity = cx.entity();` captured in `open`, as `picker.rs` does.
   Then:

```rust
    let entity = entity.clone();
    column = column.child(super::expr_suggest::render(
        &state.completion,
        &shell.expr_scroll,
        theme,
        move |i, window, cx| {
            entity.update(cx, |shell, cx| super::expr_suggest::accept(shell, i, window, cx));
        },
    ));
```

7. **Footer.** Extend each hints const to lead with
   `Hint::Key("tab"), Hint::Text("insert ·"), Hint::Key("up"),
   Hint::Key("down"), Hint::Text("move ·"),` ahead of the existing
   `enter …` entries.
8. **Module doc.** Replace the "Validation here is syntax-only…" paragraph:

```text
//! While typing, `expr_suggest` lists what fits at the caret and warns
//! about schema problems. Enter refuses a syntax error or an unknown
//! column (`geode_core::scope::complete::check`). An operator that means
//! nothing on its column fails at query time, as before.
```

   The last sentence covers ordering on text, which the check does not
   catch; only derived dimensions are checked.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --lib scope_expr`
Expected: every new and existing test in `scope_expr` passes.

Then run the broader suites:
`cargo test -p geode-shell --lib addfilter picker reload exprcomplete`.
Expected: all pass.

If `a_moved_caret_is_followed_before_tab_writes` fails because `home` did
not move the caret, check whether the input maps `home`. Use
`cmd-left` on macOS, or copy the key the input tests use. Do not weaken the
assertion.

If `clicking_a_row_inserts_it_and_typing_continues` loses focus, add
`window.prevent_default();` at the top of the `on_mouse_down` closure in
`render`, before `on_click`, as `dialog::open_shell_dialog` does for its
opening press.

- [ ] **Step 7: Add mutation entries, then commit**

```zsh
# The input observer is what follows a caret moved without typing.
run_mutation "expr suggest: the list follows a moved caret" \
  crates/geode-shell/src/shell/mod.rs \
  '        cx.observe(&dialog_input, |view, _input, cx| expr_suggest::refresh(view, cx))' \
  '        cx.observe(&dialog_input, |_view, _input, _cx| {})' \
  geode-shell \
  a_moved_caret_is_followed_before_tab_writes

# Add mode keeps the frame's expression in the values request.
run_mutation "expr suggest: add mode narrows by the current expression" \
  crates/geode-shell/src/shell/scope_expr_view.rs \
  '        Mode::Add => current.expression.clone(),' \
  '        Mode::Add => None,' \
  geode-shell \
  add_mode_requests_values_under_the_current_expression

# Enter refuses an unknown column once a schema exists.
run_mutation "expr suggest: enter refuses an unknown column" \
  crates/geode-shell/src/shell/scope_expr_view.rs \
  '        return Err(w.message);' \
  '        let _ = w;' \
  geode-shell \
  enter_refuses_an_unknown_column

# A row click inserts it.
run_mutation "expr suggest: a row click inserts" \
  crates/geode-shell/src/shell/expr_suggest.rs \
  '                    on_click(position, window, cx);' \
  '                    let _ = (position, window, cx);' \
  geode-shell \
  clicking_a_row_inserts_it_and_typing_continues
```

If the observer anchor changes shape (for example `observe_in` with a
window argument), update both strings to the exact line.

Run `--anchors-only`, then `zsh scripts/mutation-check.sh "expr suggest"`.
Expected: all `KILLED`.

Run: `cargo fmt --check` and
`cargo clippy -p geode-shell --all-targets -- -D warnings`.
Expected: clean.

```bash
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): suggestions in the scope expression dialog

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Scopes object dialog `expression` field

**Files:**
- Modify: `crates/geode-shell/src/shell/objectdialog/mod.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/render.rs`
- Modify: `crates/geode-shell/src/shell/objectdialog/scopes.rs`
- Modify: `crates/geode-shell/src/shell/expr_suggest.rs`
- Test: `crates/geode-shell/src/shell/tests/objectdialog.rs`

**Interfaces:**
- Consumes: everything from Task 4's `expr_suggest`.
- Produces:
  - `pub expr: Option<ExprCompletion>` on `ObjectDialogState`
  - `pub fn expression_scope(draft: &Draft, config: &Config) -> Scope` in
    `scopes.rs`
  - `pub(crate) fn expression_entry_open(state: &ObjectDialogState) -> bool`
    in `objectdialog/mod.rs`

- [ ] **Step 1: Write the failing tests**

Add to `shell/tests/objectdialog.rs`, next to the saved-scope tests:

```rust
/// Walk the Scopes edit cursor to the Expression row with `j` and open it
/// with `i`. Production keys only.
fn open_expression_field(shell: &Entity<ShellView>, cx: &mut gpui::VisualTestContext) {
    cx.simulate_keystrokes("enter"); // open `mine`
    for _ in 0..12 {
        let on_expression = edit_draft(shell, cx, |d| {
            matches!(d.selected_row(), Some(objectdialog::EditRow::Field(i)) if d.fields[i].key == "expression")
        });
        if on_expression {
            break;
        }
        cx.simulate_keystrokes("j");
    }
    cx.simulate_keystrokes("i");
    cx.run_until_parked();
}

/// The open `expression` field lists columns under it. Tab inserts one,
/// and the draft's query follows, so the text survives the modal's text
/// sync.
#[gpui::test]
fn the_scopes_expression_field_suggests_and_tab_inserts(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_saved_scope(), dir.path(), "config::scopes");
    open_expression_field(&shell, &mut cx);
    assert!(cx.debug_bounds("scope-expr-row-npv").is_some(), "columns listed under the field");
    cx.simulate_input("np");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    let text = shell.read_with(&cx, |s, cx| s.dialog_input.read(cx).value().to_string());
    assert_eq!(text, "npv ");
    assert_eq!(edit_draft(&shell, &cx, |d| d.query.clone()), "npv ");
    cx.simulate_input("> 0");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.fields.iter().any(
        |f| f.key == "expression" && matches!(&f.kind, FieldKind::Text(t) if t == "npv > 0")
    )));
}

/// Enter on an unknown column refuses with the notice and keeps the field open.
#[gpui::test]
fn the_scopes_expression_field_refuses_an_unknown_column(cx: &mut gpui::TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_saved_scope(), dir.path(), "config::scopes");
    open_expression_field(&shell, &mut cx);
    cx.simulate_input("bokk = 'A'");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(edit_draft(&shell, &cx, |d| d.text_entry.is_some()), "still open");
    assert_eq!(
        dialog_state(&shell, &cx, |s| s.notice.clone()),
        Some("expression: unknown column 'bokk'; did you mean 'book'?".to_string())
    );
}

/// Values for the Scopes field are narrowed by that scope's own
/// selections, not the frame's, and delivered under `EXPR_KEY`.
#[gpui::test]
fn the_scopes_expression_field_requests_values_under_the_edited_scope(
    cx: &mut gpui::TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let (shell, mut cx) =
        dialog_test_shell_in_dir(cx, services_with_a_saved_scope(), dir.path(), "config::scopes");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    cx.update(|_, app| {
        let seen = seen.clone();
        app.subscribe(&shell, move |_, e: &ShellEvent, _| {
            if let ShellEvent::DistinctRequested(p) = e {
                seen.borrow_mut().push(p.clone());
            }
        })
        .detach();
    });
    open_expression_field(&shell, &mut cx);
    cx.simulate_input("book = ");
    cx.run_until_parked();
    let req = seen.borrow().last().cloned().expect("a request");
    assert_eq!(req.key, crate::shell::EXPR_KEY);
    assert_eq!(req.scope.dimensions[0].values, ["BK001"], "the scope `mine` selects");
    assert!(req.scope.expression.is_none());
}
```

`d.query`, `d.fields`, `d.text_entry` and `d.selected_row()` are the
draft's field and method names from the object dialog. If any is private,
use the accessor the neighbouring tests use.
`services_with_a_saved_scope` already declares `npv`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-shell --lib the_scopes_expression_field`
Expected: FAIL. No `scope-expr-row-npv` is painted, and Enter accepts
`bokk`.

- [ ] **Step 3: Implement**

1. **State in `objectdialog/mod.rs`.**
   - Add `pub expr: Option<crate::exprcomplete::ExprCompletion>` to
     `ObjectDialogState`. It is initialised to `None` wherever the struct is
     built. Fix any derive that breaks; `ExprCompletion` is `Debug` only.
   - Add:

```rust
/// Whether the Scopes dialog's `expression` field is the open text entry,
/// which is the condition for its suggestions to be shown and to claim keys.
pub(crate) fn expression_entry_open(state: &ObjectDialogState) -> bool {
    state.domain == Domain::Scopes
        && state.draft.as_ref().is_some_and(|d| {
            matches!(d.text_entry, Some(TextEntry { row: EditRow::Field(i), .. })
                if d.fields.get(i).is_some_and(|f| f.key == "expression"))
        })
}
```

2. **`scopes.rs`:**

```rust
/// The scope a new `expression` for this draft is ANDed with: its own
/// selections and text filter, with no expression, since that is being
/// replaced. Narrows the expression field's value suggestions.
pub fn expression_scope(draft: &Draft, config: &Config) -> Scope {
    let mut scope = draft_scope(draft, config, "");
    scope.expression = None;
    scope
}
```

3. **`expr_suggest.rs`: extend the surface lookup.**

```rust
pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion> {
    if let Some(state) = view.scope_expr_dialog.as_mut() {
        return Some(&mut state.completion);
    }
    let state = view.object_dialog.as_mut()?;
    if !super::objectdialog::expression_entry_open(state) {
        return None;
    }
    Some(state.expr.get_or_insert_with(ExprCompletion::default))
}
```

   In `values_scope`, after the frame branch:

```rust
    if let Some(state) = view.object_dialog.as_ref()
        && super::objectdialog::expression_entry_open(state)
        && let Some(draft) = state.draft.as_ref()
    {
        let pending = super::objectdialog::apply::config_with_pending(view);
        let config = pending.as_ref().unwrap_or(&view.services.config);
        return Some(super::objectdialog::scopes::expression_scope(draft, config));
    }
```

   Loosen visibility with `pub(crate)` where the compiler asks (`apply`,
   `scopes`).

   In `accept`, after the `dialog_input.update(...)`, keep the draft in
   step. The object dialog's draft is the text's source of truth, and
   `sync_dialog_text` runs after this key and would restore the old query:

```rust
    let text = view.dialog_input.read(cx).value().to_string();
    if let Some(state) = view.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
    {
        draft.set_query(text);
    }
```

   `accept` is called from pointer handlers too, which do not pass through
   the modal key branch. Its object-dialog path therefore ends with
   `super::dialog::sync_dialog_text(view, window, cx);`, a no-op when the
   text already matches.

4. **`objectdialog/render.rs`, `handle_text_key`.** Right after the
   `if choosing { … }` block, add:

```rust
    if super::expression_entry_open_for(shell) && super::super::expr_suggest::handle_key(shell, ks, window, cx) {
        return true;
    }
```

   Here `expression_entry_open_for(shell)` is a one-line wrapper,
   `shell.object_dialog.as_ref().is_some_and(expression_entry_open)`.
   `handle_text_key` receives no `window` today. Thread `window: &mut
   Window` in from its caller (`handle_edit_key_inner` has it) and update
   the one call site.

5. **Enter's schema check.** In the plain-entry Enter branch, wrap the
   parse closure:

```rust
                let domain = domain.expect("a draft implies an open dialog");
                let vocab = vocab.clone();
                draft.apply_text_entry(&|key, text| {
                    let text = domain.parse_text(key, text)?;
                    if domain == Domain::Scopes && key == "expression" {
                        if let Some(w) = geode_core::scope::complete::check(&text, &vocab, None).into_iter().next() {
                            return Err(format!("expression: {}", w.message));
                        }
                    }
                    Ok(text)
                })
```

   `let vocab = shell.expr_vocab.clone();` goes before `draft_mut(shell)`
   is borrowed. Match `parse_text`'s real return type. It is
   `Result<String, String>` in `scopes.rs`. If the closure type in
   `apply_text_entry` differs, adapt the wrapper, not the rule.

6. **Render under the open field.** In `build_edit`, where
   `.child(filter)` is followed by `.child(list)`, insert the suggestions
   between them when `expression_entry_open(state)`:

```rust
    let suggestions = (super::expression_entry_open(state))
        .then(|| state.expr.as_ref())
        .flatten()
        .map(|c| {
            let entity = entity.clone();
            super::super::expr_suggest::render(c, &shell.expr_scroll, theme, move |i, window, cx| {
                entity.update(cx, |shell, cx| super::super::expr_suggest::accept(shell, i, window, cx));
            })
        });
```

   Then add `.children(suggestions)` after `.child(filter)`. Use whatever
   entity handle `build_edit` already holds for row clicks
   (`entity_for_click` shows the pattern).

7. **Footer hints.** In the `Completions::None` arm of the footer hints,
   when the entry is the Scopes `expression` field, push these instead of
   "type a value":
   - `Hint::new(HintRow::Move, &["up", "down"], "move")`
   - `Hint::new(HintRow::Go, &["tab"], "insert")`
   - `Hint::new(HintRow::Go, &["enter"], "apply")`

8. **Open.** In `open_text_field`, after `Step::Changed` for a Scopes
   `expression` field, call `super::super::expr_suggest::refresh(shell,
   cx)`. That needs `cx`: thread it in the same way as `window`, or rely on
   the input observer. The observer fires when `sync_dialog_text`
   `set_value`s the seed, so check with the test first. If
   `the_scopes_expression_field_suggests_and_tab_inserts` already paints
   rows on open, skip the explicit call.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-shell --lib the_scopes_expression_field`
Expected: all three pass.

Then run: `cargo test -p geode-shell --lib objectdialog scope_expr`.
Expected: all pass. The existing object-dialog Tab test for a plain field
("nothing to complete here") must still pass for the Scopes `text` field.

- [ ] **Step 5: Add a mutation entry, then commit**

```zsh
# The Scopes draft must learn the inserted text, or sync_dialog_text
# restores the old query after the key.
run_mutation "expr suggest: a Scopes insert reaches the draft" \
  crates/geode-shell/src/shell/expr_suggest.rs \
  '        draft.set_query(text);' \
  '        let _ = text;' \
  geode-shell \
  the_scopes_expression_field_suggests_and_tab_inserts
```

Run `--anchors-only`, then `zsh scripts/mutation-check.sh "Scopes insert"`.
Then run `cargo fmt --check` and
`cargo clippy -p geode-shell --all-targets -- -D warnings`.

```bash
git add -A crates/geode-shell scripts/mutation-check.sh
git commit -m "feat(shell): expression suggestions in the Scopes dialog

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Documentation, perf log, full verification

**Files:**
- Modify: `docs/current/input-and-dialogs.md` (the "Frame expression"
  section)
- Modify: `docs/current/configuration-dialogs.md` (the Scopes `expression`
  field)
- Modify: `docs/current/configuration.md` (the "validated against the
  schema" line)
- Modify: `crates/geode-core/README.md` and `crates/geode-shell/README.md`
  (module maps)
- Modify: `docs/perf.md` (the measurement)
- Modify: `crates/geode-blotter/src/tile.rs` (the stale "validated in
  `scope_expr_view`" doc comment near the frame-expression handling)

- [ ] **Step 1: Update the current guides**

In `docs/current/input-and-dialogs.md` → "Frame expression", add a
subsection titled "Suggestions" covering the following:
- the rows by caret position;
- the operators by type;
- the values rules: categorical columns only, requested once per column
  per dialog, narrowed as the Global Constraints table says, stale replies
  dropped;
- the keys: tab inserts, shift+tab and the arrows or ctrl+p/n move, enter
  applies, escape closes;
- undo;
- live warnings, which stay quiet under the caret;
- Enter's schema refusal, and that an empty schema disables it.

Also state the known limitations:
- no date literal, so a malformed date fails at query time;
- values are not narrowed by the text in progress;
- ordering on text is not checked;
- `:filter` completion is unchanged.

In `configuration-dialogs.md`, say the Scopes `expression` field has the
same suggestions while it is open, and that Enter refuses an unknown column
with an `expression:` notice.

In `configuration.md`, make the "validated against the schema" sentence
true: saved scopes are checked by `Scope::validate`, and both expression
fields are checked at Enter.

- [ ] **Step 2: Update the READMEs and the blotter comment**

- `geode-core/README.md`: `scope::complete`, the caret reader, vocab and
  live check, and its one invariant, that it is pure and agrees with the
  parser on every prefix by test.
- `geode-shell/README.md`:
  - `exprcomplete` holds the pure suggestion state;
  - `shell/expr_suggest` is the controller and renderer for both fields;
  - the `EXPR_KEY` reservation.
- `geode-blotter/src/tile.rs`: rewrite the comment so it says what is true
  now. The frame expression is checked against the schema when it is
  entered in the dialog, and a restored session is not re-checked.

- [ ] **Step 3: Record the measurement**

Append an entry to `docs/perf.md` in its existing style. It records:
- the `expr_complete/refresh_20k_values` median from Task 3;
- the machine (`sysctl -n machdep.cpu.brand_string`) and
  `rustc --version`;
- what it includes (lex, context, rank 20k and cap to 50) and what it
  excludes (paint).

- [ ] **Step 4: Full verification**

Run each command and confirm its output. Do not claim a pass without it.

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p geode-shell --features test-support --all-targets
zsh scripts/mutation-check.sh --anchors-only
zsh scripts/mutation-check.sh "expr suggest"
```

Expected:
- `fmt` and `clippy` are clean.
- All tests pass.
- Anchors exit 0 with no `AMBIG` or `DUP` for the new entries.
- Every "expr suggest" entry is `KILLED`.

If `cargo test` shows a failure in a file this branch did not touch, re-run
it under a private target first:
`CARGO_TARGET_DIR=$TMPDIR/geode-expr-target cargo test -p <crate>`. The
shared `target/` may hold another session's binary.

- [ ] **Step 5: Commit**

```bash
git add docs crates/geode-core/README.md crates/geode-shell/README.md crates/geode-blotter/src/tile.rs
git commit -m "docs: scope expression suggestions

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: List the display checks for the user**

These need a real window:
- the row layout and the detail column's alignment;
- the warning colour on a few themes;
- dialog height while the list grows and shrinks;
- the Scopes dialog with the list between the field and its rows.
