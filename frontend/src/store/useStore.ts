import { create } from 'zustand';
import { api, textContent } from '../api/tauri';
import type {
  ContentBlock,
  McpServer,
  Message,
  RawAttachment,
  RunSnapshot,
  Session,
  InputCache,
  Skill,
  TaskPlan,
  TokenStats,
} from '../api/tauri';
import { notifyBackgroundSessionCompleted } from '../utils/desktopNotification';
import {
  cloneInputCache,
  emptyInputCache,
  getInputCache,
  mergeStoredInputCache,
  setInputCacheSending,
  settleInputCacheSend,
  updateInputCacheAttachments,
  updateInputCacheText,
  type InputCacheMap,
} from './inputCache';

let switchRequestVersion = 0;
let newConversationRequestVersion = 0;
const inputCacheSyncRequestVersions = new Map<string, number>();

interface InputCacheSyncQueue {
  pending: InputCache | null;
  pendingVersion: number;
  claimed: {
    cache: InputCache;
    version: number;
    revision: number;
  } | null;
  running: boolean;
  debounceVersion: number;
  nextVersion: number;
  immediateThroughVersion: number;
  waiters: Array<{
    version: number;
    resolve: (cache: InputCache) => void;
    reject: (error: unknown) => void;
  }>;
}

const inputCacheSyncQueues = new Map<string, InputCacheSyncQueue>();

function sameAttachments(left: RawAttachment[], right: RawAttachment[]): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function rebasePendingInputCacheOnStored(
  pending: InputCache,
  submitted: InputCache,
  stored: InputCache,
): InputCache {
  return {
    text: pending.text === submitted.text ? stored.text : pending.text,
    attachments: sameAttachments(pending.attachments, submitted.attachments)
      ? stored.attachments.map((attachment) => ({ ...attachment }))
      : pending.attachments,
    is_sending: pending.is_sending,
    revision: pending.revision,
  };
}

function waitForInputCacheDebounce(): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, 200));
}

function discardInputCacheSyncQueue(cacheKey: string): void {
  const queue = inputCacheSyncQueues.get(cacheKey);
  if (queue) {
    for (const waiter of queue.waiters) waiter.resolve(emptyInputCache());
    queue.waiters = [];
  }
  inputCacheSyncQueues.delete(cacheKey);
  inputCacheSyncRequestVersions.delete(cacheKey);
}

function syncInputCacheInBackground(promise: Promise<InputCache>): void {
  void promise.catch(() => undefined);
}

// ---------------------------------------------------------------------------
// Agent 信息（从系统消息解析）
// ---------------------------------------------------------------------------

export interface AgentInfo {
  agentId?: string;
  role: string;
  label: string;
  status: 'idle' | 'running' | 'waiting_for_user' | 'waiting_for_lock' | 'terminated' | 'error';
}

/** 从系统消息中解析 Agent 列表 */
export function parseAgentsFromMessages(messages: Message[]): AgentInfo[] {
  const agents = new Map<string, AgentInfo>();
  for (const msg of messages) {
    if (msg.role !== 'system') continue;
    const text = textContent(msg);
    // [Agent] {label} ({role}) 已加入团队
    const createMatch = text.match(/^\[Agent\] (.+?) \((.+?)\) 已加入团队.*?id=([^\s]+)/);
    if (createMatch) {
      const [, label, role, agentId] = createMatch;
      agents.set(role, { agentId, role, label, status: 'idle' });
      continue;
    }
    // [Agent] {label} 状态变更: {status}
    const statusMatch = text.match(/^\[Agent\] (.+?) 状态变更: (\w+).*?id=([^\s]+)/);
    if (statusMatch) {
      const [, , status, agentId] = statusMatch;
      if (status === 'terminated') {
        for (const [role, info] of agents) {
          if (info.agentId === agentId) {
            agents.delete(role);
            break;
          }
        }
      } else if (
        status === 'idle'
        || status === 'running'
        || status === 'waiting_for_user'
        || status === 'waiting_for_lock'
        || status === 'error'
      ) {
        for (const [, info] of agents) {
          if (info.agentId === agentId) {
            info.status = status;
            break;
          }
        }
      }
    }
  }
  return Array.from(agents.values());
}

function isAgentSystemMessage(message: Message): boolean {
  if (message.role !== 'system') return false;
  const text = textContent(message);
  return text.startsWith('[Agent]') || text.startsWith('[文件锁]');
}

function shouldRefreshAgentsFromSnapshot(
  oldMessages: Message[],
  newMessages: Message[],
): boolean {
  if (oldMessages.length !== newMessages.length) {
    return true;
  }
  for (let i = 0; i < newMessages.length; i += 1) {
    const oldMessage = oldMessages[i];
    const newMessage = newMessages[i];
    if (oldMessage === newMessage) {
      continue;
    }
    if (isAgentSystemMessage(oldMessage) || isAgentSystemMessage(newMessage)) {
      return true;
    }
  }
  return false;
}

function sameJsonValue(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (left == null || right == null) return left == right;
  return JSON.stringify(left) === JSON.stringify(right);
}

function sameMessage(left: Message, right: Message): boolean {
  return left.id === right.id
    && left.role === right.role
    && sameJsonValue(left.content, right.content)
    && left.reasoning_content === right.reasoning_content
    && left.worker_id === right.worker_id
    && left.tool_call_id === right.tool_call_id
    && left.tool_name === right.tool_name
    && left.tool_result_is_error === right.tool_result_is_error
    && left.compact === right.compact
    && left.model_excluded === right.model_excluded
    && left.phase === right.phase
    && left.created_at === right.created_at
    && sameJsonValue(left.media, right.media)
    && sameJsonValue(left.tool_calls, right.tool_calls)
    && left.elapsed_ms === right.elapsed_ms
    && left.turn_status === right.turn_status;
}

function mergeSnapshotMessages(oldMessages: Message[], newMessages: Message[]): Message[] {
  if (oldMessages.length === 0 || newMessages.length === 0) {
    return newMessages;
  }

  const oldById = new Map(oldMessages.map((message) => [message.id, message]));
  let changed = oldMessages.length !== newMessages.length;
  const merged = newMessages.map((message) => {
    const old = oldById.get(message.id);
    if (old && sameMessage(old, message)) {
      return old;
    }
    changed = true;
    return message;
  });

  return changed ? merged : oldMessages;
}

function stripLeadingAgentMention(content: string, agents: AgentInfo[]): string {
  const match = content.match(/^@([A-Za-z0-9_-]+)\s*/);
  if (!match) return content;
  const role = match[1];
  const agentRoles = new Set([...agents.map((agent) => agent.role), 'all']);
  if (!agentRoles.has(role)) return content;
  return content.slice(match[0].length).trimStart();
}

function applyAgentMention(content: string, tab: string | null, agents: AgentInfo[]): string {
  const body = stripLeadingAgentMention(content, agents);
  if (!tab) return body;
  return body.length > 0 ? `@${tab} ${body}` : `@${tab} `;
}

export interface AppState {
  // 状态
  sessions: Session[];
  activeSessionId: string | null;
  /** 新对话预留的 Session ID；首次发送前不存在对应 Session。 */
  newConversationId: string | null;
  inputCaches: InputCacheMap;
  messages: Message[];
  runStatus: string;
  runSummary: string;
  lastDurationMs: number | null;
  lastUsage: { prompt_tokens: number; completion_tokens: number; total_tokens: number } | null;
  tokenStats: TokenStats | null;
  approvalRequestId: string | null;
  currentPlan: TaskPlan | undefined;
  mcpServers: McpServer[] | null;
  skills: Skill[] | null;

  // 尚未首次发送的新对话
  isNewConversation: boolean;

  // 思考强度（按 session 存储）
  reasoningEffort: string;
  reasoningEffortPerSession: Record<string, string>;
  setReasoningEffort: (effort: string) => void;

  // 工作目录
  workspaceDir: string;
  sessionCwd: string;

  // 多会话运行状态 (session_id -> status)
  sessionRunStatuses: Record<string, string>;

  // 流式消息状态
  streamingMessageId: string | null;
  streamingContent: string;
  streamingReasoningContent: string; // 流式思考过程内容

  // 语音消息映射 (消息内容 hash → 音频信息)
  voiceMessages: Record<string, { audioPath: string; duration?: number; showText: boolean }>;
  addVoiceMessage: (msgKey: string, audioPath: string, duration?: number) => void;
  toggleVoiceText: (msgKey: string) => void;

  // Agent 团队
  agents: AgentInfo[];
  selectedAgentTab: string | null; // null = 主对话, role = 指定 Agent

  // 加载状态
  isLoadingSessions: boolean;

  // 更新检查
  updateAvailable: null | { version: string; body?: string; date?: string };
  setUpdateAvailable: (info: null | { version: string; body?: string; date?: string }) => void;

  // 从外部触发打开设置页到指定 tab
  pendingSettingsTab: string | null;
  setPendingSettingsTab: (tab: string | null) => void;

  // 操作
  loadSessions: () => Promise<void>;
  startNewConversation: (targetCwd?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: () => Promise<void>;
  deleteSessionsByCwd: (cwd: string) => Promise<void>;

  sendMessage: (
    cacheKey: string,
    content: string,
    attachments: RawAttachment[],
    revision: number,
    trustMode?: string,
  ) => Promise<boolean>;
  appendMessage: (
    sessionId: string,
    content: string,
    attachments: RawAttachment[],
    revision: number,
  ) => Promise<boolean>;
  editAndResend: (
    sessionId: string,
    messageId: string,
    newContent: string,
    attachments: RawAttachment[],
    revision: number,
    baseContent: ContentBlock[],
  ) => Promise<boolean>;
  cancelTurn: () => Promise<boolean>;
  cancelAgent: (role: string) => Promise<boolean>;

  setInputCacheText: (cacheKey: string, content: string) => void;
  setInputCacheAttachments: (cacheKey: string, attachments: RawAttachment[]) => void;
  syncInputCache: (
    cacheKey: string,
    cache: InputCache,
    immediate?: boolean,
    claimRevision?: number,
  ) => Promise<InputCache>;
  flushInputCacheQueue: (cacheKey: string) => Promise<void>;

  setSessionCwd: (cwd: string) => Promise<void>;
  setWorkspaceDir: (workspaceDir: string) => Promise<void>;

  loadMcpServers: () => Promise<void>;
  loadSkills: () => Promise<void>;

  setSelectedAgentTab: (tab: string | null) => void;
  beginContextManagement: (summary: string) => void;
  endContextManagement: () => void;

  // 内部方法
  updateFromSnapshot: (snapshot: RunSnapshot) => void;
}

export function selectCurrentInputCacheKey(state: AppState): string | null {
  return state.activeSessionId ?? state.newConversationId;
}

export function selectCurrentInputCache(state: AppState): InputCache {
  return getInputCache(state.inputCaches, selectCurrentInputCacheKey(state));
}

export function selectCurrentIsSending(state: AppState): boolean {
  return selectCurrentInputCache(state).is_sending;
}

export const useStore = create<AppState>((set, get) => ({
  // 初始状态
  sessions: [],
  activeSessionId: null as string | null,
  newConversationId: null as string | null,
  inputCaches: {},
  messages: [],
  runStatus: 'idle',
  runSummary: '',
  lastDurationMs: null,
  lastUsage: null,
  tokenStats: null,
  approvalRequestId: null,
  currentPlan: undefined,
  mcpServers: null,
  skills: null,
  isNewConversation: true,
  updateAvailable: null,
  setUpdateAvailable: (info) => set({ updateAvailable: info }),
  pendingSettingsTab: null,
  setPendingSettingsTab: (tab) => set({ pendingSettingsTab: tab }),
  reasoningEffort: 'medium',
  reasoningEffortPerSession: {},
  setReasoningEffort: (effort: string) => {
    const { activeSessionId, reasoningEffortPerSession } = get();
    const key = activeSessionId || '__new_conversation__';
    const updated = { ...reasoningEffortPerSession, [key]: effort };
    set({ reasoningEffort: effort, reasoningEffortPerSession: updated });
    if (activeSessionId) {
      api.setReasoningEffort(effort, activeSessionId).catch(console.error);
    }
  },
  workspaceDir: '',
  sessionCwd: '',
  sessionRunStatuses: {},
  streamingMessageId: null,
  streamingContent: '',
  streamingReasoningContent: '',
  voiceMessages: (() => {
    try {
      return JSON.parse(localStorage.getItem('tiangong-voice-messages') || '{}');
    } catch { return {}; }
  })(),
  addVoiceMessage: (msgKey, audioPath, duration) => {
    set((state) => {
      const next = {
        ...state.voiceMessages,
        [msgKey]: { audioPath, duration, showText: false },
      };
      localStorage.setItem('tiangong-voice-messages', JSON.stringify(next));
      return { voiceMessages: next };
    });
  },
  toggleVoiceText: (msgKey) => {
    set((state) => {
      const vm = state.voiceMessages[msgKey];
      if (!vm) return {};
      const next = {
        ...state.voiceMessages,
        [msgKey]: { ...vm, showText: !vm.showText },
      };
      localStorage.setItem('tiangong-voice-messages', JSON.stringify(next));
      return { voiceMessages: next };
    });
  },
  isLoadingSessions: false,
  agents: [],
  selectedAgentTab: null,

  // 加载会话列表
  loadSessions: async () => {
    set({ isLoadingSessions: true });
    try {
      const sessions = await api.getSessions();
      const { activeSessionId, isNewConversation } = get();
      let newConversationId = get().newConversationId;
      if (!activeSessionId && isNewConversation && !newConversationId) {
        const requestVersion = ++newConversationRequestVersion;
        const generatedId = await api.newSessionId();
        newConversationId = requestVersion === newConversationRequestVersion
          ? generatedId
          : get().newConversationId;
      }
      const initialCache = newConversationId
        ? get().inputCaches[newConversationId] ?? emptyInputCache()
        : null;
      set((state) => ({
        sessions,
        isLoadingSessions: false,
        isNewConversation: state.activeSessionId ? state.isNewConversation : true,
        newConversationId,
        sessionCwd: state.activeSessionId ? state.sessionCwd : state.workspaceDir,
        inputCaches: newConversationId && initialCache
          ? { ...state.inputCaches, [newConversationId]: initialCache }
          : state.inputCaches,
      }));
      if (newConversationId && initialCache) {
        syncInputCacheInBackground(get().syncInputCache(newConversationId, initialCache));
      }

      // 从后端恢复思考强度设置
      api.getReasoningEffort().then((effort) => {
        set({ reasoningEffort: effort });
      }).catch(console.error);
    } catch (error) {
      console.error('加载会话失败:', error);
      set({ isLoadingSessions: false });
    }
  },

  // 开始新对话：只预留 ID 并初始化输入缓存，首次发送时才由 Core 创建 Session。
  // targetCwd 用于在指定 workspace 分组下创建对话；不传则用全局 workspace。
  startNewConversation: async (targetCwd?: string) => {
    switchRequestVersion += 1;
    const requestVersion = ++newConversationRequestVersion;
    const workspaceDir = get().workspaceDir;
    const newConversationCwd = targetCwd || workspaceDir;
    const { reasoningEffortPerSession } = get();
    const newConversationEffort = reasoningEffortPerSession['__new_conversation__'] || 'medium';
    try {
      const newConversationId = await api.newSessionId();
      if (requestVersion !== newConversationRequestVersion) return;
      const previousCacheId = get().newConversationId;
      const previousCacheIsSending = previousCacheId
        ? get().inputCaches[previousCacheId]?.is_sending === true
        : false;
      if (previousCacheId && previousCacheId !== newConversationId && !previousCacheIsSending) {
        discardInputCacheSyncQueue(previousCacheId);
        api.removeInputCache(previousCacheId).catch((error) =>
          console.error('清理旧输入缓存失败:', error),
        );
        api.terminalDestroySession(previousCacheId).catch((error) =>
          console.error('销毁旧新对话终端失败:', error),
        );
      }
      const initialCache = emptyInputCache();
      set((state) => {
        const inputCaches = { ...state.inputCaches };
        if (previousCacheId && !previousCacheIsSending) delete inputCaches[previousCacheId];
        inputCaches[newConversationId] = initialCache;
        return {
          isNewConversation: true,
          activeSessionId: null,
          newConversationId,
          inputCaches,
          messages: [],
          runStatus: 'idle',
          runSummary: '',
          lastUsage: null,
          tokenStats: null,
          currentPlan: undefined,
          streamingMessageId: null,
          streamingContent: '',
          streamingReasoningContent: '',
          sessionCwd: newConversationCwd,
          agents: [],
          selectedAgentTab: null,
          reasoningEffort: newConversationEffort,
        };
      });
      syncInputCacheInBackground(get().syncInputCache(newConversationId, initialCache));
    } catch (error) {
      console.error('开始新对话失败:', error);
    }
  },

  // 切换会话
  switchSession: async (id: string) => {
    newConversationRequestVersion += 1;
    const requestVersion = ++switchRequestVersion;
    try {
      await api.switchSession(id);
      const [snapshot, cwd, storedCache, sessionEffort] = await Promise.all([
        api.getRunSnapshot(),
        api.getSessionCwd(),
        api.getInputCache(id),
        api.getReasoningEffort(id),
      ]);
      if (requestVersion !== switchRequestVersion) return;
      set((state) => ({
        isNewConversation: false,
        activeSessionId: id,
        newConversationId: null,
        inputCaches: {
          ...state.inputCaches,
          [id]: state.inputCaches[id]?.revision > storedCache.revision
            ? state.inputCaches[id]
            : cloneInputCache(storedCache),
        },
        messages: snapshot.messages,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
        currentPlan: snapshot.current_plan,
        sessionCwd: cwd,
        streamingMessageId: null,
        streamingContent: '',
        streamingReasoningContent: '',
        agents: parseAgentsFromMessages(snapshot.messages),
        selectedAgentTab: null,
        reasoningEffort: sessionEffort,
        reasoningEffortPerSession: {
          ...state.reasoningEffortPerSession,
          [id]: sessionEffort,
        },
      }));
    } catch (error) {
      console.error('切换会话失败:', error);
    }
  },

  // 删除当前会话
  deleteSession: async () => {
    try {
      await api.deleteSession();
      const [sessions, snapshot] = await Promise.all([api.getSessions(), api.getRunSnapshot()]);
      const nextSessionId = snapshot.last_session_id ?? sessions[0]?.id ?? null;
      const nextCache = nextSessionId ? await api.getInputCache(nextSessionId) : null;
      const reservedSessionId = nextSessionId ? null : await api.newSessionId();

      set((state) => ({
        sessions,
        isNewConversation: !nextSessionId,
        activeSessionId: nextSessionId,
        newConversationId: reservedSessionId,
        inputCaches: nextSessionId && nextCache
          ? { ...state.inputCaches, [nextSessionId]: cloneInputCache(nextCache) }
          : reservedSessionId
            ? { ...state.inputCaches, [reservedSessionId]: emptyInputCache() }
            : state.inputCaches,
        messages: snapshot.messages,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
        sessionCwd: nextSessionId ? state.sessionCwd : state.workspaceDir,
        agents: parseAgentsFromMessages(snapshot.messages),
        selectedAgentTab: null,
      }));
    } catch (error) {
      console.error('删除会话失败:', error);
    }
  },

  // 删除指定 workspace（cwd）下的所有会话
  deleteSessionsByCwd: async (cwd: string) => {
    try {
      // 删除前记录是否正在编辑新对话；删除分组不应打断当前输入。
      const wasNewConversation = get().isNewConversation;
      await api.deleteSessionsByCwd(cwd);
      const sessions = await api.getSessions();

      if (wasNewConversation) {
        // 新对话：仅刷新会话列表，保持当前输入不变。
        set({ sessions });
        return;
      }

      // 已有对话：跟随后端活跃会话快照。
      const [snapshot, sessionCwd] = await Promise.all([api.getRunSnapshot(), api.getSessionCwd()]);
      const nextSessionId = snapshot.last_session_id ?? sessions[0]?.id ?? null;
      const nextCache = nextSessionId ? await api.getInputCache(nextSessionId) : null;
      const reservedSessionId = nextSessionId ? null : await api.newSessionId();

      set((state) => ({
        sessions,
        isNewConversation: !nextSessionId,
        activeSessionId: nextSessionId,
        newConversationId: reservedSessionId,
        inputCaches: nextSessionId && nextCache
          ? { ...state.inputCaches, [nextSessionId]: cloneInputCache(nextCache) }
          : reservedSessionId
            ? { ...state.inputCaches, [reservedSessionId]: emptyInputCache() }
            : state.inputCaches,
        messages: snapshot.messages,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
        sessionCwd: nextSessionId ? sessionCwd : state.workspaceDir,
        agents: parseAgentsFromMessages(snapshot.messages),
        selectedAgentTab: null,
      }));
    } catch (error) {
      console.error('删除 workspace 会话失败:', error);
    }
  },

  syncInputCache: (cacheKey, cache, immediate = false, claimRevision) => {
    let queue = inputCacheSyncQueues.get(cacheKey);
    if (!queue) {
      queue = {
        pending: null,
        pendingVersion: 0,
        claimed: null,
        running: false,
        debounceVersion: 0,
        nextVersion: 0,
        immediateThroughVersion: 0,
        waiters: [],
      };
      inputCacheSyncQueues.set(cacheKey, queue);
    }
    const enqueueVersion = queue.nextVersion + 1;
    queue.nextVersion = enqueueVersion;
    if (claimRevision !== undefined) {
      // 发送快照必须保留精确 revision，不能被正在排队的 R+1 新输入合并。
      // 更旧的普通 pending 可由该快照覆盖；快照之后的新输入会另存于 pending。
      queue.claimed = {
        cache: cloneInputCache(cache),
        version: enqueueVersion,
        revision: claimRevision,
      };
      queue.pending = null;
    } else {
      queue.pending = cloneInputCache(cache);
      queue.pendingVersion = enqueueVersion;
    }
    if (immediate) queue.immediateThroughVersion = enqueueVersion;
    queue.debounceVersion += 1;
    const debounceVersion = queue.debounceVersion;
    const completion = new Promise<InputCache>((resolve, reject) => {
      queue!.waiters.push({ version: enqueueVersion, resolve, reject });
    });

    if (!queue.running) {
      if (immediate) {
        void get().flushInputCacheQueue(cacheKey);
      } else {
        void waitForInputCacheDebounce().then(() => {
          const latest = inputCacheSyncQueues.get(cacheKey);
          if (
            latest === queue
            && !latest.running
            && latest.debounceVersion === debounceVersion
          ) {
            void get().flushInputCacheQueue(cacheKey);
          }
        });
      }
    }
    return completion;
  },

  flushInputCacheQueue: async (cacheKey) => {
    const queue = inputCacheSyncQueues.get(cacheKey);
    if (!queue || queue.running || (!queue.claimed && !queue.pending)) return;
    const claimed = queue.claimed;
    const submitted = claimed?.cache ?? queue.pending!;
    const submittedVersion = claimed?.version ?? queue.pendingVersion;
    const submittedClaimRevision = claimed?.revision;
    if (claimed) {
      queue.claimed = null;
    } else {
      queue.pending = null;
    }
    queue.running = true;
    const requestVersion = (inputCacheSyncRequestVersions.get(cacheKey) ?? 0) + 1;
    inputCacheSyncRequestVersions.set(cacheKey, requestVersion);
    try {
      const stored = await api.setInputCache(
        cacheKey,
        submitted,
        submittedClaimRevision ?? undefined,
      );
      if (
        inputCacheSyncQueues.get(cacheKey) !== queue
        || inputCacheSyncRequestVersions.get(cacheKey) !== requestVersion
      ) {
        return;
      }
      if (!queue.pending) {
        set((state) => ({
          inputCaches: mergeStoredInputCache(
            state.inputCaches,
            cacheKey,
            submitted.revision,
            stored,
          ),
        }));
      } else {
        queue.pending = rebasePendingInputCacheOnStored(queue.pending, submitted, stored);
      }
      const completed = queue.waiters.filter((waiter) => waiter.version <= submittedVersion);
      queue.waiters = queue.waiters.filter((waiter) => waiter.version > submittedVersion);
      for (const waiter of completed) waiter.resolve(cloneInputCache(stored));
    } catch (error) {
      console.error('同步输入缓存失败:', error);
      const failed = queue.waiters.filter((waiter) => waiter.version <= submittedVersion);
      queue.waiters = queue.waiters.filter((waiter) => waiter.version > submittedVersion);
      for (const waiter of failed) waiter.reject(error);
    } finally {
      queue.running = false;
      if (
        inputCacheSyncQueues.get(cacheKey) === queue
        && (queue.claimed || queue.pending)
      ) {
        const nextVersion = queue.claimed?.version ?? queue.pendingVersion;
        if (queue.claimed || nextVersion <= queue.immediateThroughVersion) {
          void get().flushInputCacheQueue(cacheKey);
        } else {
          queue.debounceVersion += 1;
          const debounceVersion = queue.debounceVersion;
          void waitForInputCacheDebounce().then(() => {
            const latest = inputCacheSyncQueues.get(cacheKey);
            if (
              latest === queue
              && !latest.running
              && latest.debounceVersion === debounceVersion
            ) {
              void get().flushInputCacheQueue(cacheKey);
            }
          });
        }
      }
    }
  },

  // 普通发送：新对话和已有会话都直接向目标 Core 投递。
  sendMessage: async (cacheKey, content, attachments, revision, trustMode) => {
    let deliveryAttachments = attachments.map((attachment) => ({ ...attachment }));
    const startsNewConversation = get().newConversationId === cacheKey;
    const initialCwd = get().sessionCwd || get().workspaceDir;
    const initialReasoningEffort = get().reasoningEffort;
    const navigationVersion = switchRequestVersion;
    let sendingCache: InputCache | undefined;
    set((state) => {
      const inputCaches = setInputCacheSending(state.inputCaches, cacheKey, true);
      sendingCache = inputCaches[cacheKey];
      return { inputCaches };
    });

    try {
      if (sendingCache) {
        const stored = await get().syncInputCache(
          cacheKey,
          sendingCache,
          true,
          revision,
        );
        if (stored.revision !== revision) {
          throw new Error('输入已在发送前发生变化，请重试');
        }
        deliveryAttachments = stored.attachments.map((attachment) => ({ ...attachment }));
      }

      await api.sendMessage(
        cacheKey,
        content,
        deliveryAttachments,
        revision,
        startsNewConversation ? initialCwd : undefined,
        startsNewConversation ? trustMode : undefined,
        startsNewConversation ? initialReasoningEffort : undefined,
      );

      const shouldActivate = startsNewConversation
        && switchRequestVersion === navigationVersion
        && get().isNewConversation
        && get().activeSessionId === null
        && get().newConversationId === cacheKey;
      if (shouldActivate) {
        await get().switchSession(cacheKey);
        if (get().activeSessionId === cacheKey) {
          const sessions = await api.getSessions();
          set({ sessions });
        }
      }

      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = settleInputCacheSend(
          state.inputCaches,
          cacheKey,
          revision,
          true,
        );
        settledCache = inputCaches[cacheKey];
        const isCurrent = state.activeSessionId === cacheKey;
        return {
          inputCaches,
          runStatus: isCurrent ? 'executing' : state.runStatus,
          sessionRunStatuses: {
            ...state.sessionRunStatuses,
            [cacheKey]: 'executing',
          },
        };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(cacheKey, settledCache));
      }
      return true;
    } catch (error) {
      console.error('发送消息失败:', error);
      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = settleInputCacheSend(
          state.inputCaches,
          cacheKey,
          revision,
          false,
        );
        settledCache = inputCaches[cacheKey];
        return { inputCaches };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(cacheKey, settledCache));
      }
      return false;
    }
  },

  appendMessage: async (sessionId, content, attachments, revision) => {
    let deliveryAttachments = attachments.map((attachment) => ({ ...attachment }));
    let sendingCache: InputCache | undefined;
    set((state) => {
      const inputCaches = setInputCacheSending(state.inputCaches, sessionId, true);
      sendingCache = inputCaches[sessionId];
      return { inputCaches };
    });
    try {
      if (sendingCache) {
        const stored = await get().syncInputCache(
          sessionId,
          sendingCache,
          true,
          revision,
        );
        if (stored.revision !== revision) {
          throw new Error('输入已在追加前发生变化，请重试');
        }
        deliveryAttachments = stored.attachments.map((attachment) => ({ ...attachment }));
      }
      const appended = await api.appendMessage(
        sessionId,
        content,
        deliveryAttachments,
        revision,
      );
      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = settleInputCacheSend(
          state.inputCaches,
          sessionId,
          revision,
          appended,
        );
        settledCache = inputCaches[sessionId];
        return { inputCaches };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(sessionId, settledCache));
      }
      return appended;
    } catch (error) {
      console.error('追加消息失败:', error);
      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = settleInputCacheSend(state.inputCaches, sessionId, revision, false);
        settledCache = inputCaches[sessionId];
        return { inputCaches };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(sessionId, settledCache));
      }
      return false;
    }
  },

  // 编辑态有自己的 revision；这里只按显式 session_id 更新该会话发送状态。
  editAndResend: async (
    sessionId,
    messageId,
    newContent,
    attachments,
    revision,
    baseContent,
  ) => {
    let sendingCache: InputCache | undefined;
    set((state) => {
      const inputCaches = setInputCacheSending(state.inputCaches, sessionId, true);
      sendingCache = inputCaches[sessionId];
      return { inputCaches };
    });
    try {
      if (sendingCache) {
        await get().syncInputCache(sessionId, sendingCache, true);
      }
      await api.editAndResend(
        sessionId,
        messageId,
        newContent,
        attachments,
        revision,
        baseContent,
      );
      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = setInputCacheSending(state.inputCaches, sessionId, false);
        settledCache = inputCaches[sessionId];
        const isCurrent = state.activeSessionId === sessionId;
        return {
          inputCaches,
          runStatus: isCurrent ? 'executing' : state.runStatus,
          sessionRunStatuses: { ...state.sessionRunStatuses, [sessionId]: 'executing' },
        };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(sessionId, settledCache));
      }
      return true;
    } catch (error) {
      console.error('编辑重发失败:', error);
      let settledCache: InputCache | undefined;
      set((state) => {
        const inputCaches = setInputCacheSending(state.inputCaches, sessionId, false);
        settledCache = inputCaches[sessionId];
        return { inputCaches };
      });
      if (settledCache) {
        syncInputCacheInBackground(get().syncInputCache(sessionId, settledCache));
      }
      return false;
    }
  },

  // 取消当前执行
  cancelTurn: async () => {
    try {
      const cancelled = await api.cancelTurn();
      if (cancelled) {
        // 立即获取最新快照确保状态一致（避免轮询线程的旧快照覆盖）
        const snapshot = await api.getRunSnapshot();
        const cacheKey = selectCurrentInputCacheKey(get());
        let settledCache: InputCache | undefined;
        set((state) => {
          const inputCaches = cacheKey
            ? setInputCacheSending(state.inputCaches, cacheKey, false)
            : state.inputCaches;
          settledCache = cacheKey ? inputCaches[cacheKey] : undefined;
          return {
            runStatus: 'idle',
            inputCaches,
            messages: snapshot.messages,
            currentPlan: snapshot.current_plan,
            lastUsage: snapshot.last_usage ?? null,
            tokenStats: snapshot.token_stats ?? null,
          };
        });
        if (cacheKey && settledCache) {
          syncInputCacheInBackground(get().syncInputCache(cacheKey, settledCache));
        }
        // 刷新会话列表
        api.getSessions().then((sessions) => set({ sessions })).catch(console.error);
      }
      return cancelled;
    } catch (error) {
      console.error('取消执行失败:', error);
      return false;
    }
  },

  cancelAgent: async (role: string) => {
    try {
      const cancelled = await api.cancelAgent(role);
      if (cancelled) {
        set((state) => ({
          agents: state.agents.map((agent) =>
            agent.role === role ? { ...agent, status: 'idle' } : agent
          ),
        }));
      }
      return cancelled;
    } catch (error) {
      console.error('取消 Agent 执行失败:', error);
      return false;
    }
  },

  setInputCacheText: (cacheKey: string, content: string) => {
    let nextCache: InputCache | undefined;
    set((state) => {
      const inputCaches = updateInputCacheText(state.inputCaches, cacheKey, content);
      nextCache = inputCaches[cacheKey];
      return { inputCaches };
    });
    if (nextCache) syncInputCacheInBackground(get().syncInputCache(cacheKey, nextCache));
  },

  setInputCacheAttachments: (cacheKey: string, attachments: RawAttachment[]) => {
    let nextCache: InputCache | undefined;
    set((state) => {
      const inputCaches = updateInputCacheAttachments(state.inputCaches, cacheKey, attachments);
      nextCache = inputCaches[cacheKey];
      return { inputCaches };
    });
    if (nextCache) syncInputCacheInBackground(get().syncInputCache(cacheKey, nextCache));
  },

  // 设置工作目录
  setSessionCwd: async (cwd: string) => {
    try {
      // 新对话只在前端保存，首次发送时作为 Core 创建 Session 的初始目录。
      const { isNewConversation, activeSessionId } = get();
      if (!isNewConversation && activeSessionId) {
        await api.setSessionCwd(activeSessionId, cwd);
      }
      set({ sessionCwd: cwd });
    } catch (error) {
      console.error('设置工作目录失败:', error);
      throw error;
    }
  },

  // 设置 Desktop 工作空间
  setWorkspaceDir: async (workspaceDir: string) => {
    try {
      await api.setWorkspaceDir(workspaceDir);
      set({ workspaceDir });
    } catch (error) {
      console.error('设置工作空间失败:', error);
      throw error;
    }
  },

  // 加载 MCP 服务器
  loadMcpServers: async () => {
    try {
      const servers = await api.getMcpServers();
      set({ mcpServers: servers });
    } catch (error) {
      console.error('加载 MCP 服务器失败:', error);
    }
  },

  // 加载 Skills
  loadSkills: async () => {
    try {
      const skills = await api.getSkills();
      set({ skills });
    } catch (error) {
      console.error('加载 Skills 失败:', error);
    }
  },

  setSelectedAgentTab: (tab: string | null) => {
    const state = get();
    const cacheKey = selectCurrentInputCacheKey(state);
    const currentCache = selectCurrentInputCache(state);
    const nextInput = applyAgentMention(currentCache.text, tab, state.agents);
    set({ selectedAgentTab: tab });
    if (cacheKey) get().setInputCacheText(cacheKey, nextInput);
  },

  beginContextManagement: (summary: string) => {
    const { activeSessionId } = get();
    set((state) => ({
      runStatus: 'executing',
      runSummary: summary,
      streamingMessageId: null,
      streamingContent: '',
      streamingReasoningContent: '',
      sessionRunStatuses: activeSessionId
        ? { ...state.sessionRunStatuses, [activeSessionId]: 'executing' }
        : state.sessionRunStatuses,
    }));
  },

  endContextManagement: () => {
    const { activeSessionId, runSummary } = get();
    set((state) => {
      const nextStatuses = { ...state.sessionRunStatuses };
      if (activeSessionId) {
        delete nextStatuses[activeSessionId];
      }
      return {
        runStatus: 'idle',
        runSummary: runSummary.includes('上下文') ? runSummary : '',
        sessionRunStatuses: nextStatuses,
      };
    });
  },

  // 从快照更新状态
  updateFromSnapshot: (snapshot: RunSnapshot) => {
    const { activeSessionId, isNewConversation, sessionRunStatuses: prevStatuses } = get();
    const pendingIds = snapshot.pending_session_ids || [];

    // 基于 pending_session_ids 构建新的运行状态表
    const newStatuses: Record<string, string> = {};
    for (const sid of pendingIds) {
      // 如果全局 last_session_id 匹配，用精确状态；否则标记为 executing
      if (snapshot.last_session_id === sid) {
        newStatuses[sid] = snapshot.status !== 'idle' ? snapshot.status : 'executing';
      } else {
        newStatuses[sid] = prevStatuses[sid] || 'executing';
      }
    }

    const appIsForeground = document.visibilityState === 'visible' && document.hasFocus();

    // 检测刚完成的后台会话并发送通知。
    // 后台包含两类场景：非当前查看会话完成，或当前会话在应用失焦/隐藏时完成。
    for (const sid of Object.keys(prevStatuses)) {
      if (!newStatuses[sid] && (sid !== activeSessionId || !appIsForeground)) {
        // 该会话刚从运行中变为完成
        const session = get().sessions.find(s => s.id === sid);
        const title = session?.title || '对话';
        notifyBackgroundSessionCompleted(title, sid).catch(console.warn);
      }
    }

    set({ sessionRunStatuses: newStatuses });

    // 只有快照明确属于当前会话时才采用其精确状态；后台会话的迟到快照
    // 只能通过 pending_session_ids 影响自己的状态，不能覆盖当前会话。
    const currentState = get();
    const prevStatus = currentState.runStatus;
    const prevSending = selectCurrentIsSending(currentState);
    const snapshotTargetsActive = !!activeSessionId
      && snapshot.last_session_id === activeSessionId;
    const derivedActiveStatus = activeSessionId ? newStatuses[activeSessionId] : undefined;
    const nextStatus = isNewConversation
      ? 'idle'
      : snapshotTargetsActive
        ? snapshot.status
        : derivedActiveStatus ?? 'idle';
    const snapshotApprovalRequestId = (snapshot as any).approval_request_id
      || (snapshot as any).approvalRequestId
      || null;
    const nextSummary = snapshotTargetsActive
      ? snapshot.summary || ''
      : derivedActiveStatus
        ? currentState.runSummary
        : '';
    const nextApprovalRequestId = snapshotTargetsActive
      ? snapshotApprovalRequestId
      : derivedActiveStatus
        ? currentState.approvalRequestId
        : null;
    const isContextManagementSnapshot = nextSummary.includes('上下文')
      || nextSummary.includes('正在压缩');
    // 防止取消后被旧快照覆盖
    const effectiveStatus = snapshotTargetsActive && (
      prevStatus === 'idle'
      && !prevSending
      && nextStatus !== 'idle'
      && nextStatus !== 'waiting_approval'
      && !nextApprovalRequestId
      && !isContextManagementSnapshot
    )
      ? 'idle'
      : nextStatus;
    set({
      runStatus: effectiveStatus,
      runSummary: nextSummary,
      lastDurationMs: snapshotTargetsActive
        ? snapshot.last_duration_ms ?? null
        : currentState.lastDurationMs,
      lastUsage: snapshotTargetsActive
        ? snapshot.last_usage ?? null
        : currentState.lastUsage,
      tokenStats: snapshotTargetsActive
        ? snapshot.token_stats ?? null
        : currentState.tokenStats,
      approvalRequestId: nextApprovalRequestId,
    });

    // 新对话尚无 Session，或快照不属于当前会话时，不更新消息/流式内容。
    if (isNewConversation || (snapshot.last_session_id && snapshot.last_session_id !== activeSessionId)) {
      return;
    }

    const { messages: oldMessages, streamingMessageId: oldStreamingId } = get();
    const newMessages = mergeSnapshotMessages(oldMessages, snapshot.messages);
    const shouldRefreshAgents = shouldRefreshAgentsFromSnapshot(oldMessages, newMessages);

    // 检测最后一条消息是否是新的或内容在更新
    let streamingId: string | null = null;
    let streamingContent = '';
    let streamingReasoningContent = '';

    if (newMessages.length > 0) {
      // 找最后一条 assistant 消息（不一定是数组最后一条，可能后面还有系统消息）
      let lastAssistant = null;
      for (let i = newMessages.length - 1; i >= 0; i--) {
        if (newMessages[i].role === 'assistant' && !newMessages[i].worker_id) {
          lastAssistant = newMessages[i];
          break;
        }
      }

      if (lastAssistant) {
        const oldAssistant = oldMessages.find(m => m.id === lastAssistant!.id);
        const hasRenderableMedia = !!lastAssistant.media && lastAssistant.media.length > 0;

        // 新出现的 assistant 消息、正文增长或 thinking 增长都属于流式更新。
        const isNew = !oldAssistant;
        const oldText = oldAssistant ? textContent(oldAssistant) : '';
        const newText = textContent(lastAssistant);
        const isContentGrowing = oldAssistant &&
          oldText !== newText &&
          newText.length > oldText.length;
        const isReasoningGrowing = oldAssistant &&
          oldAssistant.reasoning_content !== lastAssistant.reasoning_content &&
          lastAssistant.reasoning_content.length > oldAssistant.reasoning_content.length;

        if ((isNew || isContentGrowing || isReasoningGrowing) && !hasRenderableMedia) {
          streamingId = lastAssistant.id;
          streamingContent = textContent(lastAssistant);
          streamingReasoningContent = lastAssistant.reasoning_content;
        }
      }
    }

    // 如果没有流式消息，清空状态
    if (!streamingId && oldStreamingId) {
      streamingId = null;
      streamingContent = '';
      streamingReasoningContent = '';
    }

    set({
      messages: newMessages,
      currentPlan: snapshot.current_plan,
      streamingMessageId: streamingId,
      streamingContent,
      streamingReasoningContent,
      agents: shouldRefreshAgents ? parseAgentsFromMessages(newMessages) : get().agents,
    });

    // 状态变为 idle 时刷新会话列表（更新 message_count、标题等）
    if (effectiveStatus === 'idle' && prevStatus !== 'idle') {
      api.getSessions().then((sessions) => {
        set({ sessions });
      }).catch(console.error);
    }
  },
}));
