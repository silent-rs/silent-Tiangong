use super::*;

mod load;
mod locks;
mod persist;
mod utils;

pub(in crate::core::app_state) use utils::*;

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
