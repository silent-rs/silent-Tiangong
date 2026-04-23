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

export type MessageRole = 'system' | 'user' | 'assistant';
export type RunStatus =
  | 'idle'
  | 'planning'
  | 'executing'
  | 'waiting_approval'
  | 'completed'
  | 'failed';

export interface TokenUsage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

export interface CostSummary {
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_tokens: number;
  call_count: number;
  tool_call_count: number;
}

export interface RequestCost {
  request_id: string;
  usage: TokenUsage;
  timestamp: string;
}

export type MediaKind = 'image' | 'video' | 'audio' | 'file';

export interface MediaAsset {
  kind: MediaKind;
  url: string;
  mime_type?: string;
  title?: string;
  capability?: string;
}

export interface TaskCost {
  task_id: string;
  requests: RequestCost[];
  summary: CostSummary;
}

export interface SessionCost {
  session_id: string;
  tasks: TaskCost[];
  summary: CostSummary;
}

export interface Message {
  id: string;
  role: MessageRole;
  content: string;
  reasoning_content: string;
  worker_id?: string;
  media?: MediaAsset[];
  created_at: string;
}

export interface RunSnapshot {
  status: RunStatus;
  summary?: string;
  last_session_id?: string;
  last_duration_ms?: number;
  last_usage?: TokenUsage;
  current_plan?: TaskPlan;
  messages: Message[];
  input_draft: string;
  pending_session_ids: string[];
  approval_request_id?: string;
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
  version: string;
  description?: string;
  enabled: boolean;
  source_type: string;
}

export interface SkillDetail {
  id: string;
  name: string;
  version: string;
  description?: string;
  enabled: boolean;
  entry: string;
  readme: string;
}

export interface McpHealthStatus {
  name: string;
  healthy: boolean;
  tool_count: number;
  last_error?: string;
  server_version?: string;
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
  protocol: string;
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

export interface CapabilityAvailabilityInfo {
  key: string;
  display_name: string;
  enabled: boolean;
  routed_model?: string;
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

  appendMessage: (content: string): Promise<boolean> =>
    invoke('append_message', { content }),

  respondApproval: (requestId: string, approved: boolean): Promise<boolean> =>
    invoke('respond_approval', { requestId, approved }),

  getTrustMode: (): Promise<string> =>
    invoke('get_trust_mode'),

  setTrustMode: (mode: string): Promise<void> =>
    invoke('set_trust_mode', { mode }),

  getRunSnapshot: (): Promise<RunSnapshot> =>
    invoke('get_run_snapshot'),

  getInputDraft: (): Promise<string> =>
    invoke('get_input_draft'),

  setInputDraft: (content: string): Promise<void> =>
    invoke('set_input_draft', { content }),

  getSessionCwd: (): Promise<string> =>
    invoke('get_session_cwd'),

  setSessionCwd: (cwd: string): Promise<void> =>
    invoke('set_session_cwd', { cwd }),

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

  refreshSkills: (): Promise<string> =>
    invoke('refresh_skills'),

  gcSkills: (apply: boolean): Promise<string> =>
    invoke('gc_skills', { apply }),

  getSkillDetail: (id: string): Promise<SkillDetail> =>
    invoke('get_skill_detail', { id }),

  inspectSkill: (path: string): Promise<{ env_vars: string[]; missing_env_vars: string[]; dependencies: string[] }> =>
    invoke('inspect_skill', { path }),

  installSkill: (path: string, envValues?: Record<string, string>): Promise<string> =>
    invoke('install_skill', { path, envValues }),

  removeSkill: (id: string): Promise<string> =>
    invoke('remove_skill', { id }),

  getSkillEnv: (id: string): Promise<Record<string, string>> =>
    invoke('get_skill_env', { id }),

  setSkillEnv: (id: string, env: Record<string, string>): Promise<void> =>
    invoke('set_skill_env', { id, env }),

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

  getAvailableCapabilities: (): Promise<CapabilityAvailabilityInfo[]> =>
    invoke('get_available_capabilities'),

  hasModelCapability: (capability: string): Promise<boolean> =>
    invoke('has_model_capability', { capability }),

  getModelList: (): Promise<string[]> =>
    invoke('get_model_list'),

  fetchProviderModels: (
    baseUrl: string,
    apiKey: string,
    timeoutMs?: number,
    protocol?: string,
  ): Promise<string[]> =>
    invoke('fetch_provider_models', { baseUrl, apiKey, timeoutMs, protocol }),

  probeEmbeddingDimension: (
    baseUrl: string,
    apiKey: string,
    model: string,
    timeoutMs?: number,
    protocol?: string,
  ): Promise<number> =>
    invoke('probe_embedding_dimension', { baseUrl, apiKey, model, timeoutMs, protocol }),

  // ----------------------------------------------------------------
  // @提及补全
  // ----------------------------------------------------------------
  getMentionCandidates: (): Promise<{ value: string; label: string; kind: string; hint: string }[]> =>
    invoke('get_mention_candidates'),

  // ----------------------------------------------------------------
  // 语音合成
  // ----------------------------------------------------------------
  synthesizeSpeech: (text: string): Promise<{ file_path: string; mime_type: string }> =>
    invoke('synthesize_speech', { text }),

  playAudioFile: (filePath: string): Promise<void> =>
    invoke('play_audio_file', { filePath }),

  stopAudio: (): Promise<void> =>
    invoke('stop_audio'),

  getSessionCost: (sessionId?: string): Promise<SessionCost> =>
    invoke('get_session_cost', { sessionId }),

  hasTtsCapability: (): Promise<boolean> =>
    api.hasModelCapability('tts'),

  listTtsVoices: (): Promise<{ id: string; name: string; gender?: string }[]> =>
    invoke('list_tts_voices'),

  // ----------------------------------------------------------------
  // 语音识别
  // ----------------------------------------------------------------
  hasSttCapability: (): Promise<boolean> =>
    api.hasModelCapability('stt'),

  transcribeSpeech: (audioBase64: string, mimeType: string): Promise<{ text: string; audio_path: string; duration?: number }> =>
    invoke('transcribe_speech', { audioBase64, mimeType }),

  // ----------------------------------------------------------------
  // 事件监听
  // ----------------------------------------------------------------
  onRunSnapshot: (callback: (snapshot: RunSnapshot) => void) =>
    listen<RunSnapshot>('run_snapshot', (event) => callback(event.payload)),
};
