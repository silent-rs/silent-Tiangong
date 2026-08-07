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
  /** 会话工作目录，用于按 workspace 分组展示 */
  cwd: string;
}

export interface LoadedSession {
  id: string;
  messages: Message[];
  token_stats: TokenStats;
  current_plan?: TaskPlan;
  last_duration_ms?: number;
  last_usage?: TokenUsage;
  cwd: string;
  reasoning_effort: string;
}

export type TabKind = 'browser' | 'terminal';

export interface TabState {
  id: string;
  kind: TabKind;
  title: string;
  url: string;
  created_at: string;
}

export interface SessionTabs {
  tabs: TabState[];
  active_tab_id: string | null;
}

export interface TerminalTabInfo {
  id: string;
  title: string;
  created_at: string;
  alive: boolean;
  cwd: string;
  shell: string;
  phase: string;
}

export interface TerminalTabListResponse {
  tabs: TerminalTabInfo[];
  active_tab_id: string | null;
}

export type MessageRole = 'system' | 'user' | 'assistant' | 'tool';
export type MessagePhase = 'normal' | 'react' | 'summary' | 'compressedresume';

/** 单个对话轮次的最终执行状态（持久化在用户消息上，历史会话同样可见）。 */
export type TurnStatus = 'success' | 'failed' | 'cancelled';
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

export interface StoredAsset {
  asset_id: string;
  local_path: string;
  original_name: string;
  mime_type: string;
  size: number;
  kind: MediaKind;
}

export type ContentBlock =
  | { type: 'text'; text: string }
  | { type: 'model_instruction'; text: string }
  | {
      type: 'media';
      kind: MediaKind;
      url: string;
      mime_type?: string;
      title?: string;
    }
  | { type: 'asset_reference'; asset: StoredAsset }
  | {
      type: 'image';
      asset: StoredAsset;
      data?: string;
    };

export interface MediaAsset {
  kind: MediaKind;
  url: string;
  mime_type?: string;
  title?: string;
  capability?: string;
}

export interface RawAttachment {
  kind: MediaKind;
  source: string;
  original_name?: string;
  mime_type?: string;
}

export interface InputCache {
  text: string;
  attachments: RawAttachment[];
  is_sending: boolean;
  revision: number;
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
  /** 保留在会话历史中，但不进入当前 Agent 的模型上下文。 */
  model_excluded?: boolean;
  phase?: MessagePhase;
  created_at: string;
  /** 该用户消息所属轮次的执行时长（毫秒）。仅用户消息携带，前端展示「执行总时长」。 */
  elapsed_ms?: number;
  /** 该轮次的最终状态。仅用户消息携带，便于区分成功/失败/取消。 */
  turn_status?: TurnStatus;
}

/** Core 经 Desktop 按会话转发的单个流事件。 */
export interface StreamEvent {
  type: string;
  message_id?: string;
  content?: string;
  content_blocks?: ContentBlock[];
  media?: MediaAsset[];
  model_excluded?: boolean;
  message?: Message | string;
  name?: string;
  names?: string[];
  calls?: { id: string; name: string; arguments?: unknown }[];
  tool_call_id?: string | null;
  ok?: boolean;
  output?: string;
  duration_ms?: number | null;
  usage?: TokenUsage | null;
  current_tokens?: number | null;
  compression_threshold_tokens?: number | null;
  context_limit_tokens?: number | null;
  source?: string;
  agent_id?: string | null;
  agent_role?: string;
  role?: string;
  agent_label?: string;
  messages?: Message[];
  request_id?: string;
  tool_name?: string;
  args_summary?: string;
  attempt?: number;
  max_attempts?: number;
  phase?: string;
  seconds?: number;
  strategy?: string;
  hit_count?: number;
  label?: string;
  status?: string;
  action?: string;
  summary_up_to?: number;
  remaining_messages?: number;
  count?: number;
  holder_agent_label?: string | null;
  path?: string;
  /** title_changed 事件携带的新标题。 */
  title?: string;
}

export interface SessionStreamEvent {
  session_id: string;
  event: StreamEvent;
}

/** 提取消息的纯文本内容，兼容旧格式 content 为字符串的情况 */
export function textContent(msg: Message): string {
  const content = msg.content;
  if (typeof content === 'string') return content;
  if (!Array.isArray(content)) return '';
  return content
    .flatMap((block) => block.type === 'text' && block.text ? [block.text] : [])
    .join('');
}

/** 消息是否包含媒体内容块 */
export function hasMediaBlocks(msg: Message): boolean {
  const content = msg.content;
  if (!Array.isArray(content)) return false;
  return content.some((b) =>
    b.type === 'media' || b.type === 'asset_reference' || b.type === 'image'
  );
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
  transport: 'auto' | 'stdio' | 'http';
  command: string;
  args: string[];
  endpoint: string;
  auth_header: string;
  headers?: Record<string, string>;
  env?: Record<string, string>;
  enabled: boolean;
}

export interface RegisterMcpServerInput {
  name: string;
  transport?: 'auto' | 'stdio' | 'http' | 'sse';
  command?: string;
  args?: string[];
  endpoint?: string;
  authHeader?: string;
  headers?: Record<string, string>;
  env?: Record<string, string>;
}

export interface UpdateMcpServerInput {
  name: string;
  transport?: 'auto' | 'stdio' | 'http' | 'sse';
  command?: string;
  args?: string[];
  endpoint?: string;
  authHeader?: string;
  headers?: Record<string, string>;
  env?: Record<string, string>;
}

export interface McpHealthStatus {
  name: string;
  healthy: boolean;
  tool_count: number;
  last_error?: string;
  server_version?: string;
}

// --- 通讯网关（bot）类型 ---

export type FieldType =
  | { kind: 'string' }
  | { kind: 'secret' }
  | { kind: 'boolean' }
  | { kind: 'barcode' }
  | { kind: 'select'; options: string[] };

export interface ConfigFieldSchema {
  key: string;
  label: string;
  field_type: FieldType;
  required: boolean;
  default?: unknown;
  help?: string;
}

export interface BotConfig {
  id: string;
  artifact_id: string;
  enabled: boolean;
  config: Record<string, unknown>;
  created_at: string;
  updated_at: string;
}

export interface RegisterBotRequest {
  id: string;
  artifact_id: string;
  config?: Record<string, unknown>;
  enabled?: boolean;
}

export interface UpdateBotRequest {
  config?: Record<string, unknown>;
}

export interface BotArtifact {
  url: string;
  checksum: string;
}

export interface BotManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  config_schema: ConfigFieldSchema[];
  platforms: Record<string, BotArtifact>;
  min_app_version?: string;
}

export interface BotsIndex {
  version: number;
  bots: BotManifest[];
}

export interface LocalArtifact {
  id: string;
  name: string;
  artifact_id: string;
  version: string;
  config_schema: ConfigFieldSchema[];
  supports_mcp: boolean;
}

export interface BotPushTarget {
  target_id: string;
  label: string;
  kind: 'direct' | 'group' | string;
  enabled: boolean;
  availability: 'ready' | 'reply_window' | 'unavailable' | 'unknown' | string;
  last_seen_at: string;
  limitation?: string;
}

export interface QrSession {
  qr_url: string;
  expires_at: number;
  interval: number;
  state: unknown;
}

export interface BotLog {
  content: string;
  truncated: boolean;
}

export interface BotTransferProgress {
  downloaded: number;
  total: number;
}

export type ProvisionStatus =
  | { status: 'pending'; retry_after?: number }
  | { status: 'success' }
  | { status: 'expired' }
  | { status: 'error'; message: string };

export type BotHealth =
  | 'running'
  | 'stopped'
  | 'missing_artifact'
  | { error: { message: string } };

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
  context_window?: number;
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

// ============================================================================
// 定时任务 & Webhook
// ============================================================================


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

  getSessionTabs: (sessionId: string): Promise<SessionTabs> =>
    invoke('get_session_tabs', { sessionId }),

  setSessionTabs: (sessionId: string, tabs: TabState[], activeTabId: string | null): Promise<void> =>
    invoke('set_session_tabs', { sessionId, tabs, activeTabId }),

  switchSession: (sessionId: string): Promise<void> =>
    invoke('switch_session', { sessionId }),

  loadSession: (sessionId: string): Promise<LoadedSession> =>
    invoke('load_session', { sessionId }),

  deleteSession: (): Promise<void> =>
    invoke('delete_session'),

  deleteSessionsByCwd: (cwd: string): Promise<void> =>
    invoke('delete_sessions_by_cwd', { cwd }),

  updateSessionTitle: (title: string): Promise<void> =>
    invoke('update_session_title', { title }),

  requestDesktopNotificationPermission: (): Promise<boolean> =>
    invoke('request_desktop_notification_permission'),

  sendDesktopNotification: (title: string, body: string, sessionId?: string): Promise<boolean> =>
    invoke('send_desktop_notification', { title, body, sessionId }),

  // ----------------------------------------------------------------
  // 消息和执行
  // ----------------------------------------------------------------
  sendMessage: (
    sessionId: string,
    content: string,
    attachments: RawAttachment[],
    revision: number,
    cwd?: string,
    trustMode?: string,
    reasoningEffort?: string,
  ): Promise<void> =>
    invoke('send_message', {
      sessionId,
      content,
      attachments,
      revision,
      cwd,
      trustMode,
      reasoningEffort,
    }),

  readAttachmentAsDataUrl: (path: string, maxBase64Bytes?: number): Promise<AttachmentDataUrl> =>
    invoke('read_attachment_as_data_url', { path, maxBase64Bytes }),

  cancelTurn: (): Promise<boolean> =>
    invoke('cancel_turn'),

  cancelAgent: (role: string): Promise<boolean> =>
    invoke('cancel_agent', { role }),

  appendMessage: (
    sessionId: string,
    content: string,
    attachments: RawAttachment[],
    revision: number,
  ): Promise<boolean> =>
    invoke('append_message', { sessionId, content, attachments, revision }),

  editAndResend: (
    sessionId: string,
    messageId: string,
    newContent: string,
    attachments: RawAttachment[],
    revision: number,
    baseContent: ContentBlock[],
  ): Promise<void> =>
    invoke('edit_and_resend', {
      sessionId,
      messageId,
      newContent,
      attachments,
      revision,
      baseContent,
    }),

  respondApproval: (requestId: string, approved: boolean): Promise<boolean> =>
    invoke('respond_approval', { requestId, approved }),

  getTrustMode: (sessionId?: string): Promise<string> =>
    invoke('get_trust_mode', { sessionId }),

  setTrustMode: (mode: string, sessionId?: string): Promise<void> =>
    invoke('set_trust_mode', { mode, sessionId }),

  getDefaultTrustMode: (): Promise<string> =>
    invoke('get_default_trust_mode'),

  setDefaultTrustMode: (mode: string): Promise<void> =>
    invoke('set_default_trust_mode', { mode }),


  getReasoningEffort: (sessionId?: string): Promise<string> =>
    invoke('get_reasoning_effort', { sessionId }),

  setReasoningEffort: (effort: string, sessionId?: string): Promise<void> =>
    invoke('set_reasoning_effort', { effort, sessionId }),

  getProviderBalance: (providerName: string): Promise<ProviderBalance> =>
    invoke('get_provider_balance', { providerName }),

  newSessionId: (): Promise<string> =>
    invoke('new_session_id'),

  removeInputCache: (cacheKey: string): Promise<void> =>
    invoke('remove_input_cache', { cacheKey }),

  getInputCache: (cacheKey: string): Promise<InputCache> =>
    invoke('get_input_cache', { cacheKey }),

  setInputCache: (
    cacheKey: string,
    cache: InputCache,
    claimRevision?: number,
  ): Promise<InputCache> =>
    invoke('set_input_cache', { cacheKey, cache, claimRevision }),

  setSessionCwd: (sessionId: string, cwd: string): Promise<void> =>
    invoke('set_session_cwd', { sessionId, cwd }),

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

  probeMcpServer: (name: string): Promise<void> =>
    invoke('probe_mcp_server', { name }),

  registerMcpServer: (input: RegisterMcpServerInput): Promise<string> =>
    invoke('register_mcp_server', {
      name: input.name,
      command: input.command ?? '',
      args: input.args ?? [],
      transport: input.transport,
      endpoint: input.endpoint,
      authHeader: input.authHeader,
      headers: input.headers,
      env: input.env,
    }),

  updateMcpServer: (name: string, input: UpdateMcpServerInput): Promise<string> =>
    invoke('update_mcp_server', {
      name,
      command: input.command ?? '',
      args: input.args ?? [],
      transport: input.transport,
      endpoint: input.endpoint,
      authHeader: input.authHeader,
      headers: input.headers,
      env: input.env,
    }),

  removeMcpServer: (name: string): Promise<string> =>
    invoke('remove_mcp_server', { name }),

  setMcpServerEnabled: (name: string, enabled: boolean): Promise<string> =>
    invoke('set_mcp_server_enabled', { name, enabled }),

  // ----------------------------------------------------------------
  // 通讯网关（bot）管理
  // ----------------------------------------------------------------
  botList: (): Promise<BotConfig[]> =>
    invoke('bot_list'),

  botHealth: (id: string): Promise<BotHealth> =>
    invoke('bot_health', { id }),

  botLog: (id: string): Promise<BotLog> =>
    invoke('bot_log', { id }),

  botConfigSchema: (artifactId: string, botId?: string): Promise<ConfigFieldSchema[]> =>
    invoke('bot_config_schema', { artifactId, botId: botId ?? null }),

  botProvisionBegin: (botId: string): Promise<QrSession> =>
    invoke('bot_provision_begin', { botId }),

  botProvisionPoll: (botId: string, session: QrSession): Promise<ProvisionStatus> =>
    invoke('bot_provision_poll', { botId, session }),

  botAvailable: (): Promise<BotsIndex> =>
    invoke('bot_available'),

  botScanLocal: (): Promise<LocalArtifact[]> =>
    invoke('bot_scan_local'),

  botPushTargets: (id: string): Promise<BotPushTarget[]> =>
    invoke('bot_push_targets', { id }),

  botDeletePushTarget: (id: string, targetId: string): Promise<string> =>
    invoke('bot_delete_push_target', { id, targetId }),

  botRegisterMcp: (id: string): Promise<string> =>
    invoke('bot_register_mcp', { id }),

  botRegister: (request: RegisterBotRequest): Promise<BotConfig> =>
    invoke('bot_register', { request }),

  botUpdate: (id: string, request: UpdateBotRequest): Promise<BotConfig> =>
    invoke('bot_update', { id, request }),

  botRemove: (id: string): Promise<string> =>
    invoke('bot_remove', { id }),

  botInstall: (artifactId: string, destBotId: string): Promise<string> =>
    invoke('bot_install', { artifactId, destBotId }),

  onBotInstallProgress: (callback: (progress: BotTransferProgress) => void) =>
    listen<BotTransferProgress>('bot_install_progress', (event) => callback(event.payload)),

  botStart: (id: string): Promise<string> =>
    invoke('bot_start', { id }),

  botStop: (id: string): Promise<string> =>
    invoke('bot_stop', { id }),

  botCheckUpdate: (artifactId: string): Promise<BotManifest | null> =>
    invoke('bot_check_update', { artifactId }),

  botUpgrade: (botId: string): Promise<string> =>
    invoke('bot_upgrade', { botId }),

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

  // 预热工作区索引（索引已存在则直接返回，否则后台扫描，立即返回不阻塞）
  // 索引管理（列表/删除/重建）由「设置 → 索引管理」页经插件 UI 通道处理。
  prewarmWorkspaceIndex: (root: string): Promise<void> =>
    invoke('prewarm_workspace_index', { root }),

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

  resolveModelContextWindow: (model: string): Promise<number> =>
    invoke('resolve_model_context_window', { model }),

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
  onStreamEvent: (callback: (event: SessionStreamEvent) => void) =>
    listen<SessionStreamEvent>('stream_event', (event) => callback(event.payload)),

  // ----------------------------------------------------------------
  // Webhook 管理
  // ----------------------------------------------------------------
  webhookList: (): Promise<Webhook[]> =>
    invoke('webhook_list'),

  webhookCreate: (params: {
    name: string;
    description: string;
    sessionId?: string;
    payload: string;
    secret?: string;
    enabled?: boolean;
  }): Promise<Webhook> =>
    invoke('webhook_create', params),

  webhookUpdate: (params: {
    id: string;
    name?: string;
    description?: string;
    sessionId?: string;
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
  browserOpen: (sessionId: string, url: string, x: number, y: number, width: number, height: number): Promise<void> =>
    invoke('plugin:browser|browser_open', { sessionId, url, x, y, width, height }),

  browserClose: (sessionId: string): Promise<void> =>
    invoke('plugin:browser|browser_close', { sessionId }),

  browserSetPosition: (sessionId: string, x: number, y: number, width: number, height: number): Promise<void> =>
    invoke('plugin:browser|browser_set_position', { sessionId, x, y, width, height }),

  browserNavigate: (sessionId: string, url: string): Promise<void> =>
    invoke('plugin:browser|browser_navigate', { sessionId, url }),

  browserOpenUrl: (sessionId: string, url: string): Promise<{
    session_id: string | null;
    tabs: Array<{ id: string; url: string; title: string }>;
    active_tab_id: string | null;
  }> =>
    invoke('plugin:browser|browser_open_url', { sessionId, url }),

  browserEval: (sessionId: string, js: string): Promise<void> =>
    invoke('plugin:browser|browser_eval', { sessionId, js }),

  browserHide: (sessionId: string): Promise<void> =>
    invoke('plugin:browser|browser_hide', { sessionId }),

  browserGoBack: (sessionId: string): Promise<void> =>
    invoke('plugin:browser|browser_go_back', { sessionId }),

  browserGoForward: (sessionId: string): Promise<void> =>
    invoke('plugin:browser|browser_go_forward', { sessionId }),

  browserReload: (sessionId: string): Promise<void> =>
    invoke('plugin:browser|browser_reload', { sessionId }),

  browserSetZoom: (sessionId: string, scale: number): Promise<number> =>
    invoke('plugin:browser|browser_set_zoom', { sessionId, scale }),

  browserGetZoom: (sessionId: string): Promise<number> =>
    invoke('plugin:browser|browser_get_zoom', { sessionId }),

  browserResetZoom: (sessionId: string): Promise<number> =>
    invoke('plugin:browser|browser_reset_zoom', { sessionId }),

  browserTabList: (sessionId: string): Promise<{ tabs: Array<{ id: string; url: string; title: string }>; active_tab_id: string | null }> =>
    invoke('plugin:browser|browser_tab_list', { sessionId }),

  browserSnapshotTabs: (sessionId: string): Promise<{
    session_id: string | null;
    tabs: Array<{ id: string; url: string; title: string }>;
    active_tab_id: string | null;
  }> =>
    invoke('plugin:browser|browser_snapshot_tabs', { sessionId }),

  browserSwitchSession: (
    sessionId: string,
    activeTabId?: string | null,
  ): Promise<{
    session_id: string | null;
    tabs: Array<{ id: string; url: string; title: string }>;
    active_tab_id: string | null;
  }> =>
    invoke('plugin:browser|browser_switch_session', {
      sessionId,
      activeTabId: activeTabId ?? null,
    }),

  browserTabNew: (sessionId: string, url: string): Promise<string> =>
    invoke('plugin:browser|browser_tab_new', { sessionId, url }),

  browserTabSwitch: (sessionId: string, tabId: string): Promise<void> =>
    invoke('plugin:browser|browser_tab_switch', { sessionId, tabId }),

  browserTabClose: (sessionId: string, tabId: string): Promise<void> =>
    invoke('plugin:browser|browser_tab_close', { sessionId, tabId }),

  browserAnnotationExtract: (sessionId: string): Promise<{
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
    invoke('plugin:browser|browser_annotation_extract', { sessionId }),

  browserTabHistory: (sessionId: string, tabId?: string): Promise<{
    tab_id: string;
    entries: Array<{ url: string; title: string; timestamp: number }>;
    current_index: number;
  }> =>
    invoke('plugin:browser|browser_tab_history', { sessionId, tabId: tabId ?? null }),

  browserGlobalHistory: (offset: number, limit: number): Promise<
    Array<{ url: string; title: string; timestamp: number }>
  > =>
    invoke('plugin:browser|browser_global_history', { offset, limit }),

  browserGlobalHistoryClear: (): Promise<void> =>
    invoke('plugin:browser|browser_global_history_clear'),

  browserGlobalHistoryDelete: (url: string): Promise<void> =>
    invoke('plugin:browser|browser_global_history_delete', { url }),

  // 终端面板（通过 plugin:terminal，按对话 session 路由）
  terminalEnsureSession: (sessionId: string, cwd: string): Promise<boolean> =>
    invoke('plugin:terminal|terminal_ensure_session', { sessionId, cwd }),

  terminalSessionSendInput: (sessionId: string, input: string): Promise<void> =>
    invoke('plugin:terminal|terminal_session_send_input', { sessionId, input }),

  // 上报用户在终端提交的完整命令行（回车截断后上报，供注入 Agent 对话链）
  terminalReportUserCommand: (sessionId: string, command: string): Promise<void> =>
    invoke('plugin:terminal|terminal_report_user_command', { sessionId, command }),

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

  // 前端 xterm.js 把当前屏幕可见区域序列化回传后端（供 handle_exec_interactive 返回给 Agent）
  terminalSessionUpdateScreen: (sessionId: string, snapshot: string): Promise<void> =>
    invoke('plugin:terminal|terminal_session_update_screen', { sessionId, snapshot }),

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

  terminalTabList: (sessionId: string): Promise<TerminalTabListResponse> =>
    invoke('plugin:terminal|terminal_tab_list', { sessionId }),

  terminalTabNew: (
    sessionId: string,
    title?: string | null,
    cwd?: string | null,
  ): Promise<string> =>
    invoke('plugin:terminal|terminal_tab_new', {
      sessionId,
      title: title ?? null,
      cwd: cwd ?? null,
    }),

  terminalTabRestore: (
    sessionId: string,
    tabId: string,
    title?: string | null,
    cwd?: string | null,
  ): Promise<void> =>
    invoke('plugin:terminal|terminal_tab_restore', {
      sessionId,
      tabId,
      title: title ?? null,
      cwd: cwd ?? null,
    }),

  terminalTabSwitch: (sessionId: string, tabId: string): Promise<void> =>
    invoke('plugin:terminal|terminal_tab_switch', { sessionId, tabId }),

  terminalTabClose: (sessionId: string, tabId: string): Promise<void> =>
    invoke('plugin:terminal|terminal_tab_close', { sessionId, tabId }),

  // ── 插件 UI 桥接（WASM 插件动态 UI）──
  // 天工只提供通用桥接，不处理具体插件业务。

  listPluginContributions: (): Promise<PluginContributionEntry[]> =>
    invoke('list_plugin_contributions'),

  listPlugins: (): Promise<PluginStatus[]> => invoke('list_plugins'),

  listAvailablePlugins: (): Promise<AvailablePlugin[]> => invoke('list_available_plugins'),

  importLocalPlugin: (path: string): Promise<PluginStatus> =>
    invoke('import_local_plugin', { path }),

  installPlugin: (pluginId: string): Promise<PluginStatus> =>
    invoke('install_plugin', { pluginId }),

  upgradePlugin: (pluginId: string): Promise<PluginStatus> =>
    invoke('upgrade_plugin', { pluginId }),

  setPluginEnabled: (pluginId: string, enabled: boolean): Promise<PluginStatus> =>
    invoke('set_plugin_enabled', { pluginId, enabled }),

  rollbackPlugin: (pluginId: string): Promise<PluginStatus> =>
    invoke('rollback_plugin', { pluginId }),

  uninstallPlugin: (pluginId: string, keepData: boolean): Promise<void> =>
    invoke('uninstall_plugin', { pluginId, keepData }),

  reloadPlugin: (pluginId: string): Promise<PluginStatus> =>
    invoke('reload_plugin', { pluginId }),

  /// 按需获取插件页面 HTML（用户点击进入时才调用）。
  pluginOpenView: (pluginId: string, contributionId: string): Promise<string> =>
    invoke('plugin_open_view', { pluginId, contributionId }),

  /// 通用桥接：转发到 WASM 的 handle-view-message。
  pluginCall: (pluginId: string, method: string, payload: string): Promise<string> =>
    invoke('plugin_call', { pluginId, method, payload }),
};

/// 插件设置页贡献项。
export interface PluginContributionEntry {
  plugin_id: string;
  generation: number;
  contribution_id: string;
  title: string;
  description: string;
  icon: string;
  group: string;
  /// 是否有可渲染的配置页面。
  has_view: boolean;
}

export interface PluginStatus {
  id: string;
  name: string;
  manifest_version: string;
  loaded_version: string | null;
  state: 'loaded' | 'disabled' | 'degraded' | 'error';
  generation: number;
  enabled: boolean;
  can_rollback: boolean;
  has_sidecar: boolean;
  sidecar_running: boolean;
  last_error: string | null;
}

export interface AvailablePlugin {
  id: string;
  name: string;
  version: string;
  description: string;
  supported: boolean;
  installed_version: string | null;
  update_available: boolean;
}
