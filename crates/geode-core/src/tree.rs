//! The parent/child structure of a rollup result (Phase 3 spec §5.5).
//!
//! Built once, on the query worker, inside `Snapshot::from_batches`:
//! for a 729k-row result the build costs tens of milliseconds, which is
//! the whole §7.1 budget if it ran on the render thread. Immutable and
//! `Arc`-shared with the snapshot it describes.
//!
//! The compiler orders by `row_depth` first, so a parent always precedes
//! its children. Within a depth the order is the view's declared sort,
//! then the grouping columns — so siblings are **not** contiguous when a
//! sort is declared, and nothing here assumes they are. Each row's
//! grouping prefix is hashed into a per-depth table and its parent looked
//! up in the table for the depth above; children are then laid out in CSR
//! form in row order, which makes a declared sort the default sibling
//! order for free. Equality is on the cell text under either encoding,
//! with NULL as its own token, so a blanked ENUM value (P2 §3.6) is a
//! distinct key rather than a collision — and nothing depends on how
//! DuckDB collates an ENUM.
//!
//! Spec §5.5 asks for dictionary codes where present, strings under
//! as-of: each grouping column is resolved once, in [`TreeIndex::build`],
//! into an enum tagging it dictionary-encoded, plain text, or absent, and
//! the hash/equality below key a dictionary column on its per-row code
//! rather than resolving the string per cell. One snapshot carries one
//! encoding per column, so code equality implies value equality within
//! it — that identity is exactly what `snapshot.rs`'s
//! `concat_preserving_dictionaries` shared-dictionary fast path
//! guarantees (its own doc records the fallback path that would break
//! it).

use crate::snapshot::{DictCodes, Snapshot};
use std::collections::HashMap;
use std::collections::hash_map::Entry;

pub const NO_PARENT: u32 = u32::MAX;

#[derive(Debug, Clone, Default)]
pub struct TreeIndex {
    parent: Vec<u32>,
    depth: Vec<u8>,
    /// CSR offsets, length `rows + 1`.
    child_start: Vec<u32>,
    children: Vec<u32>,
    roots: Vec<u32>,
    unplaced: u32,
}

/// A grouping column, resolved once so the hot loop below never downcasts
/// or resolves a name per cell.
enum Col<'a> {
    Absent,
    Dict(DictCodes<'a>),
    Text(usize),
}

impl TreeIndex {
    pub fn build(snapshot: &Snapshot) -> TreeIndex {
        let n = snapshot.rows();
        let grouping = snapshot.grouping();
        let cols: Vec<Col> = grouping
            .iter()
            .map(|g| match snapshot.column_index(g) {
                Some(idx) => match snapshot.dict_codes_at(idx) {
                    Some((codes, _)) => Col::Dict(codes),
                    None => Col::Text(idx),
                },
                None => Col::Absent,
            })
            .collect();
        let max_depth = grouping.len();

        let mut depth = vec![0u8; n];
        let mut parent = vec![NO_PARENT; n];
        let mut roots = Vec::new();
        let mut unplaced = 0u32;

        if !snapshot.has_depth_column() {
            roots.extend(0..n as u32);
            return Self::finish(parent, depth, roots, unplaced);
        }

        // Rows bucketed by depth so every parent is indexed before any
        // child looks for it, whatever the row order.
        let mut by_depth: Vec<Vec<u32>> = vec![Vec::new(); max_depth + 1];
        for (r, slot) in depth.iter_mut().enumerate() {
            let d = snapshot.depth_of_row(r).unwrap_or(0).min(max_depth);
            *slot = d as u8;
            by_depth[d].push(r as u32);
        }

        // One hash table per depth, chained through `next` on collision
        // so a lookup verifies the actual prefix rather than trusting a
        // 64-bit hash. Each row sits in exactly one table.
        let mut tables: Vec<HashMap<u64, u32>> = (0..=max_depth).map(|_| HashMap::new()).collect();
        let mut next = vec![NO_PARENT; n];

        for d in 0..=max_depth {
            for &r in &by_depth[d] {
                let row = r as usize;
                if d == 0 {
                    roots.push(r);
                } else {
                    let h = prefix_hash(snapshot, &cols, row, d - 1);
                    let mut candidate = tables[d - 1].get(&h).copied();
                    let mut found = None;
                    while let Some(c) = candidate {
                        if prefix_eq(snapshot, &cols, row, c as usize, d - 1) {
                            found = Some(c);
                            break;
                        }
                        candidate = (next[c as usize] != NO_PARENT).then(|| next[c as usize]);
                    }
                    match found {
                        Some(p) => parent[row] = p,
                        None => {
                            unplaced += 1;
                            parent[row] = roots.first().copied().unwrap_or(NO_PARENT);
                        }
                    }
                }
                let h = prefix_hash(snapshot, &cols, row, d);
                match tables[d].entry(h) {
                    Entry::Occupied(mut e) => {
                        next[row] = *e.get();
                        *e.get_mut() = r;
                    }
                    Entry::Vacant(v) => {
                        v.insert(r);
                    }
                }
            }
        }
        Self::finish(parent, depth, roots, unplaced)
    }

    fn finish(parent: Vec<u32>, depth: Vec<u8>, roots: Vec<u32>, unplaced: u32) -> TreeIndex {
        let n = parent.len();
        let mut counts = vec![0u32; n];
        for &p in &parent {
            if p != NO_PARENT {
                counts[p as usize] += 1;
            }
        }
        let mut child_start = vec![0u32; n + 1];
        for i in 0..n {
            child_start[i + 1] = child_start[i] + counts[i];
        }
        let mut fill = child_start.clone();
        let mut children = vec![0u32; child_start[n] as usize];
        for (r, &p) in parent.iter().enumerate() {
            if p != NO_PARENT {
                let slot = fill[p as usize];
                children[slot as usize] = r as u32;
                fill[p as usize] += 1;
            }
        }
        TreeIndex {
            parent,
            depth,
            child_start,
            children,
            roots,
            unplaced,
        }
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    pub fn depth(&self, row: usize) -> usize {
        self.depth.get(row).copied().unwrap_or(0) as usize
    }

    pub fn parent(&self, row: usize) -> Option<usize> {
        match self.parent.get(row) {
            Some(&p) if p != NO_PARENT => Some(p as usize),
            _ => None,
        }
    }

    pub fn children(&self, row: usize) -> &[u32] {
        if row + 1 >= self.child_start.len() {
            return &[];
        }
        let (a, b) = (
            self.child_start[row] as usize,
            self.child_start[row + 1] as usize,
        );
        &self.children[a..b]
    }

    pub fn has_children(&self, row: usize) -> bool {
        !self.children(row).is_empty()
    }

    /// Depth-0 rows: the grand total, normally exactly one. A flat
    /// result lists every row.
    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    /// Rows whose parent was not in the result, attached to the first
    /// root instead. Shown in the blotter's footer, never hidden.
    pub fn unplaced(&self) -> usize {
        self.unplaced as usize
    }
}

/// FNV-1a over the first `k` grouping cells of `row`. A NULL cell hashes
/// a token no string or code can produce; an absent column is NULL
/// everywhere. A dictionary column feeds its per-row code, tagged
/// distinctly from a string's bytes so a code and a string can never
/// collide across a row that mixes encodings (spec §5.5).
fn prefix_hash(snapshot: &Snapshot, cols: &[Col], row: usize, k: usize) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    };
    for col in cols.iter().take(k) {
        match col {
            Col::Absent => feed(0x00),
            Col::Dict(codes) => match codes.code(row) {
                Some(code) => {
                    feed(0x02);
                    for b in (code as u64).to_le_bytes() {
                        feed(b);
                    }
                }
                None => feed(0x00),
            },
            Col::Text(idx) => match snapshot.text_at(*idx, row) {
                Some(s) => {
                    feed(0x01);
                    for b in s.bytes() {
                        feed(b);
                    }
                }
                None => feed(0x00),
            },
        }
        feed(0xff);
    }
    h
}

fn prefix_eq(snapshot: &Snapshot, cols: &[Col], a: usize, b: usize, k: usize) -> bool {
    cols.iter().take(k).all(|col| match col {
        Col::Absent => true,
        Col::Dict(codes) => codes.code(a) == codes.code(b),
        Col::Text(idx) => snapshot.text_at(*idx, a) == snapshot.text_at(*idx, b),
    })
}

#[cfg(test)]
mod tests {
    // Every assertion here reaches the index the way a module will —
    // through `Snapshot::tree()` — so nothing from `super` is named.
    use crate::attribution::{Attribution, ScopeSemantics};
    use crate::snapshot::{ColumnMeta, Snapshot, TestColumn};

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 4],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
        }
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    /// lhu > underlying > position, three levels, rows sorted by depth
    /// only. Siblings are deliberately interleaved within a depth: L1's
    /// children are rows 3 and 5, L2's are 4 and 6. This is what a
    /// declared `sort` produces (Phase 3 §5.5).
    fn interleaved(dict: bool) -> Snapshot {
        let lhu = vec![
            None,
            s("L1"),
            s("L2"),
            s("L1"),
            s("L2"),
            s("L1"),
            s("L2"),
            s("L1"),
        ];
        let und = vec![
            None,
            None,
            None,
            s("SPX"),
            s("SPX"),
            s("NDX"),
            s("NDX"),
            s("SPX"),
        ];
        let pos = vec![None, None, None, None, None, None, None, s("P1")];
        let col = |v: Vec<Option<String>>| {
            if dict {
                TestColumn::Dict(v)
            } else {
                TestColumn::Str(
                    v.iter()
                        .map(|x| {
                            x.as_deref().map(|s| -> &'static str {
                                Box::leak(s.to_string().into_boxed_str())
                            })
                        })
                        .collect(),
                )
            }
        };
        Snapshot::for_tests(
            vec![
                (dim("lhu"), col(lhu)),
                (dim("underlying_ref"), col(und)),
                (dim("position_ref"), col(pos)),
                (
                    dim("row_depth"),
                    TestColumn::I32(vec![0, 1, 1, 2, 2, 2, 2, 3]),
                ),
                (
                    dim("delta01"),
                    TestColumn::F64((0..8).map(|i| Some(i as f64)).collect()),
                ),
            ],
            3,
        )
    }

    #[test]
    fn children_are_found_by_prefix_not_by_contiguity() {
        for dict in [true, false] {
            let snap = interleaved(dict);
            let t = snap.tree();
            assert_eq!(t.len(), 8);
            assert_eq!(t.roots(), &[0], "dict={dict}");
            assert_eq!(t.children(0), &[1, 2], "dict={dict}");
            assert_eq!(t.children(1), &[3, 5], "L1's underlyings, dict={dict}");
            assert_eq!(t.children(2), &[4, 6], "L2's underlyings, dict={dict}");
            assert_eq!(t.children(3), &[7], "L1/SPX's position, dict={dict}");
            assert!(t.children(4).is_empty());
            assert_eq!(t.parent(7), Some(3));
            assert_eq!(t.parent(0), None);
            assert_eq!(t.depth(7), 3);
            assert!(t.has_children(1) && !t.has_children(7));
            assert_eq!(t.unplaced(), 0);
        }
    }

    #[test]
    fn children_keep_row_order_so_a_declared_sort_is_the_default_sibling_order() {
        // Rows 5 (NDX) precedes nothing here, but if the compiler's sort
        // put NDX before SPX the CSR would list 5 before 3. Row order in,
        // row order out — the index imposes none of its own.
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, Some("L1"), Some("L1"), Some("L1")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX"), Some("NDX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2, 2])),
            ],
            2,
        );
        assert_eq!(snap.tree().children(1), &[2, 3]);
    }

    #[test]
    fn a_row_whose_parent_is_missing_attaches_to_the_root_and_is_counted() {
        // A stale ENUM blanks a value the row carries at a finer level
        // (P2 §3.6): the child's prefix names an lhu no depth-1 row has.
        // Silently dropping it would hide a real position; hiding the
        // count would hide that anything went wrong.
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, Some("L1"), Some("GHOST")]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 2])),
            ],
            2,
        );
        let t = snap.tree();
        assert_eq!(t.parent(2), Some(0), "attached to the grand total");
        assert_eq!(t.children(0), &[1, 2]);
        assert_eq!(t.unplaced(), 1);
    }

    #[test]
    fn null_is_its_own_token_distinct_from_an_empty_string() {
        // A depth-1 row with a NULL lhu (the blanked-ENUM case) and one
        // with "" are different parents, and a child with NULL lhu finds
        // the NULL one.
        let snap = Snapshot::for_tests(
            vec![
                (
                    dim("lhu"),
                    TestColumn::Str(vec![None, None, Some(""), None]),
                ),
                (
                    dim("underlying_ref"),
                    TestColumn::Str(vec![None, None, None, Some("SPX")]),
                ),
                (dim("row_depth"), TestColumn::I32(vec![0, 1, 1, 2])),
            ],
            2,
        );
        let t = snap.tree();
        assert_eq!(t.parent(3), Some(1));
        assert!(t.children(2).is_empty());
        assert_eq!(t.unplaced(), 0);
    }

    #[test]
    fn a_result_without_a_depth_column_is_flat() {
        let snap = Snapshot::for_tests(
            vec![(dim("delta01"), TestColumn::F64(vec![Some(1.0), Some(2.0)]))],
            0,
        );
        let t = snap.tree();
        assert_eq!(t.roots(), &[0, 1]);
        assert!(t.children(0).is_empty());
        assert_eq!(t.unplaced(), 0);
    }

    #[test]
    fn an_empty_result_has_an_empty_tree() {
        let snap = Snapshot::for_tests(
            vec![
                (dim("lhu"), TestColumn::Str(vec![])),
                (dim("row_depth"), TestColumn::I32(vec![])),
            ],
            1,
        );
        assert!(snap.tree().is_empty());
        assert!(snap.tree().roots().is_empty());
        assert!(
            snap.tree().children(0).is_empty(),
            "out of range is empty, not a panic"
        );
        assert_eq!(snap.tree().parent(0), None);
    }

    #[test]
    fn a_grouping_column_absent_from_the_batch_still_builds() {
        // Below the depth bound the compiler emits a NULL constant for
        // every unmaterialised grouping column, so it is present. But a
        // fixture, or a future compiler, might omit it; the index treats
        // an absent column as NULL for every row rather than panicking.
        let snap = Snapshot::from_batches(
            {
                use arrow::array::{Int32Array, StringArray};
                use arrow::datatypes::{DataType, Field, Schema};
                use arrow::record_batch::RecordBatch;
                use std::sync::Arc;
                let schema = Arc::new(Schema::new(vec![
                    Field::new("lhu", DataType::Utf8, true),
                    Field::new("row_depth", DataType::Int32, true),
                ]));
                vec![
                    RecordBatch::try_new(
                        schema,
                        vec![
                            Arc::new(StringArray::from(vec![None, Some("L1")])),
                            Arc::new(Int32Array::from(vec![0, 1])),
                        ],
                    )
                    .unwrap(),
                ]
            },
            vec![dim("lhu"), dim("row_depth")],
            vec!["lhu".into(), "underlying_ref".into()],
            crate::snapshot::Provenance::default(),
        )
        .unwrap();
        assert_eq!(snap.tree().children(0), &[1]);
    }
}
