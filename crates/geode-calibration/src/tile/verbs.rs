//! The run and publish verbs and the y/n bar they share with the delete
//! verb. `r` runs from the cursor's stage and `shift+r` the whole pipeline,
//! each over the documents in hand under the tile's run counter. `p` asks
//! `Publish {series} for {underlying} to {target}? y/n` on the shared
//! confirm bar; `y` submits `RunAction::Publish { from: Filter }` on the
//! user lane, whose answer carries one document per rasterized series, and
//! each goes to the target as one upload under the tile's upload counter.
//!
//! A publish is refused, with the object named, while the pipeline has no
//! publish stage, while that stage's last result is `Held` (its reason),
//! and while a publish is in flight: the user lane is never replaceable
//! in the worker, so a second one would queue behind the first and run
//! twice. The checks run again at `y`, since an answer may land between
//! the ask and the answer.
//!
//! A question is armed over one underlying and one publish stage: the
//! payload carries both, `y` compares them with what the tile holds then,
//! and a flip of underlying or pipeline withdraws the question outright
//! (`data::clear_loaded`), so `y` can never publish for an underlying the
//! question did not name.
//!
//! Refusals are a warning notice in the header (replacing the last verb's,
//! cleared when a verb goes out); upload answers are status and danger
//! notices kept until the next publish. An upload never rewrites a stage's
//! result (only an outcome does, so a held publish stays held and keeps
//! refusing `p`): the tile counts the uploads of the last publish and,
//! once every one landed, hands the time to the model, which trails it on
//! the publish card after the engine's words.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use geode_core::calibration::{
    Pipeline, PublishSource, RunAction, Stage, StageKind, StageName, StageStatus,
};
use geode_core::document::DocumentRows;
use geode_data::UploadParams;
use geode_shell::module::UploadDelivery;
use geode_tile::confirm::{self, Confirm, ConfirmHost};
use geode_tile::notice::Notice;
use gpui::{App, Context, SharedString, Window};

use super::CalibrationTile;
use super::data::{QUEUE_FULL, no_underlying};
use crate::core::menu::{NO_CURSOR, NO_PIPELINE};

/// What an armed y/n asks. Public only because it is the public
/// `CalibrationTile`'s `ConfirmHost` payload; its fields stay private.
pub enum Pending {
    /// `Publish {series} for {underlying} to {target}?`: the underlying,
    /// document and target the question named, each checked again at `y`.
    Publish {
        target: String,
        document: String,
        underlying: String,
    },
    /// `Delete {stage} and its {n} edges?`; armed by the delete verb.
    #[allow(dead_code, reason = "the delete verb arms it")]
    Delete { stage: StageName, edges: usize },
}

/// The publish out now: where its rows go when the answer comes.
pub(super) struct Publishing {
    target: String,
    document: String,
}

/// An upload out now: what its answer names.
pub(super) struct UploadSent {
    document: String,
    key: String,
}

/// The uploads out, by tag; an answer under no tag here is stale or
/// another tile's.
pub(super) type Uploads = BTreeMap<u64, UploadSent>;

/// The last publish's uploads: how many documents it had to send and how
/// each answered. `uploaded_at` is set only when every one succeeded; a
/// failure or a refusal keeps it unset, so the card never says `uploaded`
/// over a document that did not land.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Uploading {
    expected: usize,
    succeeded: usize,
    failed: usize,
}

impl Uploading {
    fn complete(&self) -> bool {
        self.expected > 0 && self.failed == 0 && self.succeeded == self.expected
    }
}

/// What `p` would do now.
struct PublishPlan {
    target: String,
    document: String,
    /// The series the publish sends, in port order.
    series: Vec<String>,
    underlying: String,
}

/// The parts of a document key, joined for a notice.
const KEY_DISPLAY_SEPARATOR: &str = "/";

fn display_key(key: &[String]) -> String {
    key.join(KEY_DISPLAY_SEPARATOR)
}

/// The series `publish` sends: for each rasterize input, the series of
/// the fit behind it (the nearest `kalman_fit` upstream, port order first),
/// else the rasterize stage's own name.
pub(crate) fn publish_series(pipeline: &Pipeline, publish: &Stage) -> Vec<String> {
    publish
        .inputs
        .iter()
        .filter_map(|input| {
            let stage = pipeline.stage(input)?;
            if !matches!(stage.kind, StageKind::Rasterize { .. }) {
                return None;
            }
            Some(series_behind(pipeline, stage).unwrap_or_else(|| stage.name.clone()))
        })
        .collect()
}

fn series_behind(pipeline: &Pipeline, stage: &Stage) -> Option<String> {
    let mut queue: VecDeque<&str> = stage.inputs.iter().map(String::as_str).collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name) {
            continue;
        }
        let Some(s) = pipeline.stage(name) else {
            continue;
        };
        if let StageKind::KalmanFit { series, .. } = &s.kind {
            return Some(series.clone());
        }
        queue.extend(s.inputs.iter().map(String::as_str));
    }
    None
}

/// The pipeline's publish stage with its document and target.
fn publish_stage(pipeline: &Pipeline) -> Option<(&Stage, &str, &str)> {
    pipeline.stages.iter().find_map(|s| match &s.kind {
        StageKind::Publish { document, target } => Some((s, document.as_str(), target.as_str())),
        _ => None,
    })
}

impl CalibrationTile {
    /// A refusal: one warning notice, replacing the last verb's. `escape`
    /// dismisses it; the next verb that goes out clears it.
    pub(super) fn refuse(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.verb_notice = Some(Notice::warning(text));
        cx.notify();
    }

    /// Whether a run or publish may go out now: an underlying is loaded
    /// and every source is in. The refusal names what is missing in the
    /// footer's words.
    fn ready(&self, pipeline: &Pipeline, cx: &App) -> Result<(), String> {
        if self.loaded_for.is_none() {
            return Err(no_underlying(self.frame.read(cx).following()));
        }
        if !self.sources.pending.is_empty() {
            let waiting: Vec<&str> = pipeline
                .stages
                .iter()
                .map(|s| s.name.as_str())
                .filter(|name| self.sources.pending.contains(*name))
                .collect();
            return Err(format!("waiting for {}", waiting.join(", ")));
        }
        Ok(())
    }

    /// `r`: run from the cursor's stage; with none, the refusal says so.
    pub(super) fn run_from_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(from) = self.cursor.clone() else {
            self.refuse(NO_CURSOR, cx);
            return;
        };
        if let Err(e) = self.run_verb(Some(from), cx) {
            self.refuse(e, cx);
        }
    }

    /// `shift+r`, `:run [stage]` and `r` behind its cursor check: submit a
    /// run from `from` (every stage with `None`) over the documents in
    /// hand. `Err` with the reason when nothing went out: no pipeline, an
    /// unknown stage, no underlying, a source still owed, or the data
    /// handle's queue full.
    pub(super) fn run_verb(
        &mut self,
        from: Option<StageName>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let pipeline = self.pipeline().ok_or(NO_PIPELINE)?;
        if let Some(stage) = &from
            && pipeline.stage(stage).is_none()
        {
            return Err(format!("unknown stage '{stage}'"));
        }
        self.ready(&pipeline, cx)?;
        if self.submit_run(from, cx) {
            self.verb_notice = None;
            cx.notify();
            Ok(())
        } else {
            // The submission's own notice is the verb's answer here.
            self.run_notice = None;
            Err(QUEUE_FULL.into())
        }
    }

    /// What `p` would do now, or the refusal it would give. Also the
    /// menu's `Publish` row, so the row and the key say the same words.
    pub(super) fn publish_check(&self, cx: &App) -> Result<(), String> {
        self.publish_plan(cx).map(|_| ())
    }

    fn publish_plan(&self, cx: &App) -> Result<PublishPlan, String> {
        let pipeline = self.pipeline().ok_or(NO_PIPELINE)?;
        let (publish, document, target) = publish_stage(&pipeline)
            .ok_or_else(|| format!("no publish stage in {}", pipeline.name))?;
        if let Some(StageStatus::Held(reason)) = self.results.get(&publish.name).map(|r| &r.status)
        {
            return Err(format!("publish is held: {reason}"));
        }
        if self.publishing.is_some() {
            return Err("publish already in flight".into());
        }
        self.ready(&pipeline, cx)?;
        let mut series = publish_series(&pipeline, publish);
        // A publish stage fed by no rasterize: the document it writes is
        // the only name there is.
        if series.is_empty() {
            series.push(document.to_string());
        }
        Ok(PublishPlan {
            target: target.to_string(),
            document: document.to_string(),
            series,
            underlying: self.loaded_for.clone().unwrap_or_default(),
        })
    }

    /// `p` and `:publish`: ask on the bar, or refuse with the reason.
    pub(super) fn publish_verb(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let plan = self.publish_plan(cx)?;
        self.verb_notice = None;
        let prompt = format!(
            "Publish {} for {} to {}? y/n",
            plan.series.join(", "),
            plan.underlying,
            plan.target
        );
        confirm::arm(
            self,
            Pending::Publish {
                target: plan.target,
                document: plan.document,
                underlying: plan.underlying,
            },
            prompt,
            window,
            cx,
        );
        cx.notify();
        Ok(())
    }

    /// `y`: the checks run again, and the publish must still send
    /// `document` to `target` for `underlying` (a reload or a flip under
    /// the question may have changed one; a flip also withdraws the
    /// question, so this is the second line); then the publish goes out
    /// on the user lane under the next tag, remembered as the publish in
    /// flight. The last publish's upload notices and count leave with it.
    fn submit_publish(
        &mut self,
        target: String,
        document: String,
        underlying: String,
        cx: &mut Context<Self>,
    ) {
        let plan = match self.publish_plan(cx) {
            Ok(plan) => plan,
            Err(reason) => {
                self.refuse(reason, cx);
                return;
            }
        };
        if plan.underlying != underlying {
            self.refuse(
                format!(
                    "the question named {underlying}; the tile now shows {}: ask again",
                    plan.underlying
                ),
                cx,
            );
            return;
        }
        if plan.target != target || plan.document != document {
            self.refuse(
                format!(
                    "publish now sends {} to {}: ask again",
                    plan.document, plan.target
                ),
                cx,
            );
            return;
        }
        let Some(pipeline) = self.pipeline() else {
            return;
        };
        let request = self.request(
            pipeline,
            plan.underlying,
            RunAction::Publish {
                from: PublishSource::Filter,
            },
        );
        let tag = request.tag;
        match self.shared.data.calibrate(request) {
            Ok(()) => {
                self.last_user_tag = Some(tag);
                self.publishing = Some(Publishing {
                    target: plan.target,
                    document: plan.document,
                });
                self.upload_notices.clear();
                self.uploading = Uploading::default();
                self.verb_notice = None;
            }
            Err(_) => self.refuse(QUEUE_FULL, cx),
        }
        cx.notify();
    }

    /// The publish's answer landed (its stages already merged): one
    /// upload per document to the target the publish was armed for, each
    /// under the next upload tag. A refused upload is a notice at once.
    pub(super) fn publish_answered(
        &mut self,
        rows: Option<Vec<DocumentRows>>,
        cx: &mut Context<Self>,
    ) {
        let Some(Publishing { target, document }) = self.publishing.take() else {
            return;
        };
        let rows = rows.unwrap_or_default();
        self.uploading = Uploading {
            expected: rows.len(),
            ..Uploading::default()
        };
        for rows in rows {
            self.upload_tag += 1;
            let tag = self.upload_tag;
            let key = display_key(&rows.key);
            let queued = self.shared.data.upload(UploadParams {
                key: self.key(),
                tag,
                target: target.clone(),
                document: document.clone(),
                rows,
            });
            match queued {
                Ok(()) => {
                    self.uploads.insert(
                        tag,
                        UploadSent {
                            document: document.clone(),
                            key,
                        },
                    );
                }
                Err(refusal) => {
                    self.uploading.failed += 1;
                    self.upload_notices.push(Notice::danger(format!(
                        "upload {document} {key} to {target} refused: {refusal}"
                    )));
                }
            }
        }
        cx.notify();
    }

    /// An upload's answer: a status notice naming the document, its key,
    /// the target and the time, and, once every document of the publish
    /// has landed, that time trails the publish card's status; or a danger
    /// notice with what failed, which keeps the time off the card. The
    /// engine's result is never touched: only an outcome changes a stage's
    /// result, so a publish held since is still held. An answer under a tag
    /// not out, or another tile's, is dropped.
    pub fn deliver_upload(&mut self, u: UploadDelivery, cx: &mut Context<Self>) {
        if u.key != self.key() {
            return;
        }
        let Some(sent) = self.uploads.remove(&u.tag) else {
            return;
        };
        match u.result {
            Ok(()) => {
                let now: DateTime<Utc> = Utc::now();
                let time = self.clock(cx).hms(now);
                self.upload_notices.push(Notice::status(format!(
                    "uploaded {} {} to {} {time}",
                    sent.document, sent.key, u.target
                )));
                self.uploading.succeeded += 1;
                if self.uploading.complete() {
                    self.uploaded_at = Some(now);
                    self.rebuild_model(cx);
                }
            }
            Err(e) => {
                self.uploading.failed += 1;
                self.upload_notices.push(Notice::danger(format!(
                    "upload {} {} to {} failed: {e}",
                    sent.document, sent.key, u.target
                )));
            }
        }
        cx.notify();
    }

    /// The armed question's text, `None` with none armed.
    #[cfg(test)]
    pub(crate) fn confirm_text(&self) -> Option<String> {
        self.confirm.as_ref().map(|c| c.prompt_text().to_string())
    }
}

impl ConfirmHost for CalibrationTile {
    type Payload = Pending;

    fn confirm_slot(&mut self) -> &mut Option<Confirm<Pending>> {
        &mut self.confirm
    }

    fn confirmed(&mut self, pending: Pending, _window: &mut Window, cx: &mut Context<Self>) {
        match pending {
            Pending::Publish {
                target,
                document,
                underlying,
            } => self.submit_publish(target, document, underlying, cx),
            // The delete verb arrives with the editors.
            Pending::Delete { .. } => {}
        }
        cx.notify();
    }

    /// Any other answer: the question leaves and nothing else changes.
    fn cancelled(&mut self, _pending: Pending, _window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::builtin_pipeline;

    /// The builtin publish reads two rasters, each behind one fit: the
    /// fits' series, in port order. Its check input names no series.
    #[test]
    fn publish_series_names_the_fits_behind_the_rasters() {
        let pipeline = builtin_pipeline();
        let (publish, document, target) = publish_stage(&pipeline).expect("a publish stage");
        assert_eq!((document, target), ("rate_curve", "sophis"));
        assert_eq!(publish_series(&pipeline, publish), ["repo", "box"]);
        // A raster fed by no fit names itself.
        let mut bare = pipeline.clone();
        let raster = bare
            .stages
            .iter()
            .position(|s| s.name == "repo_curve")
            .unwrap();
        bare.stages[raster].inputs = vec!["implied_rates".into()];
        let publish = publish_stage(&bare).unwrap().0;
        assert_eq!(publish_series(&bare, publish), ["repo_curve", "box"]);
    }
}
