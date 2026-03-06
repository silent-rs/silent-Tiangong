use super::*;

mod load;
mod locks;
mod persist;
mod utils;

pub(in crate::core::app_state) use utils::*;

#[derive(Debug)]
pub(super) struct AppRepository {
    paths: AppPaths,
}

impl AppRepository {
    pub(super) fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    pub(super) fn paths(&self) -> &AppPaths {
        &self.paths
    }
}
