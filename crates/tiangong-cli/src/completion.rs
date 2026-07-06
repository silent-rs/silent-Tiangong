#![allow(dead_code)]

use std::path::Path;

/// 补全候选项
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CompletionCandidate {
    /// 补全后的完整文本（替换触发词）
    pub value: String,
    /// 显示用的标签
    pub label: String,
    /// 简短描述
    pub hint: String,
}

/// 补全上下文类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionTrigger {
    /// `/` 命令补全
    SlashCommand,
    /// `@` 提及补全
    AtMention,
}

/// 斜杠命令定义
struct SlashCommandDef {
    name: &'static str,
    hint: &'static str,
}

const SLASH_COMMANDS: &[SlashCommandDef] = &[
    SlashCommandDef {
        name: "/exit",
        hint: "退出 REPL",
    },
    SlashCommandDef {
        name: "/quit",
        hint: "退出 REPL",
    },
    SlashCommandDef {
        name: "/new",
        hint: "新建会话",
    },
    SlashCommandDef {
        name: "/history",
        hint: "查看会话历史",
    },
    SlashCommandDef {
        name: "/model",
        hint: "查看/切换模型",
    },
    SlashCommandDef {
        name: "/mcp",
        hint: "查看 MCP server 列表",
    },
    SlashCommandDef {
        name: "/skill",
        hint: "查看 Skill 列表",
    },
    SlashCommandDef {
        name: "/sessions",
        hint: "会话管理（同 /history）",
    },
    SlashCommandDef {
        name: "/cancel",
        hint: "取消当前任务",
    },
    SlashCommandDef {
        name: "/config",
        hint: "查看/修改配置",
    },
    SlashCommandDef {
        name: "/help",
        hint: "显示帮助信息",
    },
];

/// 检测输入中的补全���发点
///
/// `cursor` 为字符偏移（非字节偏移）。
/// 返回 (触发类型, 触发词起始位置（字符偏移）, 当��已输入的前缀)
pub fn detect_trigger(input: &str, cursor: usize) -> Option<(CompletionTrigger, usize, String)> {
    // 将字符偏移转为字节偏移，安全切片
    let byte_cursor = input
        .char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let before_cursor = &input[..byte_cursor];

    // 从光标向左扫描，找到最近的触发字符
    // `@` 提及：光标前的 @word 片段
    if let Some(at_byte_pos) = before_cursor.rfind('@') {
        let prefix = &before_cursor[at_byte_pos..];
        // @ 必须在行首或前面是空���
        if at_byte_pos == 0 || before_cursor.as_bytes()[at_byte_pos - 1] == b' ' {
            // 确保 @ 后没有空格（在输入中间截断的提及）
            if !prefix[1..].contains(' ') {
                let at_char_pos = before_cursor[..at_byte_pos].chars().count();
                return Some((
                    CompletionTrigger::AtMention,
                    at_char_pos,
                    prefix[1..].to_string(),
                ));
            }
        }
    }

    // `/` 命令：仅在行首
    if before_cursor.starts_with('/') && !before_cursor.contains(' ') {
        return Some((
            CompletionTrigger::SlashCommand,
            0,
            before_cursor.to_string(),
        ));
    }

    None
}

/// 生成补全候选列表
pub fn complete(
    trigger: CompletionTrigger,
    prefix: &str,
    skill_plugin: &tiangong_plugin_skill::SkillPlugin,
    mcp_plugin: &tiangong_plugin_mcp::McpPlugin,
) -> Vec<CompletionCandidate> {
    match trigger {
        CompletionTrigger::SlashCommand => complete_slash_commands(prefix),
        CompletionTrigger::AtMention => complete_at_mentions(prefix, skill_plugin, mcp_plugin),
    }
}

fn complete_slash_commands(prefix: &str) -> Vec<CompletionCandidate> {
    SLASH_COMMANDS
        .iter()
        .filter(|cmd| cmd.name.starts_with(prefix))
        .map(|cmd| CompletionCandidate {
            value: cmd.name.to_string(),
            label: cmd.name.to_string(),
            hint: cmd.hint.to_string(),
        })
        .collect()
}

fn complete_at_mentions(
    prefix: &str,
    skill_plugin: &tiangong_plugin_skill::SkillPlugin,
    mcp_plugin: &tiangong_plugin_mcp::McpPlugin,
) -> Vec<CompletionCandidate> {
    let mut candidates = Vec::new();

    // @file: 提及补全 - 列出当前目录文件
    candidates.extend(complete_files(prefix));

    // @skill: 提及补全（skill 数据由 skill plugin 自管）
    for skill in skill_plugin.installed_skills() {
        if !skill.enabled {
            continue;
        }
        let label = format!("skill:{}", skill.id);
        if label.starts_with(prefix) || skill.id.starts_with(prefix) || prefix.is_empty() {
            candidates.push(CompletionCandidate {
                value: format!("@skill:{}", skill.id),
                label: format!("@skill:{}", skill.id),
                hint: format!("Skill - {}@{}", skill.name, skill.version),
            });
        }
    }

    // @mcp: 提及补全（MCP servers 由 mcp plugin 自管）
    for server in mcp_plugin.mcp_servers() {
        if !server.enabled {
            continue;
        }
        let label = format!("mcp:{}", server.name);
        if label.starts_with(prefix) || server.name.starts_with(prefix) || prefix.is_empty() {
            candidates.push(CompletionCandidate {
                value: format!("@mcp:{}", server.name),
                label: format!("@mcp:{}", server.name),
                hint: format!("MCP - {}", server.name),
            });
        }
    }

    candidates
}

fn complete_files(prefix: &str) -> Vec<CompletionCandidate> {
    let file_prefix = prefix.strip_prefix("file:").unwrap_or(prefix);
    let query = file_prefix.to_lowercase();

    // 如果包含 /，从指定目录搜索
    let (search_dir, name_query) = if file_prefix.contains('/') {
        let path = Path::new(file_prefix);
        let parent = path.parent().unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        (parent.to_path_buf(), name)
    } else {
        (
            std::env::current_dir().unwrap_or_else(|_| ".".into()),
            query.clone(),
        )
    };

    let mut candidates = Vec::new();

    // 当前目录文件（前缀匹配 + 模糊匹配）
    collect_files_from_dir(&search_dir, &name_query, "", &mut candidates, 30);

    // 如果没有 /，递归子目录模糊搜索（限制深度 2）
    if !file_prefix.contains('/') && !name_query.is_empty() {
        collect_files_recursive(&search_dir, &name_query, "", &mut candidates, 30, 0, 2);
    }

    candidates
}

/// 从目录收集匹配的文件（前缀 + 模糊）
fn collect_files_from_dir(
    dir: &Path,
    query: &str,
    rel_prefix: &str,
    candidates: &mut Vec<CompletionCandidate>,
    max: usize,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        if candidates.len() >= max {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let name_lower = name.to_lowercase();
        // 模糊匹配：包含子串
        if !query.is_empty() && !name_lower.contains(query) {
            continue;
        }

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let rel_path = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{rel_prefix}/{name}")
        };
        let display = if is_dir {
            format!("{rel_path}/")
        } else {
            rel_path.clone()
        };

        candidates.push(CompletionCandidate {
            value: format!("@file:{rel_path}"),
            label: format!("@file:{display}"),
            hint: if is_dir {
                "目录".into()
            } else {
                "文件".into()
            },
        });
    }
}

/// 递归搜索子目录（模糊匹配文件名）
fn collect_files_recursive(
    dir: &Path,
    query: &str,
    rel_prefix: &str,
    candidates: &mut Vec<CompletionCandidate>,
    max: usize,
    depth: usize,
    max_depth: usize,
) {
    if depth >= max_depth || candidates.len() >= max {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        if candidates.len() >= max {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }

        let rel_path = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{rel_prefix}/{name}")
        };

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            // 先检查子目录中的文件
            collect_files_from_dir(&entry.path(), query, &rel_path, candidates, max);
            // 递归
            collect_files_recursive(
                &entry.path(),
                query,
                &rel_path,
                candidates,
                max,
                depth + 1,
                max_depth,
            );
        }
    }
}

/// 格式化帮助信息（显示所有命令和 @ 提及类型）
pub fn help_text() -> String {
    let mut lines = vec!["可用命令：".to_string()];
    for cmd in SLASH_COMMANDS {
        lines.push(format!("  {:<12} {}", cmd.name, cmd.hint));
    }
    lines.push(String::new());
    lines.push("@ 提及：".to_string());
    lines.push("  @file:<路径>       引用文件".to_string());
    lines.push("  @skill:<id>       引用 Skill".to_string());
    lines.push("  @mcp:<名称>       引用 MCP server".to_string());
    lines.join("\n")
}
