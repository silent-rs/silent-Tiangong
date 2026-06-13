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

export type MessageRole = 'system' | 'user' | 'assistant' | 'tool';
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

export interface TokenStats {
  current_tokens: number;
  compression_threshold_tokens: number;
  context_limit_tokens: number;
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_tokens: number;
  active_agent_current_tokens: number;
  active_agent_id: string | null;
  agent_current_tokens: Record<string, number>;
  agent_token_usage: Record<string, TokenUsage>;
}

export interface CostSummary {
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_tokens: number;
  call_count: number;
  tool_call_count: number;
}

export interface BalanceInfo {
  currency: string;
  total_balance: string;
  granted_balance: string;
  topped_up_balance: string;
}

export interface ProviderBalance {
  is_available: boolean;
  balance_infos: BalanceInfo[];
}

export interface RequestCost {
  request_id: string;
  usage: TokenUsage;
  timestamp: string;
}

export type MediaKind = 'image' | 'video' | 'audio' | 'file';

export interface ContentBlock {
  type: 'text' | 'media';
  // text 类型
  text?: string;
  // media 类型
  kind?: MediaKind;
  url?: string;
  mime_type?: string;
  title?: string;
}

export interface MediaAsset {
  kind: MediaKind;
  url: string;
  mime_type?: string;
  title?: string;
  capability?: string;
}

export interface AttachmentDataUrl {
  data_url: string;
  mime_type: string;
  title: string;
  base64_size: number;
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
  content: ContentBlock[];
  reasoning_content: string;
  worker_id?: string;
  media?: MediaAsset[];
  tool_calls?: { id: string; name: string; arguments?: unknown }[];
  tool_call_id?: string;
  tool_name?: string;
  tool_result_is_error?: boolean;
  compact?: boolean;
  created_at: string;
}

/** 提取消息的纯文本内容，兼容旧格式 content 为字符串的情况 */
export function textContent(msg: Message): string {
  const content = msg.content;
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .filter((b) => b.type === 'text' && b.text)
    .map((b) => b.text!)
    .join('');
}

/** 消息是否包含媒体内容块 */
export function hasMediaBlocks(msg: Message): boolean {
  const content = msg.content;
  if (!Array.isArray(content)) return false;
  return content.some((b) => b.type === 'media');
}

export interface RunSnapshot {
  status: RunStatus;
  summary?: string;
  last_session_id?: string;
  last_duration_ms?: number;
  last_usage?: TokenUsage;
  token_stats?: TokenStats;
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
  routing: Record<string, ModelEntryView>;
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

export interface MemoryConfigView {
  model_key?: string;
  embedding_key?: string;
  rerank_key?: string;
  vector_mode: string;
}

export type MemoryKind = 'episode' | 'entity' | 'decision' | 'evidence';
export type MemoryCognitiveType =
  | 'factual'
  | 'user_preference'
  | 'user_habit'
  | 'skill'
  | 'project_structure'
  | 'architecture_decision'
  | 'problem_incident'
  | 'domain_knowledge';
export type MemoryStatus = 'active' | 'archived';
export type MemoryRelationKind =
  | 'related_to'
  | 'depends_on'
  | 'supports'
  | 'contradicts'
  | 'supersedes'
  | 'caused_by'
  | 'belongs_to'
  | 'learned_from'
  | 'validated_by';

export interface MemoryNode {
  id: string;
  kind: MemoryKind;
  memory_type: MemoryCognitiveType;
  scope_type: string;
  scope_id?: string;
  title: string;
  summary: string;
  keywords: string[];
  importance: number;
  confidence: number;
  status: MemoryStatus;
  source?: string;
  usage_count: number;
  last_used_at?: string;
  created_at: string;
  updated_at: string;
}

export interface ManualMemoryDraft {
  id?: string;
  memory_type: MemoryCognitiveType;
  title: string;
  summary: string;
  keywords: string[];
  importance: number;
  workspace_id?: string;
  session_id?: string;
}

export interface MemoryRelation {
  id: string;
  from_node_id: string;
  to_node_id: string;
  relation_kind: MemoryRelationKind;
  weight: number;
  note?: string;
  created_at: string;
  updated_at: string;
}

export interface MemoryRelationDraft {
  id?: string;
  from_node_id: string;
  to_node_id: string;
  relation_kind: MemoryRelationKind;
  weight: number;
  note?: string;
}

export interface RecallHit {
  node_id: string;
  title: string;
  summary: string;
  score: number;
  kind: MemoryKind;
  importance: number;
  depth1_loaded: boolean;
}

export interface WorkspaceIndexInfo {
  id: string;
  root: string;
  entry_count: number;
  updated_at: string;
}

// ============================================================================
// 定时任务 & Webhook
// ============================================================================

export interface Job {
  id: string;
  name: string;
  description: string;
  trigger_type: 'cron';
  schedule: string | null;
  session_id: string | null;
  payload: string;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export type JobRunStatus = 'running' | 'succeeded' | 'failed';

export interface JobRun {
  id: string;
  job_id: string;
  session_id: string;
  status: JobRunStatus;
  started_at: string;
  finished_at: string | null;
  result_summary: string | null;
}

export interface Webhook {
  id: string;
  name: string;
  description: string;
  session_id: string | null;
  payload: string;
  secret: string | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export type WebhookRunStatus = 'running' | 'succeeded' | 'failed';

export interface WebhookRun {
  id: string;
  webhook_id: string;
  session_id: string;
  status: WebhookRunStatus;
  started_at: string;
  finished_at: string | null;
  result_summary: string | null;
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

  requestDesktopNotificationPermission: (): Promise<boolean> =>
    invoke('request_desktop_notification_permission'),

  sendDesktopNotification: (title: string, body: string, sessionId?: string): Promise<boolean> =>
    invoke('send_desktop_notification', { title, body, sessionId }),

  // ----------------------------------------------------------------
  // 消息和执行
  // ----------------------------------------------------------------
  sendMessage: (content: string): Promise<void> =>
    invoke('send_message', { content }),

  sendMessageWithMedia: (content: string, media: MediaAsset[]): Promise<void> =>
    invoke('send_message_with_media', { content, media }),

  readAttachmentAsDataUrl: (path: string, maxBase64Bytes?: number): Promise<AttachmentDataUrl> =>
    invoke('read_attachment_as_data_url', { path, maxBase64Bytes }),

  cancelTurn: (): Promise<boolean> =>
    invoke('cancel_turn'),

  cancelAgent: (role: string): Promise<boolean> =>
    invoke('cancel_agent', { role }),

  appendMessage: (sessionId: string, content: string): Promise<boolean> =>
    invoke('append_message', { sessionId, content }),

  editAndResend: (messageId: string, newContent: string, media?: MediaAsset[]): Promise<void> =>
    invoke('edit_and_resend', { messageId, newContent, media: media ?? null }),

  respondApproval: (requestId: string, approved: boolean): Promise<boolean> =>
    invoke('respond_approval', { requestId, approved }),

  getTrustMode: (): Promise<string> =>
    invoke('get_trust_mode'),

  setTrustMode: (mode: string): Promise<void> =>
    invoke('set_trust_mode', { mode }),

  getDefaultTrustMode: (): Promise<string> =>
    invoke('get_default_trust_mode'),

  setDefaultTrustMode: (mode: string): Promise<void> =>
    invoke('set_default_trust_mode', { mode }),

  getCustomSystemPrompt: (): Promise<string> =>
    invoke('get_custom_system_prompt'),

  setCustomSystemPrompt: (prompt: string): Promise<void> =>
    invoke('set_custom_system_prompt', { prompt }),

  getReasoningEffort: (): Promise<string> =>
    invoke('get_reasoning_effort'),

  setReasoningEffort: (effort: string): Promise<void> =>
    invoke('set_reasoning_effort', { effort }),

  getProviderBalance: (providerName: string): Promise<ProviderBalance> =>
    invoke('get_provider_balance', { providerName }),

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

  getWorkspaceDir: (): Promise<string> =>
    invoke('get_workspace_dir'),

  setWorkspaceDir: (workspaceDir: string): Promise<void> =>
    invoke('set_workspace_dir', { workspaceDir }),

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

  startServer: (): Promise<string> =>
    invoke('start_server'),

  stopServer: (): Promise<string> =>
    invoke('stop_server'),

  // ----------------------------------------------------------------
  // 模型配置（Provider + Model + Routing）
  // ----------------------------------------------------------------
  getModelsConfig: (): Promise<ModelsConfigView> =>
    invoke('get_models_config'),

  setModelsConfig: (config: ModelsConfigView): Promise<void> =>
    invoke('set_models_config', { config }),

  getMemoryConfig: (): Promise<MemoryConfigView> =>
    invoke('get_memory_config'),

  setMemoryConfig: (config: MemoryConfigView): Promise<void> =>
    invoke('set_memory_config', { config }),

  listMemoryNodes: (query?: string, status?: MemoryStatus, limit?: number, offset?: number): Promise<MemoryNode[]> =>
    invoke('list_memory_nodes', { query, status, limit, offset }),

  countMemoryNodes: (query?: string, status?: MemoryStatus, createdAfter?: string): Promise<number> =>
    invoke('count_memory_nodes', { query, status, createdAfter }),

  upsertManualMemory: (draft: ManualMemoryDraft): Promise<MemoryNode> =>
    invoke('upsert_manual_memory', { draft }),

  setMemoryNodeStatus: (nodeId: string, status: MemoryStatus): Promise<void> =>
    invoke('set_memory_node_status', { nodeId, status }),

  listMemoryRelations: (nodeId: string): Promise<MemoryRelation[]> =>
    invoke('list_memory_relations', { nodeId }),

  listMemoryRelationsBatch: (nodeIds: string[]): Promise<MemoryRelation[]> =>
    invoke('list_memory_relations_batch', { nodeIds }),

  upsertMemoryRelation: (draft: MemoryRelationDraft): Promise<MemoryRelation> =>
    invoke('upsert_memory_relation', { draft }),

  deleteMemoryRelation: (relationId: string): Promise<void> =>
    invoke('delete_memory_relation', { relationId }),

  testMemoryRecall: (query: string, limit?: number): Promise<RecallHit[]> =>
    invoke('test_memory_recall', { query, limit }),

  // ── 索引管理 ──
  listWorkspaceIndexes: (): Promise<WorkspaceIndexInfo[]> =>
    invoke('list_workspace_indexes'),

  deleteWorkspaceIndex: (workspaceId: string): Promise<void> =>
    invoke('delete_workspace_index', { workspaceId }),

  rebuildWorkspaceIndex: (root: string): Promise<number> =>
    invoke('rebuild_workspace_index', { root }),

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
  // 上下文管理
  // ----------------------------------------------------------------
  compressContext: (): Promise<boolean> =>
    invoke('compress_context'),

  resetContext: (): Promise<boolean> =>
    invoke('reset_context'),

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

  // ----------------------------------------------------------------
  // 定时任务管理
  // ----------------------------------------------------------------
  jobList: (): Promise<Job[]> =>
    invoke('job_list'),

  jobCreate: (params: {
    name: string;
    description: string;
    schedule: string;
    session_id?: string;
    payload: string;
    enabled?: boolean;
  }): Promise<Job> =>
    invoke('job_create', params),

  jobUpdate: (params: {
    id: string;
    name?: string;
    description?: string;
    schedule?: string;
    session_id?: string;
    payload?: string;
    enabled?: boolean;
  }): Promise<Job> =>
    invoke('job_update', params),

  jobDelete: (id: string): Promise<void> =>
    invoke('job_delete', { id }),

  jobTrigger: (id: string): Promise<{ job_id: string; session_id: string; status: string }> =>
    invoke('job_trigger', { id }),

  jobListRuns: (id: string, limit?: number): Promise<JobRun[]> =>
    invoke('job_list_runs', { id, limit }),

  // ----------------------------------------------------------------
  // Webhook 管理
  // ----------------------------------------------------------------
  webhookList: (): Promise<Webhook[]> =>
    invoke('webhook_list'),

  webhookCreate: (params: {
    name: string;
    description: string;
    session_id?: string;
    payload: string;
    secret?: string;
    enabled?: boolean;
  }): Promise<Webhook> =>
    invoke('webhook_create', params),

  webhookUpdate: (params: {
    id: string;
    name?: string;
    description?: string;
    session_id?: string;
    payload?: string;
    secret?: string;
    enabled?: boolean;
  }): Promise<Webhook> =>
    invoke('webhook_update', params),

  webhookDelete: (id: string): Promise<void> =>
    invoke('webhook_delete', { id }),

  webhookTrigger: (id: string): Promise<{ webhook_id: string; session_id: string; status: string }> =>
    invoke('webhook_trigger', { id }),

  webhookListRuns: (id: string, limit?: number): Promise<WebhookRun[]> =>
    invoke('webhook_list_runs', { id, limit }),

  // ----------------------------------------------------------------
  // 浏览器面板（通过 plugin:browser）
  // ----------------------------------------------------------------
  browserOpen: (url: string, x: number, y: number, width: number, height: number): Promise<void> =>
    invoke('plugin:browser|browser_open', { url, x, y, width, height }),

  browserClose: (): Promise<void> =>
    invoke('plugin:browser|browser_close'),

  browserSetPosition: (x: number, y: number, width: number, height: number): Promise<void> =>
    invoke('plugin:browser|browser_set_position', { x, y, width, height }),

  browserNavigate: (url: string): Promise<void> =>
    invoke('plugin:browser|browser_navigate', { url }),

  browserEval: (js: string): Promise<void> =>
    invoke('plugin:browser|browser_eval', { js }),

  browserHide: (): Promise<void> =>
    invoke('plugin:browser|browser_hide'),

  browserGoBack: (): Promise<void> =>
    invoke('plugin:browser|browser_go_back'),

  browserGoForward: (): Promise<void> =>
    invoke('plugin:browser|browser_go_forward'),

  browserSetZoom: (scale: number): Promise<number> =>
    invoke('plugin:browser|browser_set_zoom', { scale }),

  browserGetZoom: (): Promise<number> =>
    invoke('plugin:browser|browser_get_zoom'),

  browserResetZoom: (): Promise<number> =>
    invoke('plugin:browser|browser_reset_zoom'),

  browserTabList: (): Promise<{ tabs: Array<{ id: string; url: string; title: string }>; active_tab_id: string | null }> =>
    invoke('plugin:browser|browser_tab_list'),

  browserTabNew: (url: string): Promise<string> =>
    invoke('plugin:browser|browser_tab_new', { url }),

  browserTabSwitch: (tabId: string): Promise<void> =>
    invoke('plugin:browser|browser_tab_switch', { tabId }),

  browserTabClose: (tabId: string): Promise<void> =>
    invoke('plugin:browser|browser_tab_close', { tabId }),

  browserAnnotationExtract: (): Promise<{
    elements: Array<{
      annotation_index: number;
      rect: { x: number; y: number; width: number; height: number };
      elements: Array<{
        tag: string;
        text: string;
        attributes: Record<string, string>;
        selector: string;
        rect: { x: number; y: number; width: number; height: number };
        overlap_ratio: number;
        area: number;
      }>;
    }>;
    count: number;
  }> =>
    invoke('plugin:browser|browser_annotation_extract'),

  browserTabHistory: (tabId?: string): Promise<{
    tab_id: string;
    entries: Array<{ url: string; title: string; timestamp: number }>;
    current_index: number;
  }> =>
    invoke('plugin:browser|browser_tab_history', { tabId: tabId ?? null }),

  browserGlobalHistory: (offset: number, limit: number): Promise<
    Array<{ url: string; title: string; timestamp: number }>
  > =>
    invoke('plugin:browser|browser_global_history', { offset, limit }),

  browserGlobalHistoryClear: (): Promise<void> =>
    invoke('plugin:browser|browser_global_history_clear'),

  browserGlobalHistoryDelete: (url: string): Promise<void> =>
    invoke('plugin:browser|browser_global_history_delete', { url }),

  // 终端面板（通过 plugin:terminal）
  terminalEnsureSession: (sessionId: string, cwd: string): Promise<boolean> =>
    invoke('plugin:terminal|terminal_ensure_session', { sessionId, cwd }),

  terminalSessionSendInput: (sessionId: string, input: string): Promise<void> =>
    invoke('plugin:terminal|terminal_session_send_input', { sessionId, input }),

  terminalSessionRecentOutput: (sessionId: string, lines?: number): Promise<string> =>
    invoke('plugin:terminal|terminal_session_recent_output', { sessionId, lines: lines ?? null }),

  terminalSessionResize: (sessionId: string, cols: number, rows: number): Promise<void> =>
    invoke('plugin:terminal|terminal_session_resize', { sessionId, cols, rows }),

  terminalSessionStatus: (sessionId: string): Promise<{
    session_id: string;
    alive: boolean;
    cwd: string;
    shell: string;
    phase: string;
  }> => invoke('plugin:terminal|terminal_session_status', { sessionId }),

  terminalSessionSetCwd: (sessionId: string, cwd: string): Promise<void> =>
    invoke('plugin:terminal|terminal_session_set_cwd', { sessionId, cwd }),

  terminalSessionReset: (sessionId: string): Promise<void> =>
    invoke('plugin:terminal|terminal_session_reset', { sessionId }),

  terminalDestroySession: (sessionId: string): Promise<void> =>
    invoke('plugin:terminal|terminal_destroy_session', { sessionId }),

  terminalPanelSetSession: (sessionId: string | null): Promise<void> =>
    invoke('plugin:terminal|terminal_panel_set_session', { sessionId }),

  terminalListStatuses: (): Promise<Array<{
    session_id: string;
    alive: boolean;
    cwd: string;
    shell: string;
    phase: string;
  }>> => invoke('plugin:terminal|terminal_list_statuses'),
};
