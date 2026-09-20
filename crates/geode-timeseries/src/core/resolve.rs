//! §7's resolution rules — the module's half of the expression language.
//! The parser (`geode_core::series::expr`) knows references as typed;
//! this turns them into slot numbers against THIS tile's slots. Nothing
//! is sent until every reference resolves.

use geode_core::series::expr::{self, Expr, RefName, expression_order};
use geode_core::series::{SeriesSpec, SlotKind};

use super::model::Slot;

/// `editing` is the slot being REPLACED (`e`), excluded from what the
/// text may reference and checked for a cycle through the others.
pub fn resolve(
    text: &str,
    slots: &[Slot],
    default_source: Option<&str>,
    editing: Option<u8>,
) -> Result<Expr, String> {
    let ast = expr::parse(text).map_err(|e| e.message)?;
    let mut err: Option<String> = None;
    let resolved = ast.resolve(&mut |r: &RefName| match lookup(r, slots, default_source) {
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
    let refs = expr.slots();
    if refs.is_empty() {
        return Err("an expression must reference a loaded series".into());
    }
    if let Some(me) = editing {
        if refs.contains(&me) {
            return Err(format!("s{me} cannot reference itself"));
        }
        // A cycle through another expression: order the specs with this
        // candidate standing in for its slot.
        let specs: Vec<SeriesSpec> = slots
            .iter()
            .map(|s| SeriesSpec {
                slot: s.number,
                kind: if s.number == me {
                    SlotKind::Expr(expr.clone())
                } else {
                    s.kind.clone()
                },
            })
            .collect();
        if let Err(on_cycle) = expression_order(&specs) {
            let via = if on_cycle == me {
                refs.iter()
                    .find(|n| {
                        slots
                            .iter()
                            .any(|s| s.number == **n && matches!(s.kind, SlotKind::Expr(_)))
                    })
                    .copied()
                    .unwrap_or(me)
            } else {
                on_cycle
            };
            return Err(format!("s{me} cannot reference itself through s{via}"));
        }
    }
    Ok(expr)
}

fn lookup(r: &RefName, slots: &[Slot], default_source: Option<&str>) -> Result<u8, String> {
    match r {
        RefName::Handle(n) => slots
            .iter()
            .find(|s| s.number == *n)
            .map(|s| s.number)
            .ok_or_else(|| format!("no slot s{n}")),
        RefName::Identity {
            identity,
            source: Some(src),
        } => sources(slots)
            .find(|(_, s, i)| s == src && i == identity)
            .map(|(n, _, _)| n)
            .ok_or_else(|| format!("'{identity}@{src}' is not loaded — `a` adds it")),
        RefName::Identity {
            identity,
            source: None,
        } => {
            let matches: Vec<(u8, &str, &str)> =
                sources(slots).filter(|(_, _, i)| i == identity).collect();
            if let Some(d) = default_source
                && let Some((n, _, _)) = matches.iter().find(|(_, s, _)| *s == d)
            {
                return Ok(*n);
            }
            match matches.as_slice() {
                [] => Err(format!("'{identity}' is not loaded — `a` adds it")),
                [(n, _, _)] => Ok(*n),
                many => Err(format!(
                    "'{identity}' is ambiguous: {}",
                    many.iter()
                        .map(|(n, s, i)| format!("{i}@{s} (s{n})"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                )),
            }
        }
    }
}

fn sources(slots: &[Slot]) -> impl Iterator<Item = (u8, &str, &str)> {
    slots.iter().filter_map(|s| match &s.kind {
        SlotKind::Source {
            source, identity, ..
        } => Some((s.number, source.as_str(), identity.as_str())),
        SlotKind::Expr(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Model;
    use geode_core::series::expr::{Ast, Op};

    fn model() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap(); // s1
        m.add_source("VIX", "demo_kdb", "series").unwrap(); // s2
        m.add_source("VIX", "demo_rest", "series").unwrap(); // s3
        m.add_source("NKY.close", "demo_rest", "series").unwrap(); // s4
        m
    }

    #[test]
    fn a_handle_an_exact_pair_and_a_default_source_identity_all_resolve() {
        let m = model();
        assert_eq!(
            resolve("s1 / s4", m.slots(), Some("demo_kdb"), None).unwrap(),
            Ast::Bin(Op::Div, Box::new(Ast::Ref(1)), Box::new(Ast::Ref(4)))
        );
        assert_eq!(
            resolve("VIX@demo_rest", m.slots(), Some("demo_kdb"), None).unwrap(),
            Ast::Ref(3)
        );
        assert_eq!(
            resolve("SPX.close", m.slots(), Some("demo_kdb"), None).unwrap(),
            Ast::Ref(1),
            "bare identity, default source"
        );
        assert_eq!(
            resolve("NKY.close", m.slots(), Some("demo_kdb"), None).unwrap(),
            Ast::Ref(4),
            "not under the default source, but exactly one loaded slot has it"
        );
    }

    #[test]
    fn ambiguity_and_absence_are_named_errors() {
        let m = model();
        assert_eq!(
            resolve("VIX", m.slots(), Some("demo_rest"), None).unwrap(),
            Ast::Ref(3)
        );
        let e = resolve("VIX", m.slots(), None, None).unwrap_err();
        assert_eq!(
            e,
            "'VIX' is ambiguous: VIX@demo_kdb (s2) or VIX@demo_rest (s3)"
        );
        let e = resolve("V2X + 1", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "'V2X' is not loaded — `a` adds it");
        let e = resolve("s9", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "no slot s9");
        let e = resolve("1 + 2", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert_eq!(e, "an expression must reference a loaded series");
        let e = resolve("s1 ^ 2", m.slots(), Some("demo_kdb"), None).unwrap_err();
        assert!(e.contains("arithmetic only"), "{e}");
    }

    #[test]
    fn an_expression_may_reference_an_expression_but_not_itself_or_a_cycle() {
        let mut m = model();
        let e = resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        let (n, _) = m.add_expr("s1 / s2", e).unwrap(); // s5
        assert_eq!(n, 5);
        let e = resolve("s5 * 100", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s5 * 100", e).unwrap(); // s6
        assert_eq!(
            resolve("s6 * 2", m.slots(), Some("demo_kdb"), Some(5)).unwrap_err(),
            "s5 cannot reference itself through s6"
        );
        assert_eq!(
            resolve("s5", m.slots(), Some("demo_kdb"), Some(5)).unwrap_err(),
            "s5 cannot reference itself"
        );
        // Editing s5 to something else is fine.
        assert!(resolve("s1 - s2", m.slots(), Some("demo_kdb"), Some(5)).is_ok());
    }

    #[test]
    fn removing_an_operand_removes_its_dependants_transitively() {
        let mut m = model();
        let e = resolve("s1 / s2", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s1 / s2", e).unwrap(); // s5
        let e = resolve("s5 * 100", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s5 * 100", e).unwrap(); // s6
        let e = resolve("s3 + 1", m.slots(), Some("demo_kdb"), None).unwrap();
        m.add_expr("s3 + 1", e).unwrap(); // s7
        assert_eq!(m.dependants(2), vec![5, 6]);
        let r = m.remove(2).unwrap();
        assert_eq!(r.removed, vec![2, 5, 6]);
        assert!(r.changed.query() && r.changed.session());
        let left: Vec<u8> = m.slots().iter().map(|s| s.number).collect();
        assert_eq!(left, vec![1, 3, 4, 7]);
        assert_eq!(
            m.cursor(),
            Some(3),
            "the cursor is clamped to the last slot"
        );
    }
}
