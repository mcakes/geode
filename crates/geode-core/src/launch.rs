//! What a source tile knows at its cursor that another module may open
//! on. The shell pulls it from the focused tile (`TileContent::
//! launch_context`) and lists the kinds whose factory accepts it; the
//! factory translates it into its own restored-state table. Typed, not a
//! table, so a source and a target agree on the words without depending
//! on each other.

/// The context at a source tile's cursor. Every field is `None` when the
/// cursor names no single value: an ambiguous or NULL value is empty, never
/// a guess, because a panel opened on a made-up key is a plausible wrong
/// answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchContext {
    /// The desk's underlying identifier, as the blotter's `underlying_ref`
    /// column and a pricer instrument spell it.
    pub underlying: Option<String>,
}

/// A field of [`LaunchContext`] a target module can open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextField {
    Underlying,
}

impl LaunchContext {
    pub fn is_empty(&self) -> bool {
        self.underlying.is_none()
    }

    pub fn has(&self, field: ContextField) -> bool {
        match field {
            ContextField::Underlying => self.underlying.is_some(),
        }
    }

    /// Whether a kind accepting `accepts` can open on every field set here.
    /// An empty context is covered by nothing: there is nothing to open on.
    pub fn covered_by(&self, accepts: &[ContextField]) -> bool {
        !self.is_empty()
            && [ContextField::Underlying]
                .iter()
                .all(|f| !self.has(*f) || accepts.contains(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spx() -> LaunchContext {
        LaunchContext {
            underlying: Some("SPX".into()),
        }
    }

    #[test]
    fn an_empty_context_has_nothing_and_is_covered_by_nothing() {
        let c = LaunchContext::default();
        assert!(c.is_empty());
        assert!(!c.has(ContextField::Underlying));
        assert!(!c.covered_by(&[ContextField::Underlying]));
        assert!(!c.covered_by(&[]));
    }

    #[test]
    fn an_underlying_is_covered_only_by_a_kind_accepting_it() {
        let c = spx();
        assert!(!c.is_empty());
        assert!(c.has(ContextField::Underlying));
        assert!(c.covered_by(&[ContextField::Underlying]));
        assert!(!c.covered_by(&[]), "a kind accepting nothing is not listed");
    }
}
