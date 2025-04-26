use std::sync::Arc;

use sp_core::dependency::ResolvedGraph;
use sp_core::model::formula::Formula;

pub mod bottle;
pub mod source;

/// Info needed by both bottle & source installers
#[derive(Clone)]
pub struct FormulaInstallInfo {
    pub formula: Arc<Formula>,
    pub resolved_graph: Arc<ResolvedGraph>,
}
