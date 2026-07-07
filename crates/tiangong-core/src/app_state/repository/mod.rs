use super::*;

mod load;
mod persist;
pub(crate) mod utils;

pub(in crate::app_state) use utils::*;
// Skill 存储路径已迁至 tiangong-plugin-skill::paths（Skill 领域完全脱离 core）。

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
