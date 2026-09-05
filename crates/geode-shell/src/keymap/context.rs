const MAX_DEPTH: u32 = 64;

/// One frame of the focus-context stack, e.g. `blotter` with `mode=normal`.
/// The stack runs outermost → innermost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyContext {
    flags: Vec<String>,
    pairs: Vec<(String, String)>,
}

impl KeyContext {
    pub fn new(name: impl Into<String>) -> Self {
        KeyContext {
            flags: vec![name.into()],
            pairs: Vec::new(),
        }
    }

    pub fn flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.push(flag.into());
        self
    }

    pub fn pair(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.pairs.push((key.into(), value.into()));
        self
    }

    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// The flag a context sets to opt into count prefixes (Phase 3 §3.3):
/// while the innermost context on the stack carries it, bare digits
/// accumulate in the matcher instead of being matched.
pub const COUNTS: &str = "counts";

impl KeyContext {
    /// Opt this context into count prefixes.
    pub fn counts(self) -> Self {
        self.flag(COUNTS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    Flag(String),
    Eq(String, String),
    NotEq(String, String),
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
}

impl Predicate {
    pub fn eval(&self, stack: &[KeyContext]) -> bool {
        match self {
            Predicate::Flag(f) => stack.iter().any(|c| c.has_flag(f)),
            Predicate::Eq(k, v) => lookup(stack, k).is_some_and(|x| x == v),
            Predicate::NotEq(k, v) => lookup(stack, k).is_some_and(|x| x != v),
            Predicate::Not(p) => !p.eval(stack),
            Predicate::And(a, b) => a.eval(stack) && b.eval(stack),
            Predicate::Or(a, b) => a.eval(stack) || b.eval(stack),
        }
    }
}

/// Innermost definition of `key` wins.
fn lookup<'a>(stack: &'a [KeyContext], key: &str) -> Option<&'a str> {
    stack.iter().rev().find_map(|c| c.get(key))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Str(String),
    And,
    Or,
    Not,
    Eq,
    Ne,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '&' => {
                chars.next();
                if chars.next() != Some('&') {
                    return Err("expected '&&'".to_string());
                }
                toks.push(Tok::And);
            }
            '|' => {
                chars.next();
                if chars.next() != Some('|') {
                    return Err("expected '||'".to_string());
                }
                toks.push(Tok::Or);
            }
            '=' => {
                chars.next();
                if chars.next() != Some('=') {
                    return Err("expected '=='".to_string());
                }
                toks.push(Tok::Eq);
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    toks.push(Tok::Ne);
                } else {
                    toks.push(Tok::Not);
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some(ch) if ch == quote => break,
                        Some(ch) => value.push(ch),
                        None => return Err("unterminated string".to_string()),
                    }
                }
                toks.push(Tok::Str(value));
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' => {
                // '-' is accepted anywhere in an identifier so kebab-case context names work.
                let mut ident = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                        ident.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Ident(ident));
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(toks)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn advance(&mut self) -> Option<Tok> {
        let tok = self.toks.get(self.pos).cloned();
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn parse_or(&mut self) -> Result<Predicate, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Tok::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Predicate::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Predicate, String> {
        let mut left = self.parse_unary()?;
        while self.peek() == Some(&Tok::And) {
            self.advance();
            let right = self.parse_unary()?;
            left = Predicate::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Predicate, String> {
        if self.peek() == Some(&Tok::Not) {
            self.advance();
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                self.depth -= 1;
                return Err("context expression too deeply nested".to_string());
            }
            let result = self.parse_unary();
            self.depth -= 1;
            Ok(Predicate::Not(Box::new(result?)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Predicate, String> {
        match self.advance() {
            Some(Tok::LParen) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    self.depth -= 1;
                    return Err("context expression too deeply nested".to_string());
                }
                let inner = self.parse_or();
                self.depth -= 1;
                let inner = inner?;
                if self.advance() != Some(Tok::RParen) {
                    return Err("expected ')'".to_string());
                }
                Ok(inner)
            }
            Some(Tok::Ident(name)) => match self.peek() {
                Some(Tok::Eq) | Some(Tok::Ne) => {
                    let negated = self.advance() == Some(Tok::Ne);
                    let value = match self.advance() {
                        Some(Tok::Ident(v)) | Some(Tok::Str(v)) => v,
                        _ => return Err("expected value after comparison".to_string()),
                    };
                    Ok(if negated {
                        Predicate::NotEq(name, value)
                    } else {
                        Predicate::Eq(name, value)
                    })
                }
                _ => Ok(Predicate::Flag(name)),
            },
            other => Err(format!("unexpected token: {other:?}")),
        }
    }
}

/// Parse a context expression like `blotter && mode == normal`.
pub fn parse_predicate(s: &str) -> Result<Predicate, String> {
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return Err("empty context expression".to_string());
    }
    let mut parser = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let pred = parser.parse_or()?;
    if parser.pos != parser.toks.len() {
        return Err("unexpected trailing tokens".to_string());
    }
    Ok(pred)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Vec<KeyContext> {
        vec![
            KeyContext::new("workspace"),
            KeyContext::new("blotter").pair("mode", "normal"),
        ]
    }

    #[test]
    fn flag_matches_any_context_in_stack() {
        let p = parse_predicate("workspace").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("palette").unwrap();
        assert!(!p.eval(&stack()));
    }

    #[test]
    fn eq_uses_innermost_definition() {
        let outer_and_inner = vec![
            KeyContext::new("a").pair("mode", "visual"),
            KeyContext::new("b").pair("mode", "normal"),
        ];
        assert!(
            parse_predicate("mode == normal")
                .unwrap()
                .eval(&outer_and_inner)
        );
        assert!(
            !parse_predicate("mode == visual")
                .unwrap()
                .eval(&outer_and_inner)
        );
    }

    #[test]
    fn neq_requires_key_present() {
        assert!(parse_predicate("mode != visual").unwrap().eval(&stack()));
        assert!(
            !parse_predicate("missing != anything")
                .unwrap()
                .eval(&stack())
        );
    }

    #[test]
    fn boolean_operators_and_precedence() {
        // ! binds tighter than &&, which binds tighter than ||.
        let p = parse_predicate("palette || blotter && mode == normal").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("!palette && blotter").unwrap();
        assert!(p.eval(&stack()));
        let p = parse_predicate("!(blotter && mode == normal)").unwrap();
        assert!(!p.eval(&stack()));
    }

    #[test]
    fn quoted_values() {
        let ctx = vec![KeyContext::new("x").pair("mode", "insert mode")];
        assert!(
            parse_predicate("mode == \"insert mode\"")
                .unwrap()
                .eval(&ctx)
        );
        assert!(parse_predicate("mode == 'insert mode'").unwrap().eval(&ctx));
    }

    #[test]
    fn parse_errors() {
        assert!(parse_predicate("").is_err());
        assert!(parse_predicate("a &&").is_err());
        assert!(parse_predicate("a == ").is_err());
        assert!(parse_predicate("(a").is_err());
        assert!(parse_predicate("a b").is_err());
        assert!(parse_predicate("mode == \"unterminated").is_err());
    }

    #[test]
    fn deeply_nested_expression_is_an_error_not_a_crash() {
        let many_bangs = format!("{}x", "!".repeat(200_000));
        assert!(parse_predicate(&many_bangs).is_err());
        let many_parens = format!("{}x{}", "(".repeat(200_000), ")".repeat(200_000));
        assert!(parse_predicate(&many_parens).is_err());
    }

    #[test]
    fn reasonable_nesting_still_parses() {
        let nested = format!("{}x{}", "(".repeat(32), ")".repeat(32));
        assert!(parse_predicate(&nested).is_ok());
        let bangs = format!("{}x", "!".repeat(16));
        assert!(parse_predicate(&bangs).is_ok());
    }
}
