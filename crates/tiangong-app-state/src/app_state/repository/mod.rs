use super::*;

mod load;
mod persist;
pub(crate) mod utils;

/// storage_root 是 storage 路径的唯一对外来源（路径计算归 app 层）。
pub(in crate::app_state) use utils::*;

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
