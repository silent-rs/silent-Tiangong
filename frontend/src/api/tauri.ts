import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

// ============================================================================
// 类型定义
// ============================================================================

export interface Session {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
}

export interface Message {
  id: string;
  role: string;
  content: string;
  reasoning_content?: string;
  created_at: string;
}

export interface RunSnapshot {
  status: string;
  last_session_id?: string;
  current_plan?: TaskPlan;
  messages: Message[];
  input_draft: string;
  pending_session_ids: string[];
}

export interface TaskPlan {
  id: string;
  objective: string;
  summary: string;
  items: PlanItem[];
  risks: string[];
  skill_hints: string[];
  mcp_hints: string[];
}

export interface PlanItem {
  id: string;
  description: string;
  status: string;
  steps: PlanStep[];
}

export interface PlanStep {
  id: string;
  description: string;
  status: string;
  source: string;
}

export interface McpServer {
  name: string;
  command: string;
  args: string[];
  env?: Record<string, string>;
  enabled: boolean;
}

export interface Skill {
  id: string;
  name: string;
  description?: string;
  enabled: boolean;
  source_type: string;
}

export interface McpHealthStatus {
  name: string;
  healthy: boolean;
  tool_count: number;
  last_error?: string;
}

export interface ServerConfig {
  host: string;
  port: number;
  auth_token_masked: string;
  running: boolean;
}

export interface ConnectorInfo {
  name: string;
  connector_type: string;
  enabled: boolean;
}

// 模型配置（Provider + Model + Routing 三层架构）

export interface ProviderConfigView {
  base_url: string;
  api_key: string;
  timeout_ms: number;
}

export interface ModelEntryView {
  provider: string;
  model: string;
  capabilities: string[];
  options: Record<string, unknown>;
}

export interface ModelsConfigView {
  providers: Record<string, ProviderConfigView>;
  models: Record<string, ModelEntryView>;
  routing: Record<string, string>;
}

export interface ModelCapabilityInfo {
  key: string;
  display_name: string;
}

// ============================================================================
// API 方法
// ============================================================================

export const api = {
  // ----------------------------------------------------------------
  // 会话管理
  // ----------------------------------------------------------------
  getSessions: (): Promise<Session[]> =>
    invoke('get_sessions'),

  createSession: (): Promise<Session> =>
    invoke('create_session'),

  switchSession: (sessionId: string): Promise<void> =>
    invoke('switch_session', { sessionId }),

  deleteSession: (): Promise<void> =>
    invoke('delete_session'),

  updateSessionTitle: (title: string): Promise<void> =>
    invoke('update_session_title', { title }),

  // ----------------------------------------------------------------
  // 消息和执行
  // ----------------------------------------------------------------
  sendMessage: (content: string): Promise<void> =>
    invoke('send_message', { content }),

  cancelTurn: (): Promise<boolean> =>
    invoke('cancel_turn'),

  getRunSnapshot: (): Promise<RunSnapshot> =>
    invoke('get_run_snapshot'),

  getInputDraft: (): Promise<string> =>
    invoke('get_input_draft'),

  setInputDraft: (content: string): Promise<void> =>
    invoke('set_input_draft', { content }),

  // ----------------------------------------------------------------
  // MCP 管理
  // ----------------------------------------------------------------
  getMcpServers: (): Promise<McpServer[]> =>
    invoke('get_mcp_servers'),

  getMcpHealth: (): Promise<McpHealthStatus[]> =>
    invoke('get_mcp_health'),

  registerMcpServer: (name: string, command: string, args: string[], env?: Record<string, string>): Promise<string> =>
    invoke('register_mcp_server', { name, command, args, env }),

  removeMcpServer: (name: string): Promise<string> =>
    invoke('remove_mcp_server', { name }),

  setMcpServerEnabled: (name: string, enabled: boolean): Promise<string> =>
    invoke('set_mcp_server_enabled', { name, enabled }),

  // ----------------------------------------------------------------
  // Skill 管理
  // ----------------------------------------------------------------
  getSkills: (): Promise<Skill[]> =>
    invoke('get_skills'),

  installSkill: (path: string): Promise<string> =>
    invoke('install_skill', { path }),

  removeSkill: (id: string): Promise<string> =>
    invoke('remove_skill', { id }),

  setSkillEnabled: (id: string, enabled: boolean): Promise<string> =>
    invoke('set_skill_enabled', { id, enabled }),

  // ----------------------------------------------------------------
  // Server 管理
  // ----------------------------------------------------------------
  getServerConfig: (): Promise<ServerConfig> =>
    invoke('get_server_config'),

  setServerConfig: (host: string, port: number, authToken?: string): Promise<string> =>
    invoke('set_server_config', { host, port, authToken }),

  // ----------------------------------------------------------------
  // Connector 管理
  // ----------------------------------------------------------------
  getConnectors: (): Promise<ConnectorInfo[]> =>
    invoke('get_connectors'),

  setConnectorEnabled: (name: string, enabled: boolean): Promise<string> =>
    invoke('set_connector_enabled', { name, enabled }),

  // ----------------------------------------------------------------
  // 模型配置（Provider + Model + Routing）
  // ----------------------------------------------------------------
  getModelsConfig: (): Promise<ModelsConfigView> =>
    invoke('get_models_config'),

  setModelsConfig: (config: ModelsConfigView): Promise<void> =>
    invoke('set_models_config', { config }),

  getModelCapabilities: (): Promise<ModelCapabilityInfo[]> =>
    invoke('get_model_capabilities'),

  getModelList: (): Promise<string[]> =>
    invoke('get_model_list'),

  fetchProviderModels: (baseUrl: string, apiKey: string, timeoutMs?: number): Promise<string[]> =>
    invoke('fetch_provider_models', { baseUrl, apiKey, timeoutMs }),

  // ----------------------------------------------------------------
  // 事件监听
  // ----------------------------------------------------------------
  onRunSnapshot: (callback: (snapshot: RunSnapshot) => void) =>
    listen<RunSnapshot>('run_snapshot', (event) => callback(event.payload)),
};
