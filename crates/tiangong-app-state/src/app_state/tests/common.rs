use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;

use super::super::*;

pub(super) struct TestEnvPaths {
    pub(super) temp_root: PathBuf,
    pub(super) fake_home: PathBuf,
    pub(super) workspace: PathBuf,
}

struct TestEnvGuard {
    previous_home: Option<OsString>,
    previous_cwd: PathBuf,
    temp_root: PathBuf,
}

impl TestEnvGuard {
    fn setup(paths: &TestEnvPaths) -> Result<Self> {
        fs::create_dir_all(&paths.fake_home)?;
        fs::create_dir_all(&paths.workspace)?;

        let previous_home = std::env::var_os("HOME");
        let previous_cwd = std::env::current_dir()?;
        // SAFETY: tests are serialized by a process-wide mutex.
        unsafe { std::env::set_var("HOME", &paths.fake_home) };
        std::env::set_current_dir(&paths.workspace)?;

        Ok(Self {
            previous_home,
            previous_cwd,
            temp_root: paths.temp_root.clone(),
        })
    }
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous_cwd);
        match self.previous_home.take() {
            Some(home) => {
                // SAFETY: tests are serialized by a process-wide mutex.
                unsafe { std::env::set_var("HOME", home) };
            }
            None => {
                // SAFETY: tests are serialized by a process-wide mutex.
                unsafe { std::env::remove_var("HOME") };
            }
        }
        let _ = fs::remove_dir_all(&self.temp_root);
    }
}

fn test_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) fn with_isolated_state<F>(prefix: &str, test: F) -> Result<()>
where
    F: FnOnce(&TestEnvPaths, &mut TiangongState) -> Result<()>,
{
    let _lock = test_env_lock().lock().expect("获取测试环境锁失败");
    let nonce = scru128::new().to_string();
    let temp_root = std::env::temp_dir().join(format!("{prefix}-{nonce}"));
    let paths = TestEnvPaths {
        fake_home: temp_root.join("home"),
        workspace: temp_root.join("workspace"),
        temp_root,
    };
    let _guard = TestEnvGuard::setup(&paths)?;
    // models_config 已归 config registry(issue #245),测试需 init。
    let storage = paths.fake_home.join(".tiangong");
    std::fs::create_dir_all(&storage).ok();
    tiangong_config::registry::init_from_dir(&storage);
    let mut state = TiangongState::load_or_default();
    test(&paths, &mut state)
}
