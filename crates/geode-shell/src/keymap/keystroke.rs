#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        cmd: false,
    };
    pub const CTRL: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: false,
        cmd: false,
    };
    pub const ALT: Modifiers = Modifiers {
        ctrl: false,
        alt: true,
        shift: false,
        cmd: false,
    };
    pub const CMD: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        cmd: true,
    };

    pub fn union(self, other: Modifiers) -> Modifiers {
        Modifiers {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            cmd: self.cmd || other.cmd,
        }
    }
}

/// One key press with modifiers. `key` is lowercase; shift is always an
/// explicit modifier (`shift+g`), never inferred from case.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keystroke {
    pub mods: Modifiers,
    pub key: String,
}

/// Parse one keystroke spec like `mod+shift+h`. `mod` expands to
/// `mod_alias` (the user-configurable primary modifier).
pub fn parse_keystroke(s: &str, mod_alias: Modifiers) -> Result<Keystroke, String> {
    let mut mods = Modifiers::NONE;
    let mut key: Option<String> = None;
    if s.is_empty() {
        return Err("empty keystroke".to_string());
    }
    for part in s.split('+') {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "" => return Err(format!("empty segment in '{s}'")),
            "ctrl" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "cmd" | "super" | "win" => mods.cmd = true,
            "mod" => mods = mods.union(mod_alias),
            _ => {
                if key.is_some() {
                    return Err(format!("more than one key in '{s}'"));
                }
                key = Some(lower);
            }
        }
    }
    key.map(|key| Keystroke { mods, key })
        .ok_or_else(|| format!("no key in '{s}'"))
}

/// Parse a binding spec: a whitespace-separated sequence of keystrokes
/// (`"g g"`, `"mod+h"`).
pub fn parse_binding(s: &str, mod_alias: Modifiers) -> Result<Vec<Keystroke>, String> {
    let seq: Result<Vec<_>, _> = s
        .split_whitespace()
        .map(|part| parse_keystroke(part, mod_alias))
        .collect();
    let seq = seq?;
    if seq.is_empty() {
        return Err("empty binding".to_string());
    }
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let ks = parse_keystroke("ctrl+shift+h", Modifiers::NONE).unwrap();
        assert!(ks.mods.ctrl && ks.mods.shift && !ks.mods.alt && !ks.mods.cmd);
        assert_eq!(ks.key, "h");
    }

    #[test]
    fn mod_alias_expands() {
        let ks = parse_keystroke("mod+h", Modifiers::ALT).unwrap();
        assert_eq!(ks.mods, Modifiers::ALT);
        let ks = parse_keystroke("mod+shift+q", Modifiers::ALT).unwrap();
        assert!(ks.mods.alt && ks.mods.shift);
    }

    #[test]
    fn keys_are_stored_lowercase() {
        let ks = parse_keystroke("Ctrl+G", Modifiers::NONE).unwrap();
        assert_eq!(ks.key, "g");
        assert!(
            !ks.mods.shift,
            "shift is always explicit, never inferred from case"
        );
    }

    #[test]
    fn named_keys_and_digits() {
        assert_eq!(parse_keystroke("ctrl+1", Modifiers::NONE).unwrap().key, "1");
        assert_eq!(
            parse_keystroke("escape", Modifiers::NONE).unwrap().key,
            "escape"
        );
    }

    #[test]
    fn binding_sequences_split_on_whitespace() {
        let seq = parse_binding("g g", Modifiers::NONE).unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0].key, "g");
        let seq = parse_binding("mod+h", Modifiers::ALT).unwrap();
        assert_eq!(seq.len(), 1);
    }

    #[test]
    fn parse_errors() {
        assert!(parse_keystroke("", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+shift", Modifiers::NONE).is_err());
        assert!(parse_keystroke("ctrl+a+b", Modifiers::NONE).is_err());
        assert!(parse_binding("   ", Modifiers::NONE).is_err());
    }
}
