//! 解释器（node/python）运行环境探测与入口注入。
//!
//! GUI 进程（launchd/Finder 启动）不执行 shell 初始化，nvm/Homebrew 等
//! 安装位置不在其 PATH 中：本模块提供 PATH 之外的分层探测（安装工具
//! 声明的根目录 → 版本管理器目录从新到旧 → 系统标准位置），以及进程
//! 入口的 [`ensure_interpreter_env`] 环境注入，供插件 sidecar 与命令
//! 通道整棵子进程树使用。
//!
//! 探测结果只由传入的 [`InterpreterEnv`] 快照和文件系统状态决定：
//! 某个来源缺失就跳过继续尝试后续层次，全部未命中才判定解释器不可用。

use std::path::{Path, PathBuf};

use crate::manifest::SidecarRuntime;

/// 跨平台获取用户 home 目录（与 sidecar 框架 `endpoint::home_dir` 同源）。
pub(crate) fn user_home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = PathBuf::from(drive);
            buf.push(path);
            Some(buf)
        }
        _ => None,
    }
}

/// 解释器探测所需的环境快照：生产路径经 [`InterpreterEnv::from_process`]
/// 从进程环境读取一次，测试注入伪造值，不修改真实环境变量。
#[derive(Default)]
pub(crate) struct InterpreterEnv {
    /// `PATH` 拆分后的搜索目录。
    search_paths: Vec<PathBuf>,
    home: Option<PathBuf>,
    /// 各安装工具声明的自定义根目录（未设置时按各工具默认位置推导）。
    nvm_dir: Option<PathBuf>,
    nvm_home: Option<PathBuf>,
    nvm_symlink: Option<PathBuf>,
    volta_home: Option<PathBuf>,
    scoop: Option<PathBuf>,
    chocolatey_install: Option<PathBuf>,
    pyenv_root: Option<PathBuf>,
    asdf_data_dir: Option<PathBuf>,
    appdata: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
    program_files: Option<PathBuf>,
    program_data: Option<PathBuf>,
    /// 各平台的解释器系统标准安装位置（Homebrew、系统包管理等）。
    node_system_locations: Vec<PathBuf>,
    python_system_locations: Vec<PathBuf>,
}

impl InterpreterEnv {
    pub(crate) fn from_process() -> Self {
        let dir = |key: &str| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            search_paths: std::env::var_os("PATH")
                .map(|value| std::env::split_paths(&value).collect())
                .unwrap_or_default(),
            home: user_home_dir(),
            nvm_dir: dir("NVM_DIR"),
            nvm_home: dir("NVM_HOME"),
            nvm_symlink: dir("NVM_SYMLINK"),
            volta_home: dir("VOLTA_HOME"),
            scoop: dir("SCOOP"),
            chocolatey_install: dir("ChocolateyInstall"),
            pyenv_root: dir("PYENV_ROOT"),
            asdf_data_dir: dir("ASDF_DATA_DIR"),
            appdata: dir("APPDATA"),
            local_appdata: dir("LOCALAPPDATA"),
            program_files: dir("ProgramFiles"),
            program_data: dir("ProgramData"),
            node_system_locations: default_node_system_locations(),
            python_system_locations: default_python_system_locations(),
        }
    }
}

/// 显式环境变量存在时直接使用；仅显式变量缺失时经基准目录推导默认
/// 安装根（`base/<default_suffix>`）。
fn explicit_or_default_root(
    explicit: Option<&PathBuf>,
    base: Option<&Path>,
    default_suffix: &str,
) -> Option<PathBuf> {
    explicit
        .cloned()
        .or_else(|| base.map(|base| base.join(default_suffix)))
}

/// PATH 及常见安装位置中查找解释器（不含环境变量覆盖分支）。
pub(crate) fn search_interpreter_program(
    runtime: SidecarRuntime,
    env: &InterpreterEnv,
) -> Option<PathBuf> {
    let candidates: &[&str] = match runtime {
        SidecarRuntime::Native => return None,
        SidecarRuntime::Node => {
            if cfg!(windows) {
                &["node.exe"]
            } else {
                &["node"]
            }
        }
        SidecarRuntime::Python => {
            if cfg!(windows) {
                &["python.exe", "py.exe"]
            } else {
                &["python3", "python"]
            }
        }
    };
    for directory in &env.search_paths {
        for candidate in candidates {
            let path = directory.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    common_interpreter_locations(runtime, env)
        .into_iter()
        .find(|path| path.is_file())
}

/// 进程入口最早调用（任何后台线程启动前）：探测解释器并注入进程环境，
/// 使插件 sidecar 与命令通道整棵子进程树可见。
///
/// - `TIANGONG_NODE_PATH` / `TIANGONG_PYTHON_PATH` 未设置时写入探测结果；
///   外部显式指定（开发调试、CI）不覆盖，路径无效时跳过留待运行时
///   fail-loud 报错；
/// - 无论显式还是探测所得，均把解释器所在目录前置进 PATH（已包含则
///   跳过），sidecar 派生的 node/yarn/npx 等命令通道子进程直接可用；
/// - 未探测到时不做任何改动，留待运行时报错引导安装；
/// - `std::env::set_var` 与并发读存在数据竞争，本函数只允许在 main
///   最开头、线程池启动前调用。
pub fn ensure_interpreter_env() {
    let snapshot = InterpreterEnv::from_process();
    for (runtime, env_key) in [
        (SidecarRuntime::Node, "TIANGONG_NODE_PATH"),
        (SidecarRuntime::Python, "TIANGONG_PYTHON_PATH"),
    ] {
        let explicit = std::env::var_os(env_key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let program = match explicit {
            Some(path) if path.is_file() => path,
            Some(_) => continue,
            None => match search_interpreter_program(runtime, &snapshot) {
                Some(discovered) => {
                    unsafe { std::env::set_var(env_key, &discovered) };
                    discovered
                }
                None => continue,
            },
        };
        if let Some(bin_dir) = program.parent()
            && let Some(value) =
                prepend_dir_to_path_value(&std::env::var_os("PATH").unwrap_or_default(), bin_dir)
        {
            unsafe { std::env::set_var("PATH", value) };
        }
    }
}

/// 计算前置目录后的新 PATH；目录已在 PATH 中或拼接失败（路径含列表
/// 分隔符等极端情况）时返回 None（保持原值）。
fn prepend_dir_to_path_value(
    existing: &std::ffi::OsStr,
    directory: &Path,
) -> Option<std::ffi::OsString> {
    if std::env::split_paths(existing).any(|dir| dir == directory) {
        return None;
    }
    std::env::join_paths(
        std::iter::once(directory.to_path_buf()).chain(std::env::split_paths(existing)),
    )
    .ok()
}

/// PATH 未命中时的常见安装位置（分层回退）：安装工具声明的根目录 →
/// 版本管理器目录（从新到旧）→ 系统标准位置。某工具变量缺失只说明未
/// 使用该工具，继续尝试后续层次，不据此判定解释器不存在。
fn common_interpreter_locations(runtime: SidecarRuntime, env: &InterpreterEnv) -> Vec<PathBuf> {
    let home = env.home.as_deref();
    let versioned: Vec<(PathBuf, &str)> = match runtime {
        SidecarRuntime::Native => Vec::new(),
        SidecarRuntime::Node if cfg!(windows) => {
            // nvm-windows：NVM_HOME（默认 %APPDATA%\nvm）下 v<ver>\node.exe
            explicit_or_default_root(env.nvm_home.as_ref(), env.appdata.as_deref(), "nvm")
                .map(|root| vec![(root, "node.exe")])
                .unwrap_or_default()
        }
        SidecarRuntime::Node => {
            // nvm：NVM_DIR（默认 ~/.nvm）；asdf：ASDF_DATA_DIR（默认 ~/.asdf）。
            // 显式根各自独立生效，home 缺失不影响已显式声明的工具。
            let mut roots = Vec::new();
            if let Some(nvm) = explicit_or_default_root(env.nvm_dir.as_ref(), home, ".nvm") {
                roots.push((nvm.join("versions").join("node"), "bin/node"));
            }
            if let Some(asdf) = explicit_or_default_root(env.asdf_data_dir.as_ref(), home, ".asdf")
            {
                roots.push((asdf.join("installs").join("nodejs"), "bin/node"));
            }
            roots
        }
        SidecarRuntime::Python if cfg!(windows) => {
            // 官方安装器：用户级 %LOCALAPPDATA%\Programs\Python\Python3xx\、
            // 系统级 %ProgramFiles%\Python3xx\（版本目录直接位于 Program Files 下）；
            // pyenv-win 的 PYENV_ROOT 两种布局并存（根下直接 versions 或多一层
            // pyenv-win），依次探测。
            let mut roots = Vec::new();
            if let Some(local) = &env.local_appdata {
                roots.push((local.join("Programs").join("Python"), "python.exe"));
            }
            if let Some(program_files) = &env.program_files {
                roots.push((program_files.clone(), "python.exe"));
            }
            if let Some(pyenv) = explicit_or_default_root(env.pyenv_root.as_ref(), home, ".pyenv") {
                roots.push((pyenv.join("pyenv-win").join("versions"), "python.exe"));
                roots.push((pyenv.join("versions"), "python.exe"));
            }
            roots
        }
        SidecarRuntime::Python => explicit_or_default_root(env.pyenv_root.as_ref(), home, ".pyenv")
            .map(|pyenv| vec![(pyenv.join("versions"), "bin/python3")])
            .unwrap_or_default(),
    };
    let mut locations = Vec::new();
    for (root, leaf) in versioned {
        locations.extend(versioned_bin_candidates(&root, leaf));
    }
    // Volta 三平台结构一致（VOLTA_HOME，默认 ~/.volta/bin），统一处理
    if matches!(runtime, SidecarRuntime::Node)
        && let Some(volta) = explicit_or_default_root(env.volta_home.as_ref(), home, ".volta")
    {
        let executable = if cfg!(windows) { "node.exe" } else { "node" };
        locations.push(volta.join("bin").join(executable));
    }
    if cfg!(windows) {
        match runtime {
            SidecarRuntime::Node => {
                // nvm-windows 的当前版本 symlink（默认 %ProgramFiles%\nodejs）
                if let Some(symlink) = &env.nvm_symlink {
                    locations.push(symlink.join("node.exe"));
                }
                if let Some(scoop) = explicit_or_default_root(env.scoop.as_ref(), home, "scoop") {
                    locations.push(scoop.join("shims").join("node.exe"));
                }
                if let Some(chocolatey) = explicit_or_default_root(
                    env.chocolatey_install.as_ref(),
                    env.program_data.as_deref(),
                    "chocolatey",
                ) {
                    locations.push(chocolatey.join("bin").join("node.exe"));
                }
                if let Some(program_files) = &env.program_files {
                    locations.push(program_files.join("nodejs").join("node.exe"));
                }
            }
            SidecarRuntime::Python => {
                if let Some(chocolatey) = explicit_or_default_root(
                    env.chocolatey_install.as_ref(),
                    env.program_data.as_deref(),
                    "chocolatey",
                ) {
                    locations.push(chocolatey.join("bin").join("python.exe"));
                }
            }
            SidecarRuntime::Native => {}
        }
    }
    locations.extend(match runtime {
        SidecarRuntime::Node => env.node_system_locations.iter().cloned(),
        SidecarRuntime::Python => env.python_system_locations.iter().cloned(),
        SidecarRuntime::Native => [].iter().cloned(),
    });
    locations
}

/// node 的各平台系统标准安装位置（Homebrew、系统包管理等）。
fn default_node_system_locations() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/opt/homebrew/bin/node"),
            PathBuf::from("/usr/local/bin/node"),
        ]
    } else if cfg!(windows) {
        Vec::new()
    } else {
        vec![
            PathBuf::from("/usr/local/bin/node"),
            PathBuf::from("/usr/bin/node"),
        ]
    }
}

/// python 的各平台系统标准安装位置。
fn default_python_system_locations() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/opt/homebrew/bin/python3"),
            PathBuf::from("/usr/local/bin/python3"),
            PathBuf::from("/usr/bin/python3"),
        ]
    } else if cfg!(windows) {
        Vec::new()
    } else {
        vec![
            PathBuf::from("/usr/local/bin/python3"),
            PathBuf::from("/usr/bin/python3"),
        ]
    }
}

/// 枚举版本管理器安装根目录（如 nvm 的 `versions/node/`）下的 bin 候选，
/// 按版本号新到旧排序；目录不存在时返回空。
fn versioned_bin_candidates(root: &Path, leaf: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort_by_key(|name| std::cmp::Reverse(version_sort_key(name)));
    names
        .into_iter()
        .map(|name| root.join(name).join(leaf))
        .collect()
}

/// 版本目录名（`v22.16.0` / `22.16.0`）转数字段序列供排序；带后缀
/// （如 pyenv 的 `3.11.0-env`）只取主版本段；混合段（如 Windows Python
/// 的 `Python312`）取段内数字拼接比较；无法解析的段按 0 计。
fn version_sort_key(name: &str) -> Vec<u64> {
    name.trim_start_matches(['v', 'V'])
        .split('.')
        .map(|part| {
            let part = part.split_once('-').map_or(part, |(major, _)| major);
            part.parse().unwrap_or_else(|_| {
                let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
                digits.parse().unwrap_or(0)
            })
        })
        .collect()
}

#[cfg(test)]
mod interpreter_discovery_tests {
    use super::*;

    #[test]
    fn version_sort_key_orders_segments_numerically() {
        let mut names = vec!["v9.11.2", "v22.16.0", "v10.24.1"];
        names.sort_by_key(|name| std::cmp::Reverse(version_sort_key(name)));
        assert_eq!(names, ["v22.16.0", "v10.24.1", "v9.11.2"]);
    }

    #[test]
    fn version_sort_key_tolerates_prefix_and_suffix() {
        assert_eq!(version_sort_key("v22.16.0"), version_sort_key("22.16.0"));
        assert!(version_sort_key("3.11.0-env") > version_sort_key("3.10.9"));
        assert!(version_sort_key("Python312") > version_sort_key("Python39"));
        assert_eq!(version_sort_key("not-a-version"), vec![0]);
    }

    #[test]
    fn prepend_dir_to_path_value_prepends_and_deduplicates() {
        let directory = PathBuf::from("/opt/homebrew/bin");
        let tail = [PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        // 用 join/split 按平台分隔符构造与校验，输入输出均不写死格式
        let existing = std::env::join_paths(&tail).unwrap();
        let value = prepend_dir_to_path_value(&existing, &directory).unwrap();
        let mut expected = vec![directory.clone()];
        expected.extend(tail);
        assert_eq!(std::env::split_paths(&value).collect::<Vec<_>>(), expected);
        // 目录已存在时保持原值（返回 None）
        assert!(prepend_dir_to_path_value(&value, &directory).is_none());
    }

    #[test]
    fn versioned_bin_candidates_newest_first_and_missing_root_empty() {
        let dir = tempfile::tempdir().unwrap();
        for version in ["v18.20.0", "v22.16.0", "v9.11.2"] {
            std::fs::create_dir_all(dir.path().join(version).join("bin")).unwrap();
        }
        let candidates = versioned_bin_candidates(dir.path(), "bin/node");
        let versions: Vec<_> = candidates
            .iter()
            .map(|path| {
                path.parent()
                    .and_then(|parent| parent.parent())
                    .and_then(|grandparent| grandparent.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap()
            })
            .collect();
        assert_eq!(versions, ["v22.16.0", "v18.20.0", "v9.11.2"]);
        assert!(candidates[0].ends_with("bin/node"));
        assert!(versioned_bin_candidates(&dir.path().join("absent"), "bin/node").is_empty());
    }

    /// 探测函数经 InterpreterEnv 注入伪造环境，不读写真实环境变量，
    /// 与其他并行测试互不干扰。
    #[test]
    #[cfg(not(windows))]
    fn common_interpreter_locations_covers_nvm_and_system_paths() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".nvm/versions/node/v22.16.0/bin")).unwrap();
        std::fs::create_dir_all(home.path().join(".nvm/versions/node/v18.20.0/bin")).unwrap();
        let env = InterpreterEnv {
            home: Some(home.path().to_path_buf()),
            node_system_locations: default_node_system_locations(),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Node, &env);
        assert_eq!(
            locations[0],
            home.path().join(".nvm/versions/node/v22.16.0/bin/node")
        );
        let expected = if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/node"
        } else {
            "/usr/local/bin/node"
        };
        assert!(locations.contains(&PathBuf::from(expected)));
    }

    /// 显式声明的工具根目录直接生效，不依赖 HOME 推导（NVM_DIR、
    /// ASDF_DATA_DIR、VOLTA_HOME，home 缺失仍可工作）。
    #[test]
    #[cfg(not(windows))]
    fn explicit_node_tool_roots_work_without_home() {
        let nvm = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(nvm.path().join("versions/node/v22.16.0/bin")).unwrap();
        let asdf = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(asdf.path().join("installs/nodejs/v10.24.1/bin")).unwrap();
        let env = InterpreterEnv {
            nvm_dir: Some(nvm.path().to_path_buf()),
            asdf_data_dir: Some(asdf.path().to_path_buf()),
            volta_home: Some(PathBuf::from("/opt/volta")),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Node, &env);
        assert_eq!(
            locations[0],
            nvm.path().join("versions/node/v22.16.0/bin/node")
        );
        assert!(locations.contains(&asdf.path().join("installs/nodejs/v10.24.1/bin/node")));
        assert!(locations.contains(&PathBuf::from("/opt/volta/bin/node")));
    }

    /// 显式 PYENV_ROOT 直接生效，home 缺失仍可工作。
    #[test]
    #[cfg(not(windows))]
    fn explicit_pyenv_root_works_without_home() {
        let pyenv = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(pyenv.path().join("versions/3.12.1/bin")).unwrap();
        let env = InterpreterEnv {
            pyenv_root: Some(pyenv.path().to_path_buf()),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Python, &env);
        assert_eq!(
            locations[0],
            pyenv.path().join("versions/3.12.1/bin/python3")
        );
    }

    /// PATH 命中优先于常见安装位置。
    #[test]
    fn search_prefers_path_before_common_locations() {
        let path_dir = tempfile::tempdir().unwrap();
        let program = path_dir
            .path()
            .join(if cfg!(windows) { "node.exe" } else { "node" });
        std::fs::write(&program, b"").unwrap();
        let env = InterpreterEnv {
            search_paths: vec![path_dir.path().to_path_buf()],
            volta_home: Some(PathBuf::from("/opt/volta")),
            ..InterpreterEnv::default()
        };
        assert_eq!(
            search_interpreter_program(SidecarRuntime::Node, &env),
            Some(program)
        );
    }

    /// 空 PATH 时回退版本管理器目录。
    #[test]
    fn empty_search_paths_fall_back_to_versioned_roots() {
        let nvm = tempfile::tempdir().unwrap();
        let program = nvm.path().join("versions/node/v22.16.0/bin/node");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, b"").unwrap();
        let env = InterpreterEnv {
            nvm_dir: Some(nvm.path().to_path_buf()),
            ..InterpreterEnv::default()
        };
        assert_eq!(
            search_interpreter_program(SidecarRuntime::Node, &env),
            Some(program)
        );
    }

    /// 所有来源为空（快照全空、系统位置未填）时探测返回 None。
    #[test]
    fn all_sources_empty_returns_none() {
        let env = InterpreterEnv::default();
        assert_eq!(search_interpreter_program(SidecarRuntime::Node, &env), None);
        assert_eq!(
            search_interpreter_program(SidecarRuntime::Python, &env),
            None
        );
    }

    #[test]
    #[cfg(windows)]
    fn common_interpreter_locations_covers_windows_user_paths() {
        let home = tempfile::tempdir().unwrap();
        let env = InterpreterEnv {
            home: Some(home.path().to_path_buf()),
            nvm_symlink: Some(home.path().join("node-current")),
            program_files: Some(PathBuf::from(r"C:\Program Files")),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Node, &env);
        assert!(locations.contains(&home.path().join(".volta").join("bin").join("node.exe")));
        assert!(locations.contains(&home.path().join("node-current").join("node.exe")));
        assert!(locations.contains(&PathBuf::from(r"C:\Program Files\nodejs\node.exe")));
    }

    /// Windows 系统级 Python 版本目录直接位于 Program Files 下，且新
    /// 版本排在旧版本前面（Python312 先于 Python39）。
    #[test]
    #[cfg(windows)]
    fn common_interpreter_locations_covers_windows_system_python() {
        let program_files = tempfile::tempdir().unwrap();
        for version in ["Python39", "Python312"] {
            std::fs::create_dir_all(program_files.path().join(version)).unwrap();
        }
        let python312 = program_files.path().join("Python312").join("python.exe");
        std::fs::write(&python312, b"").unwrap();
        let env = InterpreterEnv {
            program_files: Some(program_files.path().to_path_buf()),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Python, &env);
        let position312 = locations
            .iter()
            .position(|path| path == &python312)
            .unwrap();
        let position39 = locations
            .iter()
            .position(|path| *path == program_files.path().join("Python39").join("python.exe"))
            .unwrap();
        assert!(
            position312 < position39,
            "Python312 应排在 Python39 前：{locations:?}"
        );
    }
}
