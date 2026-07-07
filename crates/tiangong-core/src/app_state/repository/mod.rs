use super::*;

mod load;
mod persist;
pub(crate) mod utils;

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
