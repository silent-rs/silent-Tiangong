use std::process::Command;

use anyhow::{Context, Result, anyhow};

use tiangong_core::app_state::TiangongState;

use crate::args::{PromptArgs, PromptSubcommand};

pub(crate) fn run_prompt_command(args: PromptArgs) -> Result<()> {
    let mut state = TiangongState::load_or_default();
    match args.command {
        PromptSubcommand::Show => {
            let prompt = state.custom_system_prompt();
            if prompt.trim().is_empty() {
                println!("（未设置自定义 Prompt）");
            } else {
                let char_count = prompt.chars().count();
                println!("{prompt}");
                println!("\n— 共 {char_count} 字 —");
            }
        }
        PromptSubcommand::Set { text, file } => {
            let content = match (text, file) {
                (Some(text), None) => text,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .with_context(|| format!("读取 Prompt 文件失败：{path}"))?,
                _ => {
                    return Err(anyhow!("请提供 Prompt 文本或使用 --file 指定文件"));
                }
            };
            let char_count = content.chars().count();
            state.set_custom_system_prompt(content)?;
            println!("自定义 Prompt 已保存（{char_count} 字）");
        }
        PromptSubcommand::Edit => {
            let content = edit_via_editor(state.custom_system_prompt().to_string())?;
            let char_count = content.chars().count();
            state.set_custom_system_prompt(content)?;
            println!("自定义 Prompt 已保存（{char_count} 字）");
        }
        PromptSubcommand::Clear => {
            state.set_custom_system_prompt(String::new())?;
            println!("自定义 Prompt 已清空");
        }
        PromptSubcommand::Path => {
            let path = state.custom_prompt_path();
            println!("{}", path.display());
        }
    }
    Ok(())
}

/// 通过 $EDITOR（回退 vim / nano）编辑 Prompt 内容，返回编辑后的文本。
fn edit_via_editor(initial: String) -> Result<String> {
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let candidates: Vec<String> = match editor {
        Some(editor) => vec![editor],
        None => vec!["vim".to_string(), "nano".to_string()],
    };

    // 写入临时文件
    let mut tmp_path = std::env::temp_dir();
    tmp_path.push(format!("tiangong-prompt-{}.md", temp_name()));
    std::fs::write(&tmp_path, &initial)
        .with_context(|| format!("写入临时文件失败：{}", tmp_path.display()))?;

    let mut launched = false;
    for candidate in &candidates {
        let mut cmd = Command::new(candidate);
        cmd.arg(&tmp_path);
        let status = match cmd.status() {
            Ok(status) => status,
            Err(_) => continue, // 编辑器不存在，尝试下一个
        };
        if !status.success() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(anyhow!("编辑器 {candidate} 退出码异常"));
        }
        launched = true;
        break;
    }

    if !launched {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(anyhow!(
            "未找到可用编辑器（$EDITOR 未设置，且 vim/nano 不可用）。请使用 `tiangong prompt set --file <路径>` 或设置 $EDITOR 环境变量。"
        ));
    }

    let content = std::fs::read_to_string(&tmp_path)
        .with_context(|| format!("读取编辑后文件失败：{}", tmp_path.display()))?;
    let _ = std::fs::remove_file(&tmp_path);
    Ok(content)
}

/// 生成临时唯一标识（用 pid + 纳秒时间戳，避免引入额外依赖）。
fn temp_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}
