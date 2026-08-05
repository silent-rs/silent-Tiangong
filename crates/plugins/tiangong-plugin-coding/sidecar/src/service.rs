use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use tiangong_plugin_coding_protocol::*;
use tiangong_plugin_runtime::protocol::{
    ErrorCode, HANDSHAKE_OPERATION, HandshakeResponse, PROTOCOL_VERSION, Request, Response,
    ServiceStatus,
};
use tiangong_plugin_runtime::sidecar::PLUGIN_DATA_DIR_ENV;

const PROJECT_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "Gemfile",
    "mix.exs",
    "deno.json",
    "deno.jsonc",
    "Makefile",
    "Justfile",
];
const RULE_FILE_NAMES: &[&str] = &[
    "agents.md",
    "claude.md",
    "contributing.md",
    "development.md",
    "instructions.md",
    "copilot-instructions.md",
];
const WORKFLOW_FILE_TOKENS: &[&str] = &[
    "plan",
    "planning",
    "roadmap",
    "task",
    "tasks",
    "todo",
    "backlog",
    "milestone",
    "milestones",
    "requirement",
    "requirements",
    "spec",
    "progress",
    "计划",
    "规划",
    "路线",
    "任务",
    "需求",
    "进度",
];
const IGNORED_DISCOVERY_DIRS: &[&str] = &[
    ".git",
    ".idea",
    ".venv",
    ".vscode",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
];
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub struct CodingService {
    data_dir: PathBuf,
}

impl CodingService {
    pub fn new() -> Result<Self> {
        let data_dir = coding_data_dir()?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("创建 Coding 数据目录失败: {}", data_dir.display()))?;
        Ok(Self { data_dir })
    }

    async fn dispatch_operation(
        &self,
        operation: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match operation {
            HANDSHAKE_OPERATION => self.handshake(),
            name if name == ProjectContext::NAME => {
                let request = serde_json::from_value(payload)?;
                serde_json::to_value(self.project_context(&request)?)
                    .context("序列化项目上下文失败")
            }
            name if name == Preflight::NAME => {
                let request = serde_json::from_value(payload)?;
                serde_json::to_value(self.preflight(&request)?).context("序列化开发前检查失败")
            }
            name if name == Checkpoint::NAME => {
                let request = serde_json::from_value(payload)?;
                serde_json::to_value(self.save_checkpoint(&request)?).context("序列化进度记录失败")
            }
            name if name == Review::NAME => {
                let request = serde_json::from_value(payload)?;
                serde_json::to_value(review(&request)?).context("序列化交付审查失败")
            }
            other => Err(anyhow!("未知的 Coding 操作: {other}")),
        }
    }

    fn handshake(&self) -> Result<serde_json::Value> {
        serde_json::to_value(HandshakeResponse {
            plugin_id: PLUGIN_ID.to_string(),
            plugin_version: PLUGIN_VERSION.to_string(),
            sidecar_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            business_protocol: CODING_PROTOCOL_VERSION,
            capabilities: vec![
                TOOL_PROJECT_CONTEXT.to_string(),
                TOOL_PREFLIGHT.to_string(),
                TOOL_CHECKPOINT.to_string(),
                TOOL_REVIEW.to_string(),
            ],
            instance_id: format!("coding-sidecar-{}", std::process::id()),
            status: ServiceStatus::Ready,
        })
        .context("序列化 Coding 握手失败")
    }

    async fn dispatch_request(&self, request: Request) -> Response {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return Response::error(
                &request_id,
                ErrorCode::ProtocolMismatch,
                "Coding sidecar 协议版本不匹配",
                false,
            );
        }
        match self
            .dispatch_operation(&request.operation, request.payload)
            .await
        {
            Ok(payload) => Response::success(&request_id, payload),
            Err(error) => Response::error(
                &request_id,
                ErrorCode::ServiceError,
                error.to_string(),
                false,
            ),
        }
    }

    fn save_checkpoint(&self, request: &CheckpointRequest) -> Result<CheckpointResponse> {
        let workspace = workspace_path(&request.workspace)?;
        let checkpoint_dir = self.data_dir.join("checkpoints");
        std::fs::create_dir_all(&checkpoint_dir).context("创建 Coding 进度目录失败")?;

        let checkpoint_path = self.checkpoint_path(&workspace);
        let saved_at = chrono::Local::now().naive_local().to_string();
        let checkpoint = SavedCheckpoint {
            saved_at: saved_at.clone(),
            state: request.clone(),
        };
        let content = serde_json::to_vec_pretty(&checkpoint).context("序列化进度记录失败")?;
        std::fs::write(&checkpoint_path, content).context("写入进度记录失败")?;

        Ok(CheckpointResponse {
            saved_at,
            checkpoint_path: checkpoint_path.display().to_string(),
        })
    }

    fn checkpoint_path(&self, workspace: &Path) -> PathBuf {
        let digest = Sha256::digest(workspace.to_string_lossy().as_bytes());
        let workspace_key = &hex::encode(digest)[..24];
        self.data_dir
            .join("checkpoints")
            .join(format!("{workspace_key}.json"))
    }

    fn load_checkpoint(&self, workspace: &Path) -> Option<SavedCheckpoint> {
        let content = std::fs::read(self.checkpoint_path(workspace)).ok()?;
        serde_json::from_slice(&content).ok()
    }

    fn project_context(&self, request: &WorkspaceRequest) -> Result<ProjectContextResponse> {
        let mut context = discover_project_context(request)?;
        context.latest_checkpoint = request
            .task
            .as_deref()
            .filter(|task| !task.trim().is_empty())
            .and_then(|task| {
                self.load_checkpoint(Path::new(&context.workspace))
                    .filter(|checkpoint| checkpoint.state.task.trim() == task.trim())
            });
        Ok(context)
    }

    fn preflight(&self, request: &PreflightRequest) -> Result<PreflightResponse> {
        let context = self.project_context(&WorkspaceRequest {
            workspace: request.workspace.clone(),
            full_trust: request.full_trust,
            task: Some(request.task.clone()),
        })?;
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        if request.task.trim().is_empty() {
            blockers.push("缺少开发任务说明".to_string());
        }
        if !context.version_control_inspected {
            warnings
                .push("版本控制状态检查未完成，不能据此判断工作区是否存在未提交改动".to_string());
        } else if context.version_controlled && context.has_uncommitted_changes {
            warnings.push("工作区存在未提交改动，修改前需确认归属".to_string());
        }
        if context.recommended_checks.is_empty() {
            warnings.push(
                "未从项目配置发现可直接执行的检查命令，需要按仓库约定确定验证方式".to_string(),
            );
        }

        Ok(PreflightResponse {
            workspace: context.workspace,
            task: request.task.trim().to_string(),
            version_controlled: context.version_controlled,
            version_control_inspected: context.version_control_inspected,
            git_branch: context.git_branch,
            has_uncommitted_changes: context.has_uncommitted_changes,
            blockers,
            warnings,
            completion_criteria: vec![
                "任务目标和边界已经明确".to_string(),
                "遵循当前项目自身的规则和工作流".to_string(),
                "最终改动只包含任务所需内容".to_string(),
                "实际验证结果通过".to_string(),
            ],
        })
    }
}
#[async_trait::async_trait]
impl tiangong_plugin_sidecar::SidecarService for CodingService {
    async fn dispatch(&self, request: Request) -> Response {
        self.dispatch_request(request).await
    }
}

fn coding_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(PLUGIN_DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let runtime_dir = tiangong_plugin_sidecar::endpoint::runtime_dir(PLUGIN_ID)?;
    let plugin_dir = runtime_dir
        .parent()
        .ok_or_else(|| anyhow!("Coding 运行目录缺少父目录"))?;
    Ok(plugin_dir.join("data"))
}

fn workspace_path(path: &str) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    if path.as_os_str().is_empty() || !path.is_dir() {
        return Err(anyhow!("工作区不存在或不是目录"));
    }
    path.canonicalize().context("解析工作区路径失败")
}

fn discover_project_context(request: &WorkspaceRequest) -> Result<ProjectContextResponse> {
    let workspace = workspace_path(&request.workspace)?;
    let roots = discovery_roots(&workspace);
    let project_files = discover_project_files(&workspace, &roots);
    let rule_files = discover_rule_files(&workspace, &roots);
    let workflow_files = discover_workflow_files(&workspace, &roots);
    let project_types = detect_project_types(&project_files);
    let recommended_checks = recommend_checks(&workspace, &project_files);
    let git = git_context(&workspace);

    Ok(ProjectContextResponse {
        workspace: workspace.display().to_string(),
        full_trust: request.full_trust,
        project_types,
        project_files,
        rule_files,
        workflow_files,
        version_controlled: git.version_controlled,
        version_control_inspected: git.inspection_complete,
        git_branch: git.branch,
        has_uncommitted_changes: !git.changed_files.is_empty(),
        recommended_checks,
        latest_checkpoint: None,
    })
}

fn review(request: &ReviewRequest) -> Result<ReviewResponse> {
    let workspace = workspace_path(&request.workspace)?;
    let git = git_review_context(&workspace, request.base_ref.as_deref())?;
    let allowed_paths = request
        .allowed_paths
        .iter()
        .map(|file| normalize_relative_path(file))
        .filter(|path| !path.is_empty())
        .collect::<BTreeSet<_>>();
    let unexpected_files = git
        .changed_files
        .iter()
        .filter(|file| {
            !allowed_paths.is_empty()
                && !allowed_paths
                    .iter()
                    .any(|allowed| path_is_within_scope(file, allowed))
        })
        .cloned()
        .collect::<Vec<_>>();
    let failed_verifications = request
        .verification
        .iter()
        .filter(|result| !result.passed)
        .map(|result| {
            if result.name.trim().is_empty() {
                "未命名检查".to_string()
            } else {
                result.name.clone()
            }
        })
        .collect::<Vec<_>>();
    let verification_complete = !request.verification.is_empty() && failed_verifications.is_empty();
    let ready = verification_complete && unexpected_files.is_empty() && git.inspection_complete;
    let mut notes = Vec::new();
    if request.verification.is_empty() {
        notes.push("尚未记录验证结果".to_string());
    } else if !failed_verifications.is_empty() {
        notes.push("仍有未通过的验证".to_string());
    }
    if !unexpected_files.is_empty() {
        notes.push("存在预期范围外的改动".to_string());
    }
    if !git.inspection_complete {
        notes.push("未能确定 Git 基线或读取完整改动范围".to_string());
    } else if !git.version_controlled {
        notes.push("未检测到版本控制，无法自动核对改动文件范围".to_string());
    }

    Ok(ReviewResponse {
        version_controlled: git.version_controlled,
        version_control_inspected: git.inspection_complete,
        base_ref: git.base_ref,
        merge_base: git.merge_base,
        has_committed_changes: !git.committed_files.is_empty(),
        has_uncommitted_changes: !git.worktree_files.is_empty(),
        changed_files: git.changed_files,
        unexpected_files,
        verification_complete,
        failed_verifications,
        ready,
        notes,
    })
}

fn discovery_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = vec![workspace.to_path_buf()];
    let Ok(entries) = std::fs::read_dir(workspace) else {
        return roots;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if IGNORED_DISCOVERY_DIRS.contains(&name.as_ref()) {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            roots.push(entry.path());
        }
    }
    roots.sort();
    roots
}

fn discover_project_files(workspace: &Path, roots: &[PathBuf]) -> Vec<String> {
    let mut files = BTreeSet::new();
    for root in roots {
        for name in PROJECT_MANIFESTS {
            let path = root.join(name);
            if path.is_file() {
                files.insert(relative_path(workspace, &path));
            }
        }
    }
    files.into_iter().collect()
}

fn discover_rule_files(workspace: &Path, roots: &[PathBuf]) -> Vec<String> {
    let mut files = BTreeSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if RULE_FILE_NAMES.contains(&name.as_str()) {
                files.insert(relative_path(workspace, &entry.path()));
            }
        }
    }
    files.into_iter().collect()
}

fn discover_workflow_files(workspace: &Path, roots: &[PathBuf]) -> Vec<String> {
    let mut files = BTreeSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path();
            if path
                .extension()
                .and_then(|value| value.to_str())
                .is_none_or(|value| !value.eq_ignore_ascii_case("md"))
            {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let tokens = stem
                .split(|character: char| !character.is_alphanumeric())
                .filter(|token| !token.is_empty())
                .collect::<Vec<_>>();
            if tokens
                .iter()
                .any(|token| WORKFLOW_FILE_TOKENS.contains(token))
                || WORKFLOW_FILE_TOKENS
                    .iter()
                    .any(|token| !token.is_ascii() && stem.contains(token))
            {
                files.insert(relative_path(workspace, &path));
            }
        }
    }
    files.into_iter().collect()
}

fn detect_project_types(project_files: &[String]) -> Vec<String> {
    let mut types = BTreeSet::new();
    for file in project_files {
        let name = Path::new(file)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        match name {
            "Cargo.toml" => {
                types.insert("rust".to_string());
            }
            "package.json" | "deno.json" | "deno.jsonc" => {
                types.insert("javascript-typescript".to_string());
            }
            "pyproject.toml" => {
                types.insert("python".to_string());
            }
            "go.mod" => {
                types.insert("go".to_string());
            }
            "pom.xml" | "build.gradle" | "build.gradle.kts" => {
                types.insert("jvm".to_string());
            }
            "composer.json" => {
                types.insert("php".to_string());
            }
            "Gemfile" => {
                types.insert("ruby".to_string());
            }
            "mix.exs" => {
                types.insert("elixir".to_string());
            }
            _ => {}
        }
    }
    types.into_iter().collect()
}

fn recommend_checks(workspace: &Path, project_files: &[String]) -> Vec<RecommendedCheck> {
    let mut checks = BTreeSet::new();
    let has_root_cargo = project_files.iter().any(|file| file == "Cargo.toml");
    for file in project_files {
        let relative = Path::new(file);
        let name = relative
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let cwd = if parent.as_os_str().is_empty() {
            ".".to_string()
        } else {
            normalize_relative_path(&parent.to_string_lossy())
        };
        match name {
            "Cargo.toml" if !has_root_cargo || file == "Cargo.toml" => {
                checks.insert((cwd, "cargo check".to_string()));
            }
            "package.json" => {
                let manifest = workspace.join(relative);
                for command in package_checks(&manifest) {
                    checks.insert((cwd.clone(), command));
                }
            }
            "go.mod" => {
                checks.insert((cwd, "go test ./...".to_string()));
            }
            "pyproject.toml" => {
                let manifest = workspace.join(relative);
                let content = std::fs::read_to_string(&manifest).unwrap_or_default();
                if content.contains("pytest") {
                    let command = if manifest
                        .parent()
                        .is_some_and(|directory| directory.join("uv.lock").is_file())
                    {
                        "uv run pytest"
                    } else {
                        "python -m pytest"
                    };
                    checks.insert((cwd, command.to_string()));
                }
            }
            _ => {}
        }
    }
    checks
        .into_iter()
        .map(|(cwd, command)| RecommendedCheck { cwd, command })
        .collect()
}

fn package_checks(manifest: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(manifest) else {
        return Vec::new();
    };
    let Ok(package) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    let Some(scripts) = package
        .get("scripts")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let manager = package_manager(manifest, &package);
    ["build", "check", "typecheck", "lint", "test"]
        .into_iter()
        .filter(|script| scripts.contains_key(*script))
        .map(|script| match manager.as_str() {
            "yarn" => format!("yarn {script}"),
            "pnpm" => format!("pnpm run {script}"),
            "bun" => format!("bun run {script}"),
            _ => format!("npm run {script}"),
        })
        .collect()
}

fn package_manager(manifest: &Path, package: &serde_json::Value) -> String {
    if let Some(manager) = package
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.split('@').next())
        .filter(|value| matches!(*value, "yarn" | "pnpm" | "bun" | "npm"))
    {
        return manager.to_string();
    }
    let directory = manifest.parent().unwrap_or_else(|| Path::new("."));
    if directory.join("yarn.lock").is_file() {
        "yarn".to_string()
    } else if directory.join("pnpm-lock.yaml").is_file() {
        "pnpm".to_string()
    } else if directory.join("bun.lock").is_file() || directory.join("bun.lockb").is_file() {
        "bun".to_string()
    } else {
        "npm".to_string()
    }
}

#[derive(Default)]
struct GitContext {
    version_controlled: bool,
    inspection_complete: bool,
    branch: Option<String>,
    changed_files: Vec<String>,
}

#[derive(Default)]
struct GitReviewContext {
    version_controlled: bool,
    inspection_complete: bool,
    base_ref: Option<String>,
    merge_base: Option<String>,
    committed_files: Vec<String>,
    worktree_files: Vec<String>,
    changed_files: Vec<String>,
}

fn git_review_context(workspace: &Path, requested_base: Option<&str>) -> Result<GitReviewContext> {
    let worktree = git_context(workspace);
    if !worktree.version_controlled {
        return Ok(GitReviewContext {
            version_controlled: false,
            inspection_complete: worktree.inspection_complete,
            worktree_files: worktree.changed_files.clone(),
            changed_files: worktree.changed_files,
            ..GitReviewContext::default()
        });
    }

    let base_ref = resolve_review_base(workspace, requested_base)?;
    let merge_base = base_ref
        .as_deref()
        .and_then(|base| command_text(workspace, &["git", "merge-base", "HEAD", base]))
        .filter(|value| !value.is_empty());
    let committed_files = merge_base
        .as_deref()
        .and_then(|base| git_diff_files(workspace, base));
    let inspection_complete = worktree.inspection_complete
        && base_ref.is_some()
        && merge_base.is_some()
        && committed_files.is_some();
    let committed_files = committed_files.unwrap_or_default();
    let worktree_files = worktree.changed_files;
    let mut changed_files = committed_files.iter().cloned().collect::<BTreeSet<_>>();
    changed_files.extend(worktree_files.iter().cloned());

    Ok(GitReviewContext {
        version_controlled: true,
        inspection_complete,
        base_ref,
        merge_base,
        committed_files,
        worktree_files,
        changed_files: changed_files.into_iter().collect(),
    })
}

fn resolve_review_base(workspace: &Path, requested_base: Option<&str>) -> Result<Option<String>> {
    if let Some(base) = requested_base
        .map(str::trim)
        .filter(|base| !base.is_empty())
    {
        if git_ref_exists(workspace, base) {
            return Ok(Some(base.to_string()));
        }
        return Err(anyhow!("Git 基线引用不存在: {base}"));
    }

    let mut candidates = Vec::new();
    if let Some(remote_head) = command_text(
        workspace,
        &["git", "symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        candidates.push(remote_head);
    }
    candidates.extend(
        ["main", "develop", "master"]
            .into_iter()
            .map(str::to_string),
    );
    if let Some(upstream) = command_text(
        workspace,
        &[
            "git",
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    ) {
        candidates.push(upstream);
    }

    Ok(candidates
        .into_iter()
        .find(|candidate| git_ref_exists(workspace, candidate)))
}

fn git_ref_exists(workspace: &Path, reference: &str) -> bool {
    let commit_ref = format!("{reference}^{{commit}}");
    matches!(
        command_output(
            workspace,
            &["git", "rev-parse", "--verify", "--quiet", &commit_ref],
        ),
        CommandOutput::Success(_)
    )
}

fn git_diff_files(workspace: &Path, base: &str) -> Option<Vec<String>> {
    let CommandOutput::Success(output) = command_output(
        workspace,
        &[
            "git",
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=ACDMRTUXB",
            base,
            "HEAD",
        ],
    ) else {
        return None;
    };
    Some(
        output
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| normalize_relative_path(&String::from_utf8_lossy(path)))
            .collect(),
    )
}

fn path_is_within_scope(path: &str, allowed: &str) -> bool {
    allowed == "."
        || path == allowed
        || path
            .strip_prefix(allowed)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn git_context(workspace: &Path) -> GitContext {
    let version_controlled =
        match command_output(workspace, &["git", "rev-parse", "--is-inside-work-tree"]) {
            CommandOutput::Success(output) => String::from_utf8_lossy(&output).trim() == "true",
            CommandOutput::ExitFailure => {
                return GitContext {
                    inspection_complete: true,
                    ..GitContext::default()
                };
            }
            CommandOutput::Unavailable => return GitContext::default(),
        };
    if !version_controlled {
        return GitContext {
            inspection_complete: true,
            ..GitContext::default()
        };
    }
    let branch = command_text(workspace, &["git", "branch", "--show-current"])
        .filter(|value| !value.is_empty())
        .or_else(|| {
            command_text(workspace, &["git", "rev-parse", "--short", "HEAD"])
                .map(|commit| format!("detached@{commit}"))
        });
    let changed_files = git_changed_files(workspace);
    GitContext {
        version_controlled,
        inspection_complete: changed_files.is_some(),
        branch,
        changed_files: changed_files.unwrap_or_default(),
    }
}

enum CommandOutput {
    Success(Vec<u8>),
    ExitFailure,
    Unavailable,
}

fn command_output(workspace: &Path, args: &[&str]) -> CommandOutput {
    let Some(program) = args.first().copied() else {
        return CommandOutput::Unavailable;
    };
    let Ok(mut child) = Command::new(program)
        .args(&args[1..])
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return CommandOutput::Unavailable;
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return CommandOutput::Unavailable;
    };
    let output_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(output_reader);
                return CommandOutput::Unavailable;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(output_reader);
                return CommandOutput::Unavailable;
            }
        }
    };
    while !output_reader.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    if !output_reader.is_finished() {
        drop(output_reader);
        return CommandOutput::Unavailable;
    }
    let Ok(Ok(output)) = output_reader.join() else {
        return CommandOutput::Unavailable;
    };
    if status.success() {
        CommandOutput::Success(output)
    } else {
        CommandOutput::ExitFailure
    }
}

fn command_text(workspace: &Path, args: &[&str]) -> Option<String> {
    match command_output(workspace, args) {
        CommandOutput::Success(output) => Some(String::from_utf8_lossy(&output).trim().to_string()),
        CommandOutput::ExitFailure | CommandOutput::Unavailable => None,
    }
}

fn git_changed_files(workspace: &Path) -> Option<Vec<String>> {
    let CommandOutput::Success(output) = command_output(
        workspace,
        &[
            "git",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    ) else {
        return None;
    };
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut files = BTreeSet::new();
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let status = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]);
        files.insert(normalize_relative_path(&path));
        if status.contains(&b'R') || status.contains(&b'C') {
            let _ = fields.next();
        }
    }
    Some(files.into_iter().collect())
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .map(|relative| normalize_relative_path(&relative.to_string_lossy()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn normalize_relative_path(path: &str) -> String {
    path.trim().trim_start_matches("./").replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(workspace: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git 应可执行");
        assert!(status.success(), "git {args:?} 执行失败");
    }

    fn passing_verification() -> Vec<VerificationResult> {
        vec![VerificationResult {
            name: "cargo check".to_string(),
            passed: true,
            details: String::new(),
        }]
    }

    #[test]
    fn review_combines_committed_and_worktree_changes() {
        let workspace = tempfile::tempdir().expect("创建临时仓库");
        run_git(workspace.path(), &["init", "-b", "main"]);
        run_git(
            workspace.path(),
            &["config", "user.email", "test@example.com"],
        );
        run_git(workspace.path(), &["config", "user.name", "Test"]);
        std::fs::write(workspace.path().join("base.txt"), "base").expect("写入基线文件");
        run_git(workspace.path(), &["add", "base.txt"]);
        run_git(workspace.path(), &["commit", "-m", "base"]);
        run_git(workspace.path(), &["switch", "-c", "feature/review"]);
        std::fs::write(workspace.path().join("committed.rs"), "fn committed() {}")
            .expect("写入已提交文件");
        run_git(workspace.path(), &["add", "committed.rs"]);
        run_git(workspace.path(), &["commit", "-m", "feature"]);

        let clean_review = review(&ReviewRequest {
            workspace: workspace.path().display().to_string(),
            base_ref: Some("main".to_string()),
            allowed_paths: Vec::new(),
            verification: passing_verification(),
        })
        .expect("检查干净功能分支");
        assert_eq!(clean_review.changed_files, vec!["committed.rs"]);
        assert!(clean_review.has_committed_changes);
        assert!(!clean_review.has_uncommitted_changes);
        assert!(clean_review.ready);

        std::fs::write(workspace.path().join("worktree.rs"), "fn worktree() {}")
            .expect("写入工作区文件");
        let combined_review = review(&ReviewRequest {
            workspace: workspace.path().display().to_string(),
            base_ref: Some("main".to_string()),
            allowed_paths: vec!["committed.rs".to_string()],
            verification: passing_verification(),
        })
        .expect("检查组合改动");
        assert_eq!(
            combined_review.changed_files,
            vec!["committed.rs", "worktree.rs"]
        );
        assert_eq!(combined_review.unexpected_files, vec!["worktree.rs"]);
        assert!(!combined_review.ready);
    }

    #[test]
    fn project_context_only_restores_matching_task_checkpoint() {
        let workspace = tempfile::tempdir().expect("创建临时工作区");
        let data_dir = tempfile::tempdir().expect("创建临时数据目录");
        let service = CodingService {
            data_dir: data_dir.path().to_path_buf(),
        };
        service
            .save_checkpoint(&CheckpointRequest {
                workspace: workspace.path().display().to_string(),
                task: "修复流式工具参数".to_string(),
                completion_criteria: Vec::new(),
                completed: Vec::new(),
                changed_files: Vec::new(),
                verification: Vec::new(),
                blockers: Vec::new(),
            })
            .expect("保存进度");

        let unrelated = service
            .project_context(&WorkspaceRequest {
                workspace: workspace.path().display().to_string(),
                full_trust: true,
                task: Some("开发其他功能".to_string()),
            })
            .expect("读取其他任务上下文");
        assert!(unrelated.latest_checkpoint.is_none());

        let matching = service
            .project_context(&WorkspaceRequest {
                workspace: workspace.path().display().to_string(),
                full_trust: true,
                task: Some("修复流式工具参数".to_string()),
            })
            .expect("读取当前任务上下文");
        assert_eq!(
            matching
                .latest_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.state.task.as_str()),
            Some("修复流式工具参数")
        );
    }
}
