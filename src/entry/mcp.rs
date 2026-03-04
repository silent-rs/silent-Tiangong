use crate::core::app_state::{RegisterMcpServerOptions, TiangongState};

use super::args::{McpArgs, McpSubcommand};

pub(super) fn run_mcp_command(args: McpArgs) -> anyhow::Result<()> {
    let mut state = TiangongState::load_or_default();
    match args.command {
        McpSubcommand::List => {
            println!("{}", state.mcp_server_summary(None));
        }
        McpSubcommand::Show { name } => {
            if let Some(name) = name {
                println!("{}", state.mcp_server_detail(Some(&name)));
            } else {
                println!("{}", state.mcp_server_summary(None));
            }
        }
        McpSubcommand::Add {
            name,
            command,
            args,
            tags,
            transport,
            endpoint,
            auth_header,
            headers,
            env,
            cwd,
            cmdline,
            enabled,
        } => {
            let (command, args) = resolve_mcp_add_command(command, args, cmdline)?;
            let msg = state.register_mcp_server(
                &name,
                &command,
                args,
                tags,
                enabled,
                RegisterMcpServerOptions {
                    transport: transport.map(Into::into),
                    endpoint,
                    auth_header,
                    headers,
                    env,
                    cwd,
                },
            )?;
            println!("{msg}");
        }
        McpSubcommand::Remove { name } => {
            let msg = state.remove_mcp_server(&name)?;
            println!("{msg}");
        }
        McpSubcommand::Enable { name } => {
            let msg = state.set_mcp_server_enabled(&name, true)?;
            println!("{msg}");
        }
        McpSubcommand::Disable { name } => {
            let msg = state.set_mcp_server_enabled(&name, false)?;
            println!("{msg}");
        }
    }
    Ok(())
}

fn resolve_mcp_add_command(
    command: Option<String>,
    mut args: Vec<String>,
    cmdline: Vec<String>,
) -> anyhow::Result<(String, Vec<String>)> {
    let command = command
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut cmdline = cmdline
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if cmdline.is_empty() {
        return Ok((command.unwrap_or_default(), args));
    }

    if let Some(command) = command {
        args.extend(cmdline);
        return Ok((command, args));
    }

    if cmdline.first().is_some_and(|item| item.starts_with('-')) {
        return Err(anyhow::anyhow!(
            "命令缺少可执行项，请在 -- 后提供 command，如 -- npx -y ..."
        ));
    }

    let command = cmdline.remove(0);
    args.extend(cmdline);
    Ok((command, args))
}
