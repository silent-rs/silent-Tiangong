use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::db::MemoryDb;

const RECOVERY_MARKER: &str = ".plugin-data-recovered";

/// 恢复短暂写入插件私有目录的 Memory 数据。
///
/// 仅当来源包含真实记忆、当前标准目录没有节点和其他有效数据时切换。
/// 返回被保留的空目录备份路径；目标原本不存在时返回 `None`。
pub fn recover_plugin_data_dir(source: &Path) -> Result<Option<PathBuf>> {
    let target = crate::paths::memory_data_dir();
    if source == target
        || source.join(RECOVERY_MARKER).exists()
        || !source.join("metadata.db").is_file()
    {
        return Ok(None);
    }

    let source_count = count_nodes(source)
        .with_context(|| format!("核对待恢复 Memory 数据失败: {}", source.display()))?;
    if source_count == 0 {
        return Ok(None);
    }

    if target.join(RECOVERY_MARKER).exists() {
        return Ok(None);
    }

    let target_count = if target.join("metadata.db").is_file() {
        count_nodes(&target)
            .with_context(|| format!("核对当前 Memory 数据失败: {}", target.display()))?
    } else {
        0
    };
    if target_count > 0 || has_meaningful_data(&target)? {
        tracing::warn!(
            source = %source.display(),
            target = %target.display(),
            source_count,
            target_count,
            "检测到两处 Memory 数据，保留当前标准目录并跳过自动恢复"
        );
        return Ok(None);
    }

    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Memory 数据目录缺少父目录: {}", target.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("创建 Memory 数据父目录失败: {}", parent.display()))?;

    let transaction_id = scru128::new().to_string();
    let staged = parent.join(format!(".memory-recovery-{transaction_id}"));
    let backup = parent.join(format!("memory.pre-recovery-{transaction_id}"));
    copy_data_directory(source, &staged)?;
    std::fs::write(
        staged.join(RECOVERY_MARKER),
        format!("source={}\n", source.display()),
    )
    .with_context(|| "写入 Memory 数据恢复标记失败")?;

    let backup_path = if target.exists() {
        std::fs::rename(&target, &backup).with_context(|| {
            format!(
                "备份当前 Memory 数据目录失败: {} -> {}",
                target.display(),
                backup.display()
            )
        })?;
        Some(backup.clone())
    } else {
        None
    };

    if let Err(error) = std::fs::rename(&staged, &target) {
        if let Some(backup) = &backup_path {
            let _ = std::fs::rename(backup, &target);
        }
        return Err(error).with_context(|| {
            format!(
                "启用恢复后的 Memory 数据失败: {} -> {}",
                staged.display(),
                target.display()
            )
        });
    }

    if let Err(error) = std::fs::write(
        source.join(RECOVERY_MARKER),
        format!("target={}\n", target.display()),
    ) {
        tracing::warn!(%error, path = %source.display(), "写入 Memory 来源恢复标记失败");
    }

    tracing::info!(
        source = %source.display(),
        target = %target.display(),
        source_count,
        backup = backup_path.as_ref().map(|path| path.display().to_string()),
        "已恢复改造期间分流的 Memory 数据"
    );
    Ok(backup_path)
}

fn count_nodes(data_dir: &Path) -> Result<usize> {
    let database = MemoryDb::open_at_data_dir(data_dir)?;
    database.count_memory_nodes(None, None, None, None)
}

fn has_meaningful_data(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if !path.is_dir() {
        bail!("Memory 数据路径不是目录: {}", path.display());
    }

    for entry in std::fs::read_dir(path)
        .with_context(|| format!("读取 Memory 数据目录失败: {}", path.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            "metadata.db"
                | "metadata.db-wal"
                | "metadata.db-shm"
                | "leader.json"
                | "leader.lock"
                | "tantivy_index"
        ) || name.starts_with(".leader.json.")
        {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

fn copy_data_directory(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir(destination).with_context(|| {
        format!(
            "创建 Memory 数据恢复临时目录失败: {}",
            destination.display()
        )
    })?;

    for entry in std::fs::read_dir(source)
        .with_context(|| format!("读取待恢复 Memory 数据失败: {}", source.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            "leader.json" | "leader.lock" | "runtime" | RECOVERY_MARKER
        ) || name_text.starts_with(".leader.json.")
        {
            continue;
        }
        copy_entry(&entry.path(), &destination.join(name))?;
    }
    Ok(())
}

fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let file_type = std::fs::symlink_metadata(source)?.file_type();
    if file_type.is_symlink() {
        bail!("Memory 数据目录不允许符号链接: {}", source.display());
    }
    if file_type.is_dir() {
        std::fs::create_dir(destination)
            .with_context(|| format!("创建 Memory 恢复目录失败: {}", destination.display()))?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if file_type.is_file() {
        std::fs::copy(source, destination).with_context(|| {
            format!(
                "复制 Memory 数据失败: {} -> {}",
                source.display(),
                destination.display()
            )
        })?;
        return Ok(());
    }
    bail!("Memory 数据包含不支持的文件类型: {}", source.display())
}
