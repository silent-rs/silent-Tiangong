use super::*;

mod load;
mod persist;
pub(crate) mod utils;

pub(in crate::app_state) use utils::*;
// Skills 存储路径供 runtime_env / skill plugin 跨 crate 使用。
// mcp-lock 同步已迁至 tiangong-plugin-skill（Skill↔MCP 依赖属于 plugin 域）。
pub use utils::default_skills_storage_dir_path;

#[derive(Debug)]
pub struct AppRepository {
    paths: AppPaths,
}

impl AppRepository {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }
}
