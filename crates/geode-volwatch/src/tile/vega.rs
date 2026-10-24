//! Book vega: one ordinary grouped query over the configured view, scoped
//! to the frame scope narrowed to the members, joined by name into the
//! rows. It rides under the tile's key but never with the board's
//! request, and a refusal of it never touches the board: the column drops
//! and a standing warning names the setting until an answer lands.

use std::collections::HashMap;
use std::time::Instant;

use geode_core::link::UNDERLYING;
use geode_core::query::{AsOf, QueryOutcome};
use geode_core::scope::{DimensionSelection, Scope};
use geode_core::snapshot::Snapshot;
use geode_data::QueryParams;
use gpui::Context;

use super::VolwatchTile;
use crate::content::VegaSetting;

/// The vega query's state; its tag is its own, apart from the board's.
#[derive(Default)]
pub(crate) struct VegaState {
    pub(crate) setting: Option<VegaSetting>,
    /// The latest admitted submission's tag; an answer under any other is
    /// stale.
    pub(crate) tag: u64,
    /// Book vega by underlying, from the last good answer.
    pub(crate) values: Option<HashMap<String, f64>>,
    /// Why the view was refused; the column is absent while this stands.
    pub(crate) refused: Option<String>,
    /// The frame scope and as-of the last submission read, so a frame
    /// change that moves neither asks nothing.
    pub(crate) read: Option<(Scope, AsOf)>,
}

/// The scope layer naming exactly `members`.
pub(crate) fn members_layer(members: &[String]) -> Scope {
    Scope {
        dimensions: vec![DimensionSelection {
            column: UNDERLYING.into(),
            values: members.to_vec(),
        }],
        ..Default::default()
    }
}

/// Book vega by underlying from a grouped answer: the depth-1 rows only,
/// never the depth-0 total.
pub(crate) fn join(snapshot: &Snapshot, column: &str) -> Result<HashMap<String, f64>, String> {
    if snapshot.column_index(column).is_none() {
        return Err(format!("no column '{column}'"));
    }
    if snapshot.column_index(UNDERLYING).is_none() {
        return Err(format!("no column '{UNDERLYING}'"));
    }
    let mut out = HashMap::new();
    for row in 0..snapshot.rows() {
        if snapshot.depth_of_row(row) != Some(1) {
            continue;
        }
        if let (Some(name), Some(v)) = (
            snapshot.text_value(UNDERLYING, row),
            snapshot.f64_value(column, row),
        ) {
            out.insert(name.to_string(), v);
        }
    }
    Ok(out)
}

impl VolwatchTile {
    /// Whether the vega column is offered: configured and not refused.
    pub(crate) fn vega_offered(&self) -> bool {
        self.vega.setting.is_some() && self.vega.refused.is_none()
    }

    /// Ask for book vega over the members. A refusal (frame scope that
    /// cannot resolve, or a bounded queue) drops the column; nothing
    /// retries by itself.
    pub(crate) fn submit_vega(&mut self, cx: &mut Context<Self>) {
        let Some(setting) = self.vega.setting.clone() else {
            return;
        };
        if self.members.is_empty() {
            return;
        }
        let (scope, as_of) = {
            let frame = self.frame.read(cx);
            (
                frame.effective_scope(&members_layer(&self.members)),
                frame.as_of().clone(),
            )
        };
        let scope = match scope {
            Ok(scope) => scope,
            Err(e) => {
                // Anything out answers a scope that no longer holds.
                self.vega.tag += 1;
                self.refuse_vega(e, cx);
                return;
            }
        };
        self.vega.read = Some((scope.clone(), as_of.clone()));
        let tag = self.vega.tag + 1;
        let params = QueryParams {
            key: self.key(),
            tag,
            submitted: Instant::now(),
            view: setting.view,
            grouping: Some(vec![UNDERLYING.into()]),
            scope,
            as_of,
            max_depth: 1,
        };
        match self.data.query(params) {
            Ok(()) => self.vega.tag = tag,
            Err(r) => self.refuse_vega(r.to_string(), cx),
        }
    }

    /// The frame moved: the grouping is re-read first (a pinned tile's
    /// request does not change, so it regroups nothing), then vega asks
    /// again when its scope or as-of moved.
    pub(crate) fn on_frame_changed(&mut self, cx: &mut Context<Self>) {
        self.regroup(false, cx);
        if self.vega.setting.is_none() || self.members.is_empty() {
            return;
        }
        let frame = self.frame.read(cx);
        let unchanged = self.vega.read.as_ref().is_some_and(|(scope, as_of)| {
            frame.as_of() == as_of
                && frame
                    .effective_scope(&members_layer(&self.members))
                    .as_ref()
                    == Ok(scope)
        });
        if !unchanged {
            self.submit_vega(cx);
        }
    }

    /// The vega answer lands: the join, or the view's refusal. A tag
    /// other than the one out is superseded and dropped.
    pub(crate) fn deliver_vega(&mut self, outcome: QueryOutcome, cx: &mut Context<Self>) {
        if outcome.key != self.key() || outcome.tag != self.vega.tag {
            return;
        }
        let Some(setting) = self.vega.setting.clone() else {
            return;
        };
        match outcome
            .snapshot
            .and_then(|snapshot| join(&snapshot, &setting.column))
        {
            Ok(values) => {
                self.vega.values = Some(values);
                self.vega.refused = None;
                self.rebuild_columns();
                self.rebuild_rows(cx);
                self.rebuild_chrome(cx);
                cx.notify();
            }
            Err(e) => self.refuse_vega(e, cx),
        }
    }

    /// Drop the column and stand the warning.
    fn refuse_vega(&mut self, text: String, cx: &mut Context<Self>) {
        self.vega.values = None;
        self.vega.refused = Some(text);
        self.rebuild_columns();
        self.rebuild_rows(cx);
        self.rebuild_chrome(cx);
        cx.notify();
    }

    /// Whatever vega is out answers a list or setting that is gone.
    pub(crate) fn forget_vega(&mut self) {
        self.vega.tag += 1;
        self.vega.values = None;
        self.vega.refused = None;
        self.vega.read = None;
    }
}
