use super::*;

mod load;
mod locks;
mod persist;
pub(crate) mod utils;

pub(in crate::app_state) use utils::*;
// Skills 存储路径供 runtime_env / skill plugin 跨 crate 使用
pub use utils::{default_mcp_lock_path, default_skills_storage_dir_path};

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
