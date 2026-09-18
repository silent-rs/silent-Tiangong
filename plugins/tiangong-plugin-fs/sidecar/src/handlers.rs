//! Fs 工具业务实现：从原进程内 `handler.rs` 迁移。
//!
//! 改造点：
//! - 入参由 `ToolCall`（JSON）改为 protocol 的类型化 Request
//! - 输出由 core `ToolResult` 改为 protocol `FsToolResponse`
//! - 路径解析经 [`PathPolicy`](crate::path_policy::PathPolicy)（沙箱预留点 A），
//!   不再直接调 toolkit 自由函数
//! - 锁的 acquire/release 在写工具内部完成（一次调用完成加锁+写+解锁，避免锁泄漏），
//!   响应里带回 locked/unlocked 路径供 wasm 发 StreamEvent

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use tiangong_plugin_fs_protocol::FsAccessContext;
use tiangong_plugin_fs_protocol::tools::{
    ApplyPatchRequest, FsToolResponse, ListDirRequest, MentionFileCandidate, MentionFilesRequest,
    MentionFilesResponse, ReadFileRequest, ReplaceInFileRequest, TreeDirRequest, WriteFileRequest,
};
use tiangong_toolkit as shared;

use crate::file_lock::{FileLockTable, canonicalize_for_lock};
use crate::path_policy::PathPolicy;

const DEFAULT_TREE_MAX_DEPTH: usize = 2;
const MAX_TREE_MAX_DEPTH: usize = 8;
const MAX_TREE_NODES: usize = 1200;
const DEFAULT_READ_MAX_LINES: usize = 200;
const MAX_READ_MAX_LINES: usize = 2000;

/// 取访问上下文里的 workspace 作为 base，未注入时报错（fs 工具必须知道 workspace）。
fn base_of(access: &FsAccessContext) -> Result<PathBuf> {
    access
        .workspace
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("会话工作目录未注入，无法执行文件工具"))
}

/// 在工具结果 summary 头部回显工作目录与可写根（规范化绝对路径）。
///
/// 工作目录与允许目录不进入 system prompt（会话动态信息，cwd 迟落定/变更时
/// 改写已发送前缀会作废对话缓存），模型经 fs 工具调用获知。可写根为工作区
/// 与应用存储根（`~/.tiangong`），与写边界校验同源自 toolkit。写进 summary
/// 而非 stdout，避免污染 read_file 等工具的文件正文。无状态设计：每次调用
/// 都回显，按需 sidecar 无需维护"是否已告知"的会话状态。
pub fn annotate_workdir(mut resp: FsToolResponse, access: &FsAccessContext) -> FsToolResponse {
    if let Some(base) = access.workspace.as_deref() {
        let canonical = std::fs::canonicalize(base).unwrap_or_else(|_| PathBuf::from(base));
        let storage = shared::app_storage_root();
        resp.summary = format!(
            "工作目录：{}（另可写：{}）；{}",
            canonical.display(),
            storage.display(),
            resp.summary
        );
    }
    resp
}

// ── list_dir ─────────────────────────────────────────────────

pub fn handle_list_dir(req: ListDirRequest, policy: &dyn PathPolicy) -> FsToolResponse {
    let result = (|| {
        let base = base_of(&req.access)?;
        let path = req.path.as_deref().unwrap_or(".");
        let full_path = policy.resolve_read(path, &base)?;
        ensure_dir_target(&full_path)?;
        let mut items = Vec::new();
        for entry in fs::read_dir(&full_path)
            .with_context(|| format!("读取目录失败：{}", full_path.display()))?
        {
            let entry = entry?;
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().to_string();
            items.push(if is_dir { format!("{name}/") } else { name });
        }
        items.sort();
        Ok::<_, anyhow::Error>(FsToolResponse {
            ok: true,
            summary: format!(
                "目录列表：{}",
                shared::display_rel_path_with(&full_path, &base)
            ),
            stdout: items.join("\n"),
            exit_code: 0,
            ..Default::default()
        })
    })();
    result.unwrap_or_else(|e| error_response("list_dir", e))
}

// ── tree_dir ─────────────────────────────────────────────────

pub fn handle_tree_dir(req: TreeDirRequest, policy: &dyn PathPolicy) -> FsToolResponse {
    let result = (|| {
        let base = base_of(&req.access)?;
        let path = req.path.as_deref().unwrap_or(".");
        let max_depth = if req.max_depth == 0 {
            DEFAULT_TREE_MAX_DEPTH
        } else {
            req.max_depth.min(MAX_TREE_MAX_DEPTH)
        };
        let full_path = policy.resolve_read(path, &base)?;
        ensure_dir_target(&full_path)?;
        let rel = shared::display_rel_path_with(&full_path, &base);
        let mut lines = vec![if rel == "." {
            "./".to_string()
        } else {
            format!("{rel}/")
        }];
        let mut visited = 0usize;
        let mut truncated = false;
        append_tree_lines(
            &full_path,
            0,
            max_depth,
            "",
            &mut lines,
            &mut visited,
            &mut truncated,
        )?;
        if truncated {
            lines.push(format!(
                "...(节点数量超过限制，已截断，max_nodes={MAX_TREE_NODES})"
            ));
        }
        Ok(FsToolResponse {
            ok: true,
            summary: format!("目录树：{} (max_depth={max_depth})", rel),
            stdout: lines.join("\n"),
            exit_code: 0,
            ..Default::default()
        })
    })();
    result.unwrap_or_else(|e| error_response("tree_dir", e))
}

// ── read_file ────────────────────────────────────────────────

pub fn handle_read_file(req: ReadFileRequest, policy: &dyn PathPolicy) -> FsToolResponse {
    let result = (|| {
        let base = base_of(&req.access)?;
        let start_line = req.start_line.max(1);
        let max_lines = if req.max_lines == 0 {
            DEFAULT_READ_MAX_LINES
        } else {
            req.max_lines.clamp(1, MAX_READ_MAX_LINES)
        };
        let full_path = policy.resolve_read(&req.path, &base)?;
        // 信任模式解析对不存在路径静默放行，这里必须区分"不存在"与"类型不对"，
        // 否则文件缺失会被误报成"目标不是文件"，误导调用方反复重试。
        match fs::metadata(&full_path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(anyhow!("目标不是文件：{}", full_path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(anyhow!("文件不存在：{}", full_path.display()));
            }
            Err(e) => {
                return Err(anyhow!("无法访问目标文件：{}（{e}）", full_path.display()));
            }
        }
        let content = fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start_idx = (start_line - 1).min(total);
        let end_idx = (start_idx + max_lines).min(total);
        let stdout = lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(idx, line)| format!("{:>6}\t{}", start_idx + idx + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(FsToolResponse {
            ok: true,
            summary: format!(
                "已读取文件：{} (range={}..{}, total_lines={})",
                shared::display_rel_path_with(&full_path, &base),
                start_idx + 1,
                end_idx,
                total
            ),
            stdout,
            exit_code: 0,
            ..Default::default()
        })
    })();
    result.unwrap_or_else(|e| error_response("read_file", e))
}

// ── write_file ───────────────────────────────────────────────

pub fn handle_write_file(req: WriteFileRequest, policy: &dyn PathPolicy) -> FsToolResponse {
    let (locked, unlocked, result) = with_write_lock(
        &req.access,
        policy,
        std::slice::from_ref(&req.path),
        |base| {
            let full_path = policy.resolve_write(&req.path, base)?;
            if let Some(parent) = full_path.parent()
                && let Err(e) = fs::create_dir_all(parent)
                    .with_context(|| format!("创建目录失败：{}", parent.display()))
            {
                return Err(e);
            }
            if req.append {
                use std::fs::OpenOptions;
                use std::io::Write;
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full_path)
                    .with_context(|| format!("追加打开文件失败：{}", full_path.display()))?;
                file.write_all(req.content.as_bytes())
                    .with_context(|| format!("追加写入文件失败：{}", full_path.display()))?;
            } else {
                atomic_write_file(&full_path, req.content.as_bytes())?;
            }
            Ok(FsToolResponse {
                ok: true,
                summary: format!(
                    "文件写入成功：{} (mode={})",
                    shared::display_rel_path_with(&full_path, base),
                    if req.append {
                        "append"
                    } else {
                        "overwrite-atomic"
                    }
                ),
                stdout: format!("written_bytes={},append={}", req.content.len(), req.append),
                exit_code: 0,
                ..Default::default()
            })
        },
    );
    finalize_write(result, locked, unlocked)
}

// ── replace_in_file ──────────────────────────────────────────

pub fn handle_replace_in_file(
    req: ReplaceInFileRequest,
    policy: &dyn PathPolicy,
) -> FsToolResponse {
    let (locked, unlocked, result) = with_write_lock(
        &req.access,
        policy,
        std::slice::from_ref(&req.path),
        |base| {
            if req.old.is_empty() {
                return Err(anyhow!("replace_in_file old 参数不能为空"));
            }
            let full_path = policy.resolve_write(&req.path, base)?;
            if !full_path.is_file() {
                return Err(anyhow!("目标不是文件：{}", full_path.display()));
            }
            let content = fs::read_to_string(&full_path)
                .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
            let mut old = req.old.clone();
            let mut new = req.new.clone();
            let line_ending = preferred_line_ending(&content);
            if new.contains('\n') || new.contains('\r') {
                new = normalize_line_endings(&new, line_ending);
            }
            let mut count = content.matches(&old).count();
            if count == 0 && (old.contains('\n') || old.contains('\r')) {
                old = normalize_line_endings(&old, line_ending);
                count = content.matches(&old).count();
            }
            if count == 0 {
                return Err(anyhow!("未找到待替换内容"));
            }
            if let Some(expected) = req.expected_count
                && count != expected
            {
                return Err(anyhow!(
                    "命中数量不符合预期：expected={expected}, actual={count}"
                ));
            }
            let (replaced, replaced_count) = if req.replace_all {
                (content.replace(&old, &new), count)
            } else {
                if count != 1 {
                    return Err(anyhow!(
                        "默认仅允许单点替换，当前命中 {} 处；如需全量替换请设置 replace_all=true",
                        count
                    ));
                }
                (content.replacen(&old, &new, 1), 1)
            };
            fs::write(&full_path, replaced.as_bytes())
                .with_context(|| format!("写入替换结果失败：{}", full_path.display()))?;
            Ok(FsToolResponse {
                ok: true,
                summary: format!(
                    "文件替换成功：{} (replacements={}, replace_all={})",
                    shared::display_rel_path_with(&full_path, base),
                    replaced_count,
                    req.replace_all
                ),
                exit_code: 0,
                ..Default::default()
            })
        },
    );
    finalize_write(result, locked, unlocked)
}

// ── apply_patch ──────────────────────────────────────────────

pub fn handle_apply_patch(req: ApplyPatchRequest, policy: &dyn PathPolicy) -> FsToolResponse {
    // 先解析出本次补丁涉及的所有目标路径（用于全有或全无加锁）。
    let targets_result = (|| {
        let base = base_of(&req.access)?;
        let workdir_raw = req.workdir.as_deref();
        let effective_cwd = shared::resolve_effective_cwd_with(workdir_raw, &base)?;
        if req.patch.trim().is_empty() {
            return Err(anyhow!("apply_patch patch 内容不能为空"));
        }
        enumerate_patch_targets(&req.patch, &effective_cwd)
    })();
    let targets = match targets_result {
        Ok(t) => t,
        Err(e) => return error_response("apply_patch", e),
    };
    let target_strs: Vec<String> = targets
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let (locked, unlocked, result) = with_write_lock(&req.access, policy, &target_strs, |base| {
        let workdir_raw = req.workdir.as_deref();
        let effective_cwd = shared::resolve_effective_cwd_with(workdir_raw, base)?;
        let stats = apply_unified_diff_patch(&req.patch, &effective_cwd, req.verify)?;
        let summary = format!(
            "补丁{}成功：add={}, delete={}, update={}, move={}",
            if req.verify { "校验" } else { "应用" },
            stats.added,
            stats.deleted,
            stats.updated,
            stats.moved
        );
        let stdout = json!({
            "verify": req.verify,
            "effective_cwd": effective_cwd.display().to_string(),
            "counts": {
                "add": stats.added,
                "delete": stats.deleted,
                "update": stats.updated,
                "move": stats.moved,
            },
            "files": stats.files,
        })
        .to_string();
        Ok(FsToolResponse {
            ok: true,
            summary,
            stdout: shared::truncate_output(&stdout),
            exit_code: 0,
            ..Default::default()
        })
    });
    finalize_write(result, locked, unlocked)
}

// ── 写工具锁包装 ─────────────────────────────────────────────

/// 写工具统一锁包装：对目标路径加锁 → 执行业务 → 解锁。
///
/// 锁在本次调用内 acquire/release，与写操作绑定（一次 IPC 完成加锁+写+解锁，
/// 避免锁泄漏）。返回 `(locked_paths, unlocked_paths, 业务结果)`。
///
/// 注意：`raw_paths` 是**解析前**的原始路径字符串，需先用 policy 解析并
/// canonicalize_for_lock 规范化后才能加锁（与原实现一致，防软链接/.. 绕过）。
fn with_write_lock<F>(
    access: &FsAccessContext,
    policy: &dyn PathPolicy,
    raw_paths: &[String],
    body: F,
) -> (Vec<PathBuf>, Vec<PathBuf>, Result<FsToolResponse>)
where
    F: FnOnce(&Path) -> Result<FsToolResponse>,
{
    let base = match base_of(access) {
        Ok(b) => b,
        Err(e) => return (Vec::new(), Vec::new(), Err(e)),
    };

    // 解析并规范化每个写路径为锁表 key。用 policy（读解析即可，写路径在 body 内再 resolve_write）。
    // 注意：这里只为加锁取得稳定 key，用 resolve_write 解析真实写路径再 canonicalize_for_lock。
    let mut keys = Vec::with_capacity(raw_paths.len());
    for raw in raw_paths {
        match policy.resolve_write(raw, &base) {
            Ok(resolved) => keys.push(canonicalize_for_lock(&resolved)),
            Err(e) => return (Vec::new(), Vec::new(), Err(e)),
        }
    }

    if keys.is_empty() {
        // 无可加锁目标，直接执行。
        return (Vec::new(), Vec::new(), body(&base));
    }

    let now = chrono::Local::now().naive_local();
    let operation_id = match FileLockTable::acquire(&keys, &now) {
        Ok(id) => id,
        Err(e) => return (Vec::new(), Vec::new(), Err(anyhow!(e))),
    };
    let locked: Vec<PathBuf> = keys.clone();

    let result = body(&base);

    // 无论成功/失败都释放本次操作取得的锁。release 内部靠 operation_id 校验，
    // 若某路径已被新操作接管（旧操作超时后），不会误删，也不会出现在 released 里。
    let unlocked = FileLockTable::release(&keys, &operation_id);
    (locked, unlocked, result)
}

/// 把锁包装的结果组装成最终响应：业务结果失败时仍带上 locked/unlocked 路径，
/// 这样 wasm 侧无论业务成败都能正确发 locked/unlocked 事件。
fn finalize_write(
    result: Result<FsToolResponse>,
    locked: Vec<PathBuf>,
    unlocked: Vec<PathBuf>,
) -> FsToolResponse {
    let locked_str: Vec<String> = locked.iter().map(|p| p.display().to_string()).collect();
    let unlocked_str: Vec<String> = unlocked.iter().map(|p| p.display().to_string()).collect();
    match result {
        Ok(mut resp) => {
            resp.locked_paths = locked_str;
            resp.unlocked_paths = unlocked_str;
            resp
        }
        Err(e) => {
            let resp = error_response("write", e);
            FsToolResponse {
                locked_paths: locked_str,
                unlocked_paths: unlocked_str,
                ..resp
            }
        }
    }
}

// ── 辅助函数 ─────────────────────────────────────────────────

/// 目录类读工具的目标判定：区分目录不存在 / 目标不是目录 / 无法访问
/// （信任模式解析对不存在路径静默放行，不能依赖单一 is_dir 判定）。
fn ensure_dir_target(full_path: &Path) -> Result<()> {
    match fs::metadata(full_path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(anyhow!("目标不是目录：{}", full_path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow!("目录不存在：{}", full_path.display()))
        }
        Err(e) => Err(anyhow!("无法访问目标目录：{}（{e}）", full_path.display())),
    }
}

fn error_response(tool: &str, e: anyhow::Error) -> FsToolResponse {
    let summary = format!("{tool} 失败：{e}");
    FsToolResponse {
        ok: false,
        summary: summary.clone(),
        stderr: summary,
        exit_code: 1,
        ..Default::default()
    }
}

fn atomic_write_file(path: &Path, content: &[u8]) -> Result<()> {
    // TODO: 当前是 remove→rename 非原子写，与原 fs 实现一致。
    // 未来可考虑改为 rename 覆盖（Unix 原子），属行为变更需单独评估。
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("无法确定目标文件父目录：{}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("目标文件名非法：{}", path.display()))?;
    let temp_path = parent.join(format!(".{}.tmp-{}", file_name, scru128::new()));
    fs::write(&temp_path, content)
        .with_context(|| format!("写入临时文件失败：{}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("删除旧文件失败：{}", path.display()))?;
    }
    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "原子替换失败：temp={}, target={}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn append_tree_lines(
    path: &Path,
    current_depth: usize,
    max_depth: usize,
    prefix: &str,
    lines: &mut Vec<String>,
    visited: &mut usize,
    truncated: &mut bool,
) -> Result<()> {
    if current_depth >= max_depth {
        return Ok(());
    }
    if *visited >= MAX_TREE_NODES {
        *truncated = true;
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("读取目录失败：{}", path.display()))?
    {
        let entry = entry.with_context(|| format!("读取目录项失败：{}", path.display()))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry
            .file_type()
            .with_context(|| format!("读取目录项类型失败：{}", path.display()))?
            .is_dir();
        entries.push((name, is_dir, entry.path()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let total = entries.len();
    for (idx, (name, is_dir, child_path)) in entries.into_iter().enumerate() {
        if *visited >= MAX_TREE_NODES {
            *truncated = true;
            return Ok(());
        }

        *visited += 1;
        let last = idx + 1 == total;
        let branch = if last { "`-- " } else { "|-- " };
        let display = if is_dir { format!("{name}/") } else { name };
        lines.push(format!("{prefix}{branch}{display}"));

        if is_dir {
            let next_prefix = if last {
                format!("{prefix}    ")
            } else {
                format!("{prefix}|   ")
            };
            append_tree_lines(
                &child_path,
                current_depth + 1,
                max_depth,
                &next_prefix,
                lines,
                visited,
                truncated,
            )?;
            if *truncated {
                return Ok(());
            }
        }
    }
    Ok(())
}

// ── apply_patch 实现（unified diff）──────────────────────────

#[derive(Default)]
struct PatchStats {
    added: usize,
    deleted: usize,
    updated: usize,
    moved: usize,
    files: Vec<Value>,
}

fn apply_unified_diff_patch(patch: &str, effective_cwd: &Path, verify: bool) -> Result<PatchStats> {
    use diffy::{Patch, apply as diffy_apply};

    let sections = split_unified_diff_sections(patch)?;
    let mut stats = PatchStats::default();

    for section in &sections {
        let parsed =
            Patch::from_str(section).map_err(|err| anyhow!("解析 unified diff 失败：{err}"))?;
        let original = normalize_diff_filename(parsed.original().unwrap_or_default())?;
        let modified = normalize_diff_filename(parsed.modified().unwrap_or_default())?;

        let is_add = original == "/dev/null" && modified != "/dev/null";
        let is_delete = modified == "/dev/null" && original != "/dev/null";

        if is_add {
            let target = shared::resolve_write_path_from_base(&modified, effective_cwd)?;
            if target.exists() {
                return Err(anyhow!("新增文件已存在：{}", target.display()));
            }
            let content =
                diffy_apply("", &parsed).map_err(|err| anyhow!("新增文件补丁应用失败：{err}"))?;
            if !verify {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|err| anyhow!("创建目录失败：{}，{err}", parent.display()))?;
                }
                fs::write(&target, content.as_bytes())
                    .map_err(|err| anyhow!("写入新增文件失败：{}，{err}", target.display()))?;
            }
            stats.added += 1;
            stats.files.push(json!({
                "action": "add",
                "target": shared::display_rel_path_with(&target, effective_cwd),
            }));
            continue;
        }

        if is_delete {
            let source = shared::resolve_write_path_from_base(&original, effective_cwd)?;
            if !source.is_file() {
                return Err(anyhow!("删除目标不是文件：{}", source.display()));
            }
            let base_content = fs::read_to_string(&source)
                .map_err(|err| anyhow!("读取删除目标失败：{}，{err}", source.display()))?;
            let content = apply_text_patch(&base_content, &parsed, "删除补丁应用失败")?;
            if !content.is_empty() {
                return Err(anyhow!(
                    "删除补丁校验失败：应用后内容非空：{}",
                    source.display()
                ));
            }
            if !verify {
                fs::remove_file(&source)
                    .map_err(|err| anyhow!("删除文件失败：{}，{err}", source.display()))?;
            }
            stats.deleted += 1;
            stats.files.push(json!({
                "action": "delete",
                "source": shared::display_rel_path_with(&source, effective_cwd),
            }));
            continue;
        }

        let source = shared::resolve_write_path_from_base(&original, effective_cwd)?;
        let target = shared::resolve_write_path_from_base(&modified, effective_cwd)?;
        if !source.is_file() {
            return Err(anyhow!("修改目标不是文件：{}", source.display()));
        }
        let base_content = fs::read_to_string(&source)
            .map_err(|err| anyhow!("读取文件失败：{}，{err}", source.display()))?;
        let content = apply_text_patch(&base_content, &parsed, "修改补丁应用失败")?;

        if !verify {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|err| anyhow!("创建目录失败：{}，{err}", parent.display()))?;
            }
            fs::write(&target, content.as_bytes())
                .map_err(|err| anyhow!("写入文件失败：{}，{err}", target.display()))?;
            if source != target {
                fs::remove_file(&source)
                    .map_err(|err| anyhow!("删除原文件失败：{}，{err}", source.display()))?;
            }
        }

        stats.updated += 1;
        if source != target {
            stats.moved += 1;
        }
        stats.files.push(json!({
            "action": if source == target { "update" } else { "move_update" },
            "source": shared::display_rel_path_with(&source, effective_cwd),
            "target": shared::display_rel_path_with(&target, effective_cwd),
        }));
    }

    Ok(stats)
}

fn apply_text_patch(
    base_content: &str,
    patch: &diffy::Patch<'_, str>,
    error_context: &str,
) -> Result<String> {
    let crlf = uses_crlf_line_endings(base_content);
    let normalized;
    let base = if crlf {
        normalized = base_content.replace("\r\n", "\n");
        normalized.as_str()
    } else {
        base_content
    };
    let content = diffy::apply(base, patch).map_err(|err| anyhow!("{error_context}：{err}"))?;
    Ok(if crlf {
        content.replace('\n', "\r\n")
    } else {
        content
    })
}

fn preferred_line_ending(content: &str) -> &'static str {
    if uses_crlf_line_endings(content) {
        "\r\n"
    } else {
        "\n"
    }
}

fn normalize_line_endings(value: &str, line_ending: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', line_ending)
}

fn uses_crlf_line_endings(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut saw_line_ending = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\n' => {
                saw_line_ending = true;
                if index == 0 || bytes[index - 1] != b'\r' {
                    return false;
                }
            }
            b'\r' if bytes.get(index + 1) != Some(&b'\n') => return false,
            _ => {}
        }
    }
    saw_line_ending
}

fn split_unified_diff_sections(patch: &str) -> Result<Vec<String>> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.len() < 3 {
        return Err(anyhow!("unified diff 内容过短，无法解析"));
    }

    let mut section_starts = Vec::new();
    for idx in 0..(lines.len() - 1) {
        if lines[idx].starts_with("--- ") && lines[idx + 1].starts_with("+++ ") {
            section_starts.push(idx);
        }
    }
    if section_starts.is_empty() {
        return Err(anyhow!("unified diff 缺少文件头（--- / +++）"));
    }

    let mut sections = Vec::new();
    for (index, start) in section_starts.iter().enumerate() {
        let end = section_starts
            .get(index + 1)
            .copied()
            .unwrap_or(lines.len());
        let mut section = lines[*start..end].join("\n");
        if !section.ends_with('\n') {
            section.push('\n');
        }
        sections.push(section);
    }
    Ok(sections)
}

/// 枚举 patch 涉及的所有目标路径（写前加锁用）。
///
/// - add：目标 = modified
/// - delete：目标 = original
/// - update / move：目标 = original + modified（move 涉及删旧建新两个路径）
fn enumerate_patch_targets(patch: &str, effective_cwd: &Path) -> Result<Vec<PathBuf>> {
    use diffy::Patch;

    let sections = split_unified_diff_sections(patch)?;
    let mut targets = Vec::new();
    for section in &sections {
        let parsed =
            Patch::from_str(section).map_err(|err| anyhow!("解析 unified diff 失败：{err}"))?;
        let original = normalize_diff_filename(parsed.original().unwrap_or_default())?;
        let modified = normalize_diff_filename(parsed.modified().unwrap_or_default())?;

        let is_add = original == "/dev/null" && modified != "/dev/null";
        let is_delete = modified == "/dev/null" && original != "/dev/null";

        if is_add {
            targets.push(shared::resolve_write_path_from_base(
                &modified,
                effective_cwd,
            )?);
        } else if is_delete {
            targets.push(shared::resolve_write_path_from_base(
                &original,
                effective_cwd,
            )?);
        } else {
            targets.push(shared::resolve_write_path_from_base(
                &original,
                effective_cwd,
            )?);
            if original != modified {
                targets.push(shared::resolve_write_path_from_base(
                    &modified,
                    effective_cwd,
                )?);
            }
        }
    }
    Ok(targets)
}

fn normalize_diff_filename(raw: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径为空"));
    }
    if path == "/dev/null" {
        return Ok(path.to_string());
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .trim();
    if path.is_empty() {
        return Err(anyhow!("unified diff 文件路径非法"));
    }
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path_policy::TrustModePathPolicy;

    fn access(root: &Path) -> FsAccessContext {
        FsAccessContext {
            workspace: Some(root.to_string_lossy().into_owned()),
            full_trust: false,
        }
    }

    #[test]
    fn list_dir_missing_path_reports_not_found() {
        let root = tempfile::tempdir().unwrap();

        let response = handle_list_dir(
            ListDirRequest {
                path: Some("docs".to_string()),
                access: access(root.path()),
            },
            &TrustModePathPolicy::new(true),
        );

        assert!(!response.ok);
        assert!(
            response.stderr.contains("目录不存在"),
            "stderr={}",
            response.stderr
        );
    }

    #[test]
    fn list_dir_file_target_reports_not_dir() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README.md"), "hi\n").unwrap();

        let response = handle_list_dir(
            ListDirRequest {
                path: Some("README.md".to_string()),
                access: access(root.path()),
            },
            &TrustModePathPolicy::new(true),
        );

        assert!(!response.ok);
        assert!(
            response.stderr.contains("目标不是目录"),
            "stderr={}",
            response.stderr
        );
    }

    #[test]
    fn read_file_missing_path_reports_not_found() {
        let root = tempfile::tempdir().unwrap();

        let response = handle_read_file(
            ReadFileRequest {
                path: "docs/requirements.md".to_string(),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(true),
        );

        assert!(!response.ok);
        assert!(
            response.stderr.contains("文件不存在"),
            "stderr={}",
            response.stderr
        );
    }

    #[test]
    fn read_file_directory_target_reports_not_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();

        let response = handle_read_file(
            ReadFileRequest {
                path: "docs".to_string(),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(true),
        );

        assert!(!response.ok);
        assert!(
            response.stderr.contains("目标不是文件"),
            "stderr={}",
            response.stderr
        );
    }

    #[test]
    fn read_file_reads_markdown_content() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("README.md"), "第一行\n第二行\n").unwrap();

        let response = handle_read_file(
            ReadFileRequest {
                path: "README.md".to_string(),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(true),
        );

        assert!(response.ok, "{}", response.stderr);
        assert!(response.stdout.contains("第一行"));
    }

    #[test]
    fn replace_in_file_matches_lf_text_and_preserves_crlf() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("windows.txt");
        fs::write(&path, b"alpha\r\nbeta\r\ngamma\r\n").unwrap();

        let response = handle_replace_in_file(
            ReplaceInFileRequest {
                path: "windows.txt".to_string(),
                old: "alpha\nbeta".to_string(),
                new: "alpha\nBETA".to_string(),
                expected_count: Some(1),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(false),
        );

        assert!(response.ok, "{}", response.stderr);
        assert_eq!(fs::read(&path).unwrap(), b"alpha\r\nBETA\r\ngamma\r\n");
    }

    #[test]
    fn replace_in_file_matches_crlf_text_and_preserves_lf() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unix.txt");
        fs::write(&path, b"alpha\nbeta\ngamma\n").unwrap();

        let response = handle_replace_in_file(
            ReplaceInFileRequest {
                path: "unix.txt".to_string(),
                old: "alpha\r\nbeta".to_string(),
                new: "alpha\r\nBETA".to_string(),
                expected_count: Some(1),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(false),
        );

        assert!(response.ok, "{}", response.stderr);
        assert_eq!(fs::read(&path).unwrap(), b"alpha\nBETA\ngamma\n");
    }

    #[test]
    fn apply_patch_updates_and_preserves_crlf() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("windows.txt");
        fs::write(&path, b"alpha\r\nbeta\r\ngamma\r\n").unwrap();
        let patch =
            "--- a/windows.txt\n+++ b/windows.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";

        let response = handle_apply_patch(
            ApplyPatchRequest {
                patch: patch.to_string(),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(false),
        );

        assert!(response.ok, "{}", response.stderr);
        assert_eq!(fs::read(&path).unwrap(), b"alpha\r\nBETA\r\ngamma\r\n");
    }

    #[test]
    fn apply_patch_updates_and_preserves_lf() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unix.txt");
        fs::write(&path, b"alpha\nbeta\ngamma\n").unwrap();
        let patch =
            "--- a/unix.txt\n+++ b/unix.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n";

        let response = handle_apply_patch(
            ApplyPatchRequest {
                patch: patch.to_string(),
                access: access(root.path()),
                ..Default::default()
            },
            &TrustModePathPolicy::new(false),
        );

        assert!(response.ok, "{}", response.stderr);
        assert_eq!(fs::read(&path).unwrap(), b"alpha\nBETA\ngamma\n");
    }
}

// ── @提及文件候选 ─────────────────────────────────────────────

/// 候选检索默认与上限数量。
const DEFAULT_MENTION_LIMIT: usize = 50;
const MAX_MENTION_LIMIT: usize = 200;
/// 遍历节点上限：输入框补全是交互路径，宁可截断也不能卡住。
const MAX_MENTION_SCAN_NODES: usize = 20_000;
/// 遍历深度上限，避免深层目录树拖慢补全。
const MAX_MENTION_DEPTH: usize = 12;

/// 补全阶段默认跳过的目录：体量大且几乎不会被 @ 引用。
const MENTION_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// 在工作区内按查询词检索文件候选。
///
/// 只枚举路径、不读取内容；范围严格限定在工作区内（不跟随符号链接外跳），
/// 因此不依赖信任模式放宽读边界。
pub fn handle_mention_files(req: MentionFilesRequest) -> MentionFilesResponse {
    let Some(workspace) = req.access.workspace.as_deref() else {
        // 没有工作区（如全局草稿）时无文件候选，不报错。
        return MentionFilesResponse::default();
    };
    let root = match std::fs::canonicalize(workspace) {
        Ok(root) if root.is_dir() => root,
        _ => return MentionFilesResponse::default(),
    };
    let limit = if req.limit == 0 {
        DEFAULT_MENTION_LIMIT
    } else {
        req.limit.min(MAX_MENTION_LIMIT)
    };
    let needle = req.query.trim().to_lowercase();

    let mut candidates = Vec::new();
    let mut visited = 0usize;
    collect_mention_files(
        &root,
        &root,
        0,
        &needle,
        limit,
        &mut visited,
        &mut candidates,
    );
    // 先按路径深度、再按字典序：浅层文件更可能是用户想要的。
    candidates.sort_by(|left, right| {
        let left_depth = left.relative_path.matches('/').count();
        let right_depth = right.relative_path.matches('/').count();
        left_depth
            .cmp(&right_depth)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    candidates.truncate(limit);
    MentionFilesResponse { candidates }
}

fn collect_mention_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    needle: &str,
    limit: usize,
    visited: &mut usize,
    out: &mut Vec<MentionFileCandidate>,
) {
    if depth >= MAX_MENTION_DEPTH || *visited >= MAX_MENTION_SCAN_NODES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        // 无权限或已删除的目录直接跳过，不影响其余候选。
        return;
    };
    let mut sub_dirs = Vec::new();
    for entry in entries.flatten() {
        if *visited >= MAX_MENTION_SCAN_NODES {
            return;
        }
        *visited += 1;
        let name = entry.file_name().to_string_lossy().to_string();
        // 不跟随符号链接：避免跳出工作区或陷入循环。
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if MENTION_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            sub_dirs.push(entry.path());
            continue;
        }
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        // 跨平台统一用 `/`：候选 token 在不同系统间可原样往返。
        let relative_path = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if relative_path.is_empty() {
            continue;
        }
        // 查询词同时匹配文件名与相对路径，便于按目录筛选。
        if !needle.is_empty()
            && !relative_path.to_lowercase().contains(needle)
            && !name.to_lowercase().contains(needle)
        {
            continue;
        }
        out.push(MentionFileCandidate {
            relative_path,
            file_name: name,
        });
    }
    // 已收集足够多的浅层候选时不再深入（排序后仍会截断）。
    if out.len() >= limit.saturating_mul(4) {
        return;
    }
    sub_dirs.sort();
    for sub_dir in sub_dirs {
        collect_mention_files(root, &sub_dir, depth + 1, needle, limit, visited, out);
    }
}

#[cfg(test)]
mod mention_files_tests {
    use super::*;

    fn access(workspace: &Path) -> FsAccessContext {
        FsAccessContext {
            workspace: Some(workspace.to_string_lossy().into_owned()),
            full_trust: false,
        }
    }

    fn request(workspace: &Path, query: &str) -> MentionFilesRequest {
        MentionFilesRequest {
            query: query.to_string(),
            limit: 0,
            access: access(workspace),
        }
    }

    fn paths(response: &MentionFilesResponse) -> Vec<String> {
        response
            .candidates
            .iter()
            .map(|candidate| candidate.relative_path.clone())
            .collect()
    }

    #[test]
    fn 按查询词匹配文件名与相对路径() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/core")).unwrap();
        fs::write(root.path().join("src/core/session.rs"), "x").unwrap();
        fs::write(root.path().join("README.md"), "x").unwrap();

        let by_name = handle_mention_files(request(root.path(), "session"));
        assert_eq!(paths(&by_name), vec!["src/core/session.rs"]);

        // 查询词也可命中目录片段，便于按目录筛选。
        let by_dir = handle_mention_files(request(root.path(), "src/core"));
        assert_eq!(paths(&by_dir), vec!["src/core/session.rs"]);

        // 空查询返回工作区内文件，浅层优先。
        let all = handle_mention_files(request(root.path(), ""));
        assert_eq!(paths(&all), vec!["README.md", "src/core/session.rs"]);
    }

    #[test]
    fn 跳过体量大的常见目录() {
        let root = tempfile::tempdir().unwrap();
        for skipped in [".git", "node_modules", "target"] {
            fs::create_dir_all(root.path().join(skipped)).unwrap();
            fs::write(root.path().join(skipped).join("noise.rs"), "x").unwrap();
        }
        fs::write(root.path().join("main.rs"), "x").unwrap();

        let response = handle_mention_files(request(root.path(), ""));
        assert_eq!(paths(&response), vec!["main.rs"]);
    }

    #[test]
    fn 支持空格与中文路径且分隔符统一() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("设计 文档")).unwrap();
        fs::write(root.path().join("设计 文档/需求 说明.md"), "x").unwrap();

        let response = handle_mention_files(request(root.path(), "需求"));
        assert_eq!(paths(&response), vec!["设计 文档/需求 说明.md"]);
        assert_eq!(response.candidates[0].file_name, "需求 说明.md");
    }

    #[test]
    fn 无工作区时无候选且不报错() {
        let response = handle_mention_files(MentionFilesRequest::default());
        assert!(response.candidates.is_empty());
    }

    #[test]
    fn 不跟随符号链接避免跳出工作区() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "x").unwrap();
        fs::write(root.path().join("inside.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

        let response = handle_mention_files(request(root.path(), ""));
        assert_eq!(paths(&response), vec!["inside.txt"]);
    }

    #[test]
    fn 数量上限生效() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..30 {
            fs::write(root.path().join(format!("file-{index}.txt")), "x").unwrap();
        }
        let response = handle_mention_files(MentionFilesRequest {
            query: String::new(),
            limit: 5,
            access: access(root.path()),
        });
        assert_eq!(response.candidates.len(), 5);
    }
}
