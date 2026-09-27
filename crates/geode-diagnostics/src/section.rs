//! The page's sections, in rail order.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Sources,
    Data,
    Config,
    Log,
    Perf,
}

impl Section {
    pub const ALL: [Section; 5] = [
        Section::Sources,
        Section::Data,
        Section::Config,
        Section::Log,
        Section::Perf,
    ];

    /// The session and `:section` spelling.
    pub fn name(self) -> &'static str {
        match self {
            Section::Sources => "sources",
            Section::Data => "data",
            Section::Config => "config",
            Section::Log => "log",
            Section::Perf => "perf",
        }
    }

    /// The rail label.
    pub fn title(self) -> &'static str {
        match self {
            Section::Sources => "Sources",
            Section::Data => "Data",
            Section::Config => "Config",
            Section::Log => "Log",
            Section::Perf => "Perf",
        }
    }

    pub fn from_name(s: &str) -> Option<Section> {
        Section::ALL.iter().copied().find(|x| x.name() == s)
    }

    pub fn next(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + 1) % Section::ALL.len()]
    }

    pub fn prev(self) -> Section {
        let i = Section::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Section::ALL[(i + Section::ALL.len() - 1) % Section::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_cycling_wraps() {
        for s in Section::ALL {
            assert_eq!(Section::from_name(s.name()), Some(s));
        }
        assert_eq!(Section::from_name("nope"), None);
        assert_eq!(Section::Perf.next(), Section::Sources);
        assert_eq!(Section::Sources.prev(), Section::Perf);
    }
}
