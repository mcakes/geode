//! Resolve expression references and `:` command targets against loaded
//! source slots. The shared expression parser retains references as typed;
//! this module converts each reference to a slot number and refuses the
//! expression if any reference is missing or ambiguous.
//!
//! An explicit `identity@source` selects an exact pair. A bare identity
//! prefers the default source, otherwise it must identify one loaded slot.
//! Duplicate matches are errors, even when their bucket rules differ.
//! Expressions have no referenceable name and cannot depend on each other.

use geode_core::series::SlotKind;
use geode_core::series::expr::{self, Expr, RefName};

use super::model::Slot;

pub fn resolve(text: &str, slots: &[Slot], default_source: Option<&str>) -> Result<Expr, String> {
    let ast = expr::parse(text).map_err(|e| e.message)?;
    let mut err: Option<String> = None;
    let resolved = ast.resolve(&mut |r: &RefName| match find_source(
        &r.identity,
        r.source.as_deref(),
        slots,
        default_source,
    ) {
        Ok(n) => Some(n),
        Err(e) => {
            err.get_or_insert(e);
            None
        }
    });
    let expr = match resolved {
        Ok(e) => e,
        Err(_) => return Err(err.unwrap_or_else(|| "unresolved reference".into())),
    };
    if expr.slots().is_empty() {
        return Err("an expression must reference a loaded series".into());
    }
    Ok(expr)
}

/// The slot a series name typed after a `:` verb means: `identity` or
/// `identity@source`, split at the `@`.
pub fn find_named(name: &str, slots: &[Slot], default_source: Option<&str>) -> Result<u8, String> {
    match name.split_once('@') {
        Some((identity, source)) => find_source(identity, Some(source), slots, default_source),
        None => find_source(name, None, slots, default_source),
    }
}

/// With a source, the one source slot holding that exact pair. Without,
/// the one under the default source holding `identity`, else the one
/// slot anywhere holding it. None is "not loaded"; several is ambiguous,
/// and the refusal lists their labels (with the rule where two labels
/// read the same, a pair loaded twice).
pub fn find_source(
    identity: &str,
    source: Option<&str>,
    slots: &[Slot],
    default_source: Option<&str>,
) -> Result<u8, String> {
    let named = |s: &&Slot| match &s.kind {
        SlotKind::Source {
            source: ss,
            identity: ii,
            ..
        } => ii == identity && source.is_none_or(|want| ss == want),
        SlotKind::Expr(_) => false,
    };
    let mut matches: Vec<&Slot> = slots.iter().filter(named).collect();
    if source.is_none()
        && let Some(d) = default_source
        && matches
            .iter()
            .any(|s| matches!(&s.kind, SlotKind::Source { source, .. } if source == d))
    {
        matches.retain(|s| matches!(&s.kind, SlotKind::Source { source, .. } if source == d));
    }
    let typed = match source {
        Some(s) => format!("{identity}@{s}"),
        None => identity.to_string(),
    };
    match matches.as_slice() {
        [] => Err(format!("'{typed}' is not loaded — `a` adds it")),
        [one] => Ok(one.number),
        many => {
            let labels: Vec<String> = many.iter().map(|s| s.label(default_source)).collect();
            let listed: Vec<String> = many
                .iter()
                .zip(&labels)
                .map(|(s, label)| {
                    let twin = labels.iter().filter(|l| *l == label).count() > 1;
                    match &s.kind {
                        SlotKind::Source { rule, .. } if twin => {
                            format!("{label} ({})", rule.as_str())
                        }
                        _ => label.clone(),
                    }
                })
                .collect();
            Err(format!("'{typed}' is ambiguous: {}", listed.join(" or ")))
        }
    }
}

/// The text a rewritten expression uses to name source slot `number`:
/// always the full `identity@source`, never the default-aware label, so
/// a later change of the default source cannot retarget it. It must
/// parse as that one name and resolve back to exactly that slot; `Err`
/// says why not (a pair loaded twice, or a name the grammar cannot
/// spell).
pub fn name_for(number: u8, slots: &[Slot]) -> Result<String, String> {
    let slot = slots
        .iter()
        .find(|s| s.number == number)
        .ok_or("that series is gone")?;
    let SlotKind::Source {
        source, identity, ..
    } = &slot.kind
    else {
        return Err("an expression has no name to reference".into());
    };
    let full = format!("{identity}@{source}");
    match expr::parse(&full) {
        Ok(expr::Ast::Ref(r)) if r.identity == *identity && r.source.as_deref() == Some(source) => {
        }
        _ => return Err(format!("'{full}' cannot be written in an expression")),
    }
    match find_source(identity, Some(source), slots, None)? {
        n if n == number => Ok(full),
        _ => Err(format!("'{full}' names another series")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Model;
    use geode_core::series::BucketRule;
    use geode_core::series::expr::{Ast, Op};

    fn model() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap(); // 1
        m.add_source("VIX", "demo_kdb", "series").unwrap(); // 2
        m.add_source("VIX", "demo_rest", "series").unwrap(); // 3
        m.add_source("NKY.close", "demo_rest", "series").unwrap(); // 4
        m
    }

    #[test]
    fn an_exact_pair_a_default_source_identity_and_a_unique_identity_all_resolve() {
        let m = model();
        assert_eq!(
            resolve("SPX.close / NKY.close", m.slots(), Some("demo_kdb")).unwrap(),
            Ast::Bin(Op::Div, Box::new(Ast::Ref(1)), Box::new(Ast::Ref(4)))
        );
        assert_eq!(
            resolve("VIX@demo_rest", m.slots(), Some("demo_kdb")).unwrap(),
            Ast::Ref(3)
        );
        assert_eq!(
            resolve("VIX", m.slots(), Some("demo_kdb")).unwrap(),
            Ast::Ref(2),
            "bare identity, default source"
        );
        assert_eq!(
            resolve("NKY.close", m.slots(), Some("demo_kdb")).unwrap(),
            Ast::Ref(4),
            "not under the default source, but exactly one loaded slot has it"
        );
    }

    #[test]
    fn ambiguity_and_absence_are_named_errors_that_name_series_by_label() {
        let m = model();
        assert_eq!(
            resolve("VIX", m.slots(), Some("demo_rest")).unwrap(),
            Ast::Ref(3)
        );
        let e = resolve("VIX", m.slots(), None).unwrap_err();
        assert_eq!(e, "'VIX' is ambiguous: VIX@demo_kdb or VIX@demo_rest");
        let e = resolve("V2X + 1", m.slots(), Some("demo_kdb")).unwrap_err();
        assert_eq!(e, "'V2X' is not loaded — `a` adds it");
        let e = resolve("s1", m.slots(), Some("demo_kdb")).unwrap_err();
        assert_eq!(
            e, "'s1' is not loaded — `a` adds it",
            "a handle-shaped word is a name like any other"
        );
        let e = resolve("1 + 2", m.slots(), Some("demo_kdb")).unwrap_err();
        assert_eq!(e, "an expression must reference a loaded series");
        let e = resolve("VIX ^ 2", m.slots(), Some("demo_kdb")).unwrap_err();
        assert!(e.contains("arithmetic only"), "{e}");
    }

    /// A pair loaded twice (two rules) cannot be told apart by name, so
    /// naming it is refused — never resolved to whichever came first.
    #[test]
    fn a_pair_loaded_twice_is_ambiguous_and_lists_the_rules() {
        let mut m = model();
        m.add_source("VIX", "demo_kdb", "series").unwrap(); // 5
        m.set_rule(5, BucketRule::Mean).unwrap();
        let e = resolve("VIX@demo_kdb", m.slots(), Some("demo_kdb")).unwrap_err();
        assert_eq!(e, "'VIX@demo_kdb' is ambiguous: VIX (last) or VIX (mean)");
        assert_eq!(
            find_named("VIX", m.slots(), Some("demo_kdb")).unwrap_err(),
            "'VIX' is ambiguous: VIX (last) or VIX (mean)"
        );
    }

    #[test]
    fn an_expression_is_never_a_name() {
        let mut m = model();
        let e = resolve("SPX.close / VIX", m.slots(), Some("demo_kdb")).unwrap();
        m.add_expr("SPX.close / VIX", e).unwrap();
        assert!(find_named("SPX.close / VIX", m.slots(), Some("demo_kdb")).is_err());
        assert_eq!(
            name_for(5, m.slots()).unwrap_err(),
            "an expression has no name to reference"
        );
    }

    #[test]
    fn find_named_splits_at_the_at_sign() {
        let m = model();
        assert_eq!(
            find_named("VIX@demo_rest", m.slots(), Some("demo_kdb")),
            Ok(3)
        );
        assert_eq!(find_named("VIX", m.slots(), Some("demo_kdb")), Ok(2));
        assert_eq!(find_named("NKY.close", m.slots(), Some("demo_kdb")), Ok(4));
        assert_eq!(
            find_named("NKY.close@demo_kdb", m.slots(), Some("demo_kdb")).unwrap_err(),
            "'NKY.close@demo_kdb' is not loaded — `a` adds it"
        );
    }

    #[test]
    fn name_for_is_always_the_full_pair_and_resolves_back() {
        let mut m = model();
        assert_eq!(
            name_for(2, m.slots()).unwrap(),
            "VIX@demo_kdb",
            "never the bare label, whatever the default source"
        );
        assert_eq!(name_for(3, m.slots()).unwrap(), "VIX@demo_rest");
        m.add_source("VIX", "demo_kdb", "series").unwrap(); // 5
        assert_eq!(
            name_for(2, m.slots()).unwrap_err(),
            "'VIX@demo_kdb' is ambiguous: VIX@demo_kdb (last) or VIX@demo_kdb (last)",
            "a pair loaded twice has no name of its own"
        );
        m.add_source("9lives", "demo_kdb", "series").unwrap(); // 6
        assert_eq!(
            name_for(6, m.slots()).unwrap_err(),
            "'9lives@demo_kdb' cannot be written in an expression"
        );
    }

    #[test]
    fn removing_an_operand_removes_its_dependants() {
        let mut m = model();
        let e = resolve("SPX.close / VIX", m.slots(), Some("demo_kdb")).unwrap();
        m.add_expr("SPX.close / VIX", e).unwrap(); // 5
        let e = resolve("VIX@demo_rest + 1", m.slots(), Some("demo_kdb")).unwrap();
        m.add_expr("VIX@demo_rest + 1", e).unwrap(); // 6
        assert_eq!(m.dependants(2), vec![5]);
        let r = m.remove(2).unwrap();
        assert_eq!(r.removed, vec![2, 5]);
        assert!(r.changed.query() && r.changed.session());
        let left: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(left, vec![1, 3, 4, 6]);
        assert_eq!(
            m.cursor(),
            Some(3),
            "the cursor is clamped to the last slot"
        );
    }
}
