use std::fs;

use anyhow::{Context, Result, anyhow};

use super::common::{display_rel_path, resolve_workspace_write_path};
use super::{LocalToolExecutor, ToolCall, ToolResult};

impl LocalToolExecutor {
    pub(super) fn replace_in_file(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .first()
            .ok_or_else(|| anyhow!("replace_in_file 缺少路径参数"))?;
        let old = call
            .args
            .get(1)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 old 参数"))?;
        let new = call
            .args
            .get(2)
            .ok_or_else(|| anyhow!("replace_in_file 缺少 new 参数"))?;
        let replace_all = parse_bool_flag(call.args.get(3).map(String::as_str), false)?;
        let expected_count = parse_optional_usize(call.args.get(4).map(String::as_str))?;
        if old.is_empty() {
            return Err(anyhow!("replace_in_file old 参数不能为空"));
        }

        let full_path = resolve_workspace_write_path(path)?;
        if !full_path.is_file() {
            return Err(anyhow!(
                "replace_in_file 目标不是文件：{}",
                full_path.display()
            ));
        }

        let content = fs::read_to_string(&full_path)
            .with_context(|| format!("读取文件失败：{}", full_path.display()))?;
        let count = content.matches(old).count();
        if count == 0 {
            return Err(anyhow!("replace_in_file 未找到待替换内容"));
        }
        if let Some(expected_count) = expected_count
            && count != expected_count
        {
            return Err(anyhow!(
                "replace_in_file 命中数量不符合预期：expected={}, actual={}",
                expected_count,
                count
            ));
        }

        let (replaced, replaced_count) = if replace_all {
            (content.replace(old, new), count)
        } else {
            if count != 1 {
                return Err(anyhow!(
                    "replace_in_file 默认仅允许单点替换，当前命中 {} 处；如需全量替换请设置 replace_all=true",
                    count
                ));
            }
            (content.replacen(old, new, 1), 1)
        };
        fs::write(&full_path, replaced.as_bytes())
            .with_context(|| format!("写入替换结果失败：{}", full_path.display()))?;

        Ok(ToolResult {
            ok: true,
            summary: format!(
                "文件替换成功：{} (replacements={}, replace_all={})",
                display_rel_path(&full_path),
                replaced_count,
                replace_all
            ),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            execution: None,
        })
    }
}

fn parse_bool_flag(raw: Option<&str>, default: bool) -> Result<bool> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(default);
    };
    match text.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(anyhow!("replace_in_file 布尔参数非法：{text}")),
    }
}

fn parse_optional_usize(raw: Option<&str>) -> Result<Option<usize>> {
    let Some(text) = raw.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let value = text
        .parse::<usize>()
        .with_context(|| format!("replace_in_file expected_count 参数非法：{text}"))?;
    Ok(Some(value))
}
