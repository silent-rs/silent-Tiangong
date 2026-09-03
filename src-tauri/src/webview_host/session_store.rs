//! 旧版浏览器会话标签文件清理。
//!
//! 浏览器标签现在只保存在当前应用进程中。这里不再提供读取或写入能力，
//! 仅在物理删除会话时清理旧版本留下的 `browser-sessions` 文件。

use std::path::Path;

use anyhow::{Context, Result};

pub struct BrowserSessionStore;

impl BrowserSessionStore {
    /// 删除会话对应的旧式文件及 WebView 插件作用域文件。
    pub fn remove(session_id: &str) -> Result<()> {
        Self::remove_at(&tiangong_config::io::storage_root(), session_id)
    }

    fn remove_at(root: &Path, session_id: &str) -> Result<()> {
        let directory = root.join("browser-sessions");
        if !directory
            .try_exists()
            .with_context(|| format!("检查浏览器会话状态目录失败：{}", directory.display()))?
        {
            return Ok(());
        }

        let direct_name = format!("{}.json", sanitize(session_id));
        let scoped_suffix = format!("_{direct_name}");
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("读取浏览器会话状态目录失败：{}", directory.display()))?
        {
            let entry = entry
                .with_context(|| format!("读取浏览器会话状态条目失败：{}", directory.display()))?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            let is_direct = file_name == direct_name;
            let is_webview_scope =
                file_name.starts_with("webview_") && file_name.ends_with(&scoped_suffix);
            if !is_direct && !is_webview_scope {
                continue;
            }

            let path = entry.path();
            if path.is_file() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("删除浏览器会话状态失败：{}", path.display()))?;
            }
        }
        Ok(())
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}
