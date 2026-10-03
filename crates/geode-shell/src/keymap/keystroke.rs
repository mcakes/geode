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

    /// Whether the modifiers form a chord. Control, Alt, and Command do;
    /// Shift alone remains typing, so focused text fields retain shifted letters.
    pub fn is_chord(self) -> bool {
        self.ctrl || self.alt || self.cmd
    }

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

/// The multi-character key names gpui reports in `Keystroke::key`: the
/// pinned platform backends' named keys (gpui's `is_printable_key` list,
/// plus `space`, `tab`, `enter`, and Windows' `menu`). Every other key is
/// one character.
pub const NAMED_KEYS: &[&str] = &[
    "space",
    "tab",
    "enter",
    "escape",
    "backspace",
    "delete",
    "insert",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "back",
    "forward",
    "menu",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
    "f21",
    "f22",
    "f23",
    "f24",
    "f25",
    "f26",
    "f27",
    "f28",
    "f29",
    "f30",
    "f31",
    "f32",
    "f33",
    "f34",
    "f35",
];

fn is_modifier_name(part: &str) -> bool {
    matches!(
        part.to_ascii_lowercase().as_str(),
        "ctrl" | "alt" | "shift" | "cmd" | "super" | "win" | "mod"
    )
}

/// Check one key part against what a keyboard sends. A key the platform
/// never reports would compile into a binding that can never fire, so it
/// is refused with the spelling that would: modifiers join with `+`, and
/// shift is a modifier, never a letter's case.
fn key_name(part: &str, spec: &str) -> Result<String, String> {
    let mut chars = part.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_uppercase() {
            return Err(format!(
                "'{spec}': write shift+{} for a shifted letter; a key's case is not shift",
                c.to_lowercase()
            ));
        }
        return Ok(part.to_string());
    }
    let lower = part.to_ascii_lowercase();
    if NAMED_KEYS.contains(&lower.as_str()) {
        return Ok(lower);
    }
    let dashed: Vec<&str> = part.split('-').collect();
    if dashed.len() > 1
        && dashed[..dashed.len() - 1]
            .iter()
            .all(|m| is_modifier_name(m))
        && !dashed[dashed.len() - 1].is_empty()
    {
        return Err(format!(
            "'{spec}': join modifiers with '+', as in {}",
            dashed.join("+")
        ));
    }
    Err(format!("'{spec}': '{part}' is not a key name"))
}

/// Parse one keystroke spec like `mod+shift+h`. `mod` expands to
/// `mod_alias` (the user-configurable primary modifier). Modifier names
/// ignore case; the key must be one character (a letter in lowercase:
/// shift is spelled `shift+`) or one of [`NAMED_KEYS`], in any case.
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
                key = Some(key_name(part, s)?);
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
    fn modifier_and_named_key_case_is_ignored() {
        let ks = parse_keystroke("Ctrl+g", Modifiers::NONE).unwrap();
        assert_eq!(ks.key, "g");
        assert!(ks.mods.ctrl && !ks.mods.shift);
        assert_eq!(
            parse_keystroke("Escape", Modifiers::NONE).unwrap().key,
            "escape"
        );
    }

    /// A key no keyboard reports would bind silently and never fire.
    #[test]
    fn keys_a_keyboard_never_sends_are_refused_with_the_spelling_that_works() {
        let err = parse_keystroke("ctrl+G", Modifiers::NONE).unwrap_err();
        assert!(err.contains("shift+g"), "{err}");
        assert!(parse_binding("z R", Modifiers::NONE).is_err());
        let err = parse_keystroke("alt-backspace", Modifiers::NONE).unwrap_err();
        assert!(err.contains("alt+backspace"), "{err}");
        let err = parse_keystroke("shift-tab", Modifiers::NONE).unwrap_err();
        assert!(err.contains("shift+tab"), "{err}");
        let err = parse_keystroke("pgdn", Modifiers::NONE).unwrap_err();
        assert!(err.contains("not a key name"), "{err}");
    }

    #[test]
    fn every_gpui_named_key_and_single_characters_parse() {
        // Spelled out from the pinned gpui backends rather than read from
        // `NAMED_KEYS`, so dropping or misspelling a name there fails here.
        let gpui_names = [
            "space",
            "tab",
            "enter",
            "escape",
            "backspace",
            "delete",
            "insert",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "back",
            "forward",
            "menu",
            "f1",
            "f12",
            "f24",
            "f35",
        ];
        for name in gpui_names {
            assert_eq!(parse_keystroke(name, Modifiers::NONE).unwrap().key, name);
        }
        for n in 1..=35 {
            let name = format!("f{n}");
            assert_eq!(parse_keystroke(&name, Modifiers::NONE).unwrap().key, name);
        }
        for key in ["-", "=", "[", "/", "1", "é", "ctrl+-", "shift+="] {
            assert!(parse_keystroke(key, Modifiers::NONE).is_ok(), "{key}");
        }
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
