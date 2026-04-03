use super::super::*;

impl TiangongState {
    /// 执行管理命令（由 RuntimeEngine 通过 TurnEvent 触发）
    pub(in crate::app_state) fn execute_management_command(&mut self, cmd: ManagementCommand) {
        let result = match cmd {
            ManagementCommand::RegisterMcpServer {
                name,
                command,
                args,
                env,
                transport,
                endpoint,
            } => {
                let transport_mode = transport.as_deref().and_then(|t| match t {
                    "http" => Some(McpTransportMode::Http),
                    _ => None,
                });
                self.register_mcp_server(RegisterMcpServerRequest {
                    name: name.clone(),
                    command,
                    args,
                    tags: Vec::new(),
                    enabled: true,
                    options: RegisterMcpServerOptions {
                        transport: transport_mode,
                        endpoint,
                        auth_header: None,
                        headers: Vec::new(),
                        env,
                        cwd: None,
                    },
                })
            }
            ManagementCommand::RemoveMcpServer { name } => self.remove_mcp_server(&name),
            ManagementCommand::SetMcpServerEnabled { name, enabled } => {
                self.set_mcp_server_enabled(&name, enabled)
            }
            ManagementCommand::InstallSkill { path, enabled } => {
                self.install_local_skill(&path, enabled)
            }
            ManagementCommand::RemoveSkill { id } => self.remove_skill(&id),
            ManagementCommand::SetSkillEnabled { id, enabled } => {
                self.set_skill_enabled(&id, enabled)
            }
        };

        match result {
            Ok(msg) => tracing::info!("管理命令执行成功：{msg}"),
            Err(err) => tracing::warn!("管理命令执行失败：{err}"),
        }
    }
}
