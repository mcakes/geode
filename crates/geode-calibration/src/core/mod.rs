//! The tile's pure core, GPUI-free: [`layout`] places a pipeline's stages
//! as a layered DAG in design pixels, and [`model`] turns the pipeline,
//! its results and the tile's cursor and filter into the prepared render
//! model paint reads without formatting. Both run once per change, never
//! per frame.

pub mod layout;
pub mod menu;
pub mod model;

#[cfg(test)]
pub(crate) mod testing {
    use geode_core::calibration::{BUILTIN_CALIBRATION, CALIBRATION_DOC, Pipeline, from_doc};
    use geode_core::config::{LayerDoc, merge_docs};

    /// The builtin `implied_forwards` pipeline: eleven stages in document
    /// order.
    pub(crate) fn builtin_pipeline() -> Pipeline {
        let doc = merge_docs(
            CALIBRATION_DOC,
            &[LayerDoc::builtin(CALIBRATION_DOC, BUILTIN_CALIBRATION).expect("well-formed TOML")],
        );
        let (pipelines, diags) = from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        pipelines
            .pipelines
            .into_iter()
            .next()
            .expect("one pipeline")
    }
}
