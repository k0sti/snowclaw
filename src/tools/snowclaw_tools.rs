//! Snowclaw-specific tool registrations.
//!
//! Extracted from `mod.rs` to minimize upstream diff. New tools added by
//! the Snowclaw fork are registered here.

use crate::security::SecurityPolicy;
use crate::tools::{AgentLessonTool, NostrTaskTool, SocialSearchTool, Tool};
use std::path::Path;
use std::sync::Arc;

/// Create Snowclaw-specific tools and append them to the tools vector.
pub(crate) fn register_snowclaw_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    security: &Arc<SecurityPolicy>,
    workspace_dir: &Path,
    config_dir: &Path,
) {
    tools.push(Arc::new(NostrTaskTool::new(
        security.clone(),
        workspace_dir,
    )));
    tools.push(Arc::new(SocialSearchTool::new(config_dir)));
    tools.push(Arc::new(AgentLessonTool::new(config_dir)));
}
