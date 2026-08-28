//! 解释器（node/python）运行环境探测与入口注入。
//!
//! GUI 进程（launchd/Finder 启动）不执行 shell 初始化，nvm/Homebrew 等
//! 安装位置不在其 PATH 中：本模块提供 PATH 之外的分层探测（安装工具
//! 声明的根目录 → 版本管理器目录从新到旧 → 系统标准位置），以及进程
//! 入口的 [`ensure_interpreter_env`] 环境注入，供插件 sidecar 与命令
//! 通道整棵子进程树使用。

use std::path::{Path, PathBuf};

use crate::manifest::SidecarRuntime;

/// 跨平台获取用户 home 目录（与 sidecar 框架 `endpoint::home_dir` 同源）。
pub(crate) fn user_home_dir() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(home));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(profile));
    }
    let drive = std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty());
    let path = std::env::var_os("HOMEPATH").filter(|v| !v.is_empty());
    match (drive, path) {
        (Some(drive), Some(path)) => {
            let mut buf = std::path::PathBuf::from(drive);
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
    /// `ProgramFiles` 环境缺失时退回惯例位置（Windows 专用字段）。
    program_files: Option<PathBuf>,
}

impl InterpreterEnv {
    pub(crate) fn from_process() -> Self {
        let dir = |key: &str| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
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
            program_files: dir("ProgramFiles").or_else(|| Some(PathBuf::from(r"C:\Program Files"))),
        }
    }
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
    let search_paths = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    for directory in search_paths {
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
            let root = env
                .nvm_home
                .clone()
                .or_else(|| env.appdata.as_ref().map(|appdata| appdata.join("nvm")));
            root.map(|root| vec![(root, "node.exe")])
                .unwrap_or_default()
        }
        SidecarRuntime::Node => home
            .map(|home| {
                vec![
                    (
                        env.nvm_dir
                            .clone()
                            .unwrap_or_else(|| home.join(".nvm"))
                            .join("versions/node"),
                        "bin/node",
                    ),
                    (
                        env.asdf_data_dir
                            .clone()
                            .unwrap_or_else(|| home.join(".asdf"))
                            .join("installs/nodejs"),
                        "bin/node",
                    ),
                ]
            })
            .unwrap_or_default(),
        SidecarRuntime::Python if cfg!(windows) => {
            // 官方安装器：用户级 %LOCALAPPDATA%\Programs\Python\Python3xx\、
            // 系统级 %ProgramFiles%\Python3xx\（版本目录直接位于 Program Files 下）；
            // pyenv-win：PYENV_ROOT（默认 ~\.pyenv）\pyenv-win\versions\<ver>\
            let mut roots = Vec::new();
            if let Some(local) = &env.local_appdata {
                roots.push((local.join("Programs").join("Python"), "python.exe"));
            }
            if let Some(program_files) = &env.program_files {
                roots.push((program_files.clone(), "python.exe"));
            }
            if let Some(home) = home {
                roots.push((
                    env.pyenv_root
                        .clone()
                        .unwrap_or_else(|| home.join(".pyenv"))
                        .join("pyenv-win")
                        .join("versions"),
                    "python.exe",
                ));
            }
            roots
        }
        SidecarRuntime::Python => home
            .map(|home| {
                vec![(
                    env.pyenv_root
                        .clone()
                        .unwrap_or_else(|| home.join(".pyenv"))
                        .join("versions"),
                    "bin/python3",
                )]
            })
            .unwrap_or_default(),
    };
    let mut locations = Vec::new();
    for (root, leaf) in versioned {
        locations.extend(versioned_bin_candidates(&root, leaf));
    }
    // Volta 三平台结构一致（~/.volta/bin），统一处理
    if matches!(runtime, SidecarRuntime::Node)
        && let Some(home) = home
    {
        let volta = env
            .volta_home
            .clone()
            .unwrap_or_else(|| home.join(".volta"));
        let executable = if cfg!(windows) { "node.exe" } else { "node" };
        locations.push(volta.join("bin").join(executable));
    }
    match runtime {
        SidecarRuntime::Node => {
            if cfg!(target_os = "macos") {
                locations.extend([
                    PathBuf::from("/opt/homebrew/bin/node"),
                    PathBuf::from("/usr/local/bin/node"),
                ]);
            } else if cfg!(windows) {
                // nvm-windows 的当前版本 symlink（默认 C:\Program Files\nodejs）
                if let Some(symlink) = &env.nvm_symlink {
                    locations.push(symlink.join("node.exe"));
                }
                if let Some(home) = home {
                    let scoop = env.scoop.clone().unwrap_or_else(|| home.join("scoop"));
                    locations.push(scoop.join("shims").join("node.exe"));
                }
                let chocolatey = env
                    .chocolatey_install
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\chocolatey"));
                locations.push(chocolatey.join("bin").join("node.exe"));
                if let Some(program_files) = &env.program_files {
                    locations.push(program_files.join("nodejs").join("node.exe"));
                }
            } else {
                locations.extend([
                    PathBuf::from("/usr/local/bin/node"),
                    PathBuf::from("/usr/bin/node"),
                ]);
            }
        }
        SidecarRuntime::Python => {
            if cfg!(target_os = "macos") {
                locations.extend([
                    PathBuf::from("/opt/homebrew/bin/python3"),
                    PathBuf::from("/usr/local/bin/python3"),
                    PathBuf::from("/usr/bin/python3"),
                ]);
            } else if !cfg!(windows) {
                locations.extend([
                    PathBuf::from("/usr/local/bin/python3"),
                    PathBuf::from("/usr/bin/python3"),
                ]);
            } else if let Some(chocolatey) = &env.chocolatey_install {
                locations.push(chocolatey.join("bin").join("python.exe"));
            }
        }
        SidecarRuntime::Native => {}
    }
    locations
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

    #[test]
    #[cfg(not(windows))]
    fn common_interpreter_locations_honors_tool_root_overrides() {
        // NVM_DIR 自定义根优先于 home 默认位置；home 缺失仍可工作
        let custom = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(custom.path().join("versions/node/v22.16.0/bin")).unwrap();
        let env = InterpreterEnv {
            home: Some(PathBuf::from("/nonexistent-home")),
            nvm_dir: Some(custom.path().to_path_buf()),
            volta_home: Some(PathBuf::from("/opt/volta")),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Node, &env);
        assert_eq!(
            locations[0],
            custom.path().join("versions/node/v22.16.0/bin/node")
        );
        assert!(locations.contains(&PathBuf::from("/opt/volta/bin/node")));
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

    #[test]
    #[cfg(windows)]
    fn common_interpreter_locations_covers_windows_system_python() {
        // 系统级 Python 版本目录直接位于 Program Files 下（Python3xx），
        // 经 ProgramFiles 变量推导而非写死盘符
        let env = InterpreterEnv {
            program_files: Some(PathBuf::from(r"D:\Program Files")),
            ..InterpreterEnv::default()
        };
        let locations = common_interpreter_locations(SidecarRuntime::Python, &env);
        assert!(locations.contains(&PathBuf::from(r"D:\Program Files\Python312\python.exe")));
    }
}
