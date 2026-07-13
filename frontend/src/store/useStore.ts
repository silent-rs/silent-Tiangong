import { create } from 'zustand';
import { api, textContent } from '../api/tauri';
import type {
  ContentBlock,
  McpServer,
  Message,
  RawAttachment,
  RunSnapshot,
  Session,
  SessionInputDraft,
  Skill,
  TaskPlan,
  TokenStats,
} from '../api/tauri';
import { notifyBackgroundSessionCompleted } from '../utils/desktopNotification';
import {
  cloneSessionInputDraft,
  emptySessionInputDraft,
  getSessionInputDraft,
  mergePersistedDraft,
  migrateDraftKey,
  setDraftSending,
  settleDraftSend,
  updateDraftAttachments,
  updateDraftText,
  type SessionDraftMap,
} from './sessionDrafts';

let switchRequestVersion = 0;
let createDraftRequestVersion = 0;
const draftPersistRequestVersions = new Map<string, number>();

interface DraftPersistQueue {
  pending: SessionInputDraft | null;
  pendingVersion: number;
  claimed: {
    draft: SessionInputDraft;
    version: number;
    revision: number;
  } | null;
  running: boolean;
  debounceVersion: number;
  nextVersion: number;
  immediateThroughVersion: number;
  waiters: Array<{
    version: number;
    resolve: (draft: SessionInputDraft) => void;
    reject: (error: unknown) => void;
  }>;
}

const draftPersistQueues = new Map<string, DraftPersistQueue>();

function sameAttachments(left: RawAttachment[], right: RawAttachment[]): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function rebasePendingDraftOnPersisted(
  pending: SessionInputDraft,
  submitted: SessionInputDraft,
  persisted: SessionInputDraft,
): SessionInputDraft {
  return {
    text: pending.text === submitted.text ? persisted.text : pending.text,
    attachments: sameAttachments(pending.attachments, submitted.attachments)
      ? persisted.attachments.map((attachment) => ({ ...attachment }))
      : pending.attachments,
    is_sending: pending.is_sending,
    revision: pending.revision,
  };
}

function waitForDraftDebounce(): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, 200));
}

function discardDraftPersistQueue(draftKey: string): void {
  const queue = draftPersistQueues.get(draftKey);
  if (queue) {
    for (const waiter of queue.waiters) waiter.resolve(emptySessionInputDraft());
    queue.waiters = [];
  }
  draftPersistQueues.delete(draftKey);
  draftPersistRequestVersions.delete(draftKey);
}

function persistDraftInBackground(promise: Promise<SessionInputDraft>): void {
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
  draftSessionId: string | null;
  inputDrafts: SessionDraftMap;
  /**
   * 草稿态终端的稳定临时 id。
   * 草稿态（activeSessionId=null）无法用真实 session_id 创建 PTY，
   * 用此临时 id 先创建一个草稿 PTY 供用户在终端面板操作；
   * 首条消息转正后，该 PTY 会迁移归属到真实 session_id。
   * 转正完成（或切换/删除对话）时清空。
   */
  draftTerminalId: string | null;
  workspaceTabsTransfer: {
    fromSessionId: string;
    toSessionId: string;
    version: number;
  } | null;
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

  // 草稿模式
  isDraft: boolean;

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
  createSession: (targetCwd?: string) => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: () => Promise<void>;
  deleteSessionsByCwd: (cwd: string) => Promise<void>;

  sendMessage: (
    draftKey: string,
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

  setDraftText: (draftKey: string, content: string) => void;
  setDraftAttachments: (draftKey: string, attachments: RawAttachment[]) => void;
  persistInputDraft: (
    draftKey: string,
    draft: SessionInputDraft,
    immediate?: boolean,
    claimRevision?: number,
  ) => Promise<SessionInputDraft>;
  flushInputDraftQueue: (draftKey: string) => Promise<void>;

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

export function selectCurrentDraftKey(state: AppState): string | null {
  return state.activeSessionId ?? state.draftSessionId;
}

export function selectCurrentInputDraft(state: AppState): SessionInputDraft {
  return getSessionInputDraft(state.inputDrafts, selectCurrentDraftKey(state));
}

export function selectCurrentIsSending(state: AppState): boolean {
  return selectCurrentInputDraft(state).is_sending;
}

export const useStore = create<AppState>((set, get) => ({
  // 初始状态
  sessions: [],
  activeSessionId: null as string | null,
  draftSessionId: null as string | null,
  inputDrafts: {},
  draftTerminalId: null as string | null,
  workspaceTabsTransfer: null,
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
  isDraft: true,
  updateAvailable: null,
  setUpdateAvailable: (info) => set({ updateAvailable: info }),
  pendingSettingsTab: null,
  setPendingSettingsTab: (tab) => set({ pendingSettingsTab: tab }),
  reasoningEffort: 'medium',
  reasoningEffortPerSession: {},
  setReasoningEffort: (effort: string) => {
    const { activeSessionId, reasoningEffortPerSession } = get();
    const key = activeSessionId || '__draft__';
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
      const { activeSessionId, isDraft } = get();
      let draftSessionId = get().draftSessionId;
      if (!activeSessionId && isDraft && !draftSessionId) {
        const requestVersion = ++createDraftRequestVersion;
        const generatedDraftId = await api.newDraftId();
        draftSessionId = requestVersion === createDraftRequestVersion
          ? generatedDraftId
          : get().draftSessionId;
      }
      const initialDraft = draftSessionId
        ? get().inputDrafts[draftSessionId] ?? emptySessionInputDraft()
        : null;
      set((state) => ({
        sessions,
        isLoadingSessions: false,
        isDraft: state.activeSessionId ? state.isDraft : true,
        draftSessionId,
        draftTerminalId: state.activeSessionId ? state.draftTerminalId : draftSessionId,
        sessionCwd: state.activeSessionId ? state.sessionCwd : state.workspaceDir,
        inputDrafts: draftSessionId && initialDraft
          ? { ...state.inputDrafts, [draftSessionId]: initialDraft }
          : state.inputDrafts,
      }));
      if (draftSessionId && initialDraft) {
        persistDraftInBackground(get().persistInputDraft(draftSessionId, initialDraft));
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

  // 创建新会话 — 纯前端草稿模式，不立即调后端
  // targetCwd 用于在指定 workspace 分组下创建对话；不传则用全局 workspace。
  createSession: async (targetCwd?: string) => {
    switchRequestVersion += 1;
    const requestVersion = ++createDraftRequestVersion;
    const workspaceDir = get().workspaceDir;
    const draftCwd = targetCwd || workspaceDir;
    const { reasoningEffortPerSession } = get();
    const draftEffort = reasoningEffortPerSession['__draft__'] || 'medium';
    try {
      const draftSessionId = await api.newDraftId();
      if (requestVersion !== createDraftRequestVersion) return;
      const previousDraftId = get().draftSessionId;
      const previousDraftIsSending = previousDraftId
        ? get().inputDrafts[previousDraftId]?.is_sending === true
        : false;
      if (previousDraftId && previousDraftId !== draftSessionId && !previousDraftIsSending) {
        discardDraftPersistQueue(previousDraftId);
        api.removeInputDraft(previousDraftId).catch((error) =>
          console.error('清理旧草稿输入失败:', error),
        );
        api.terminalDestroySession(previousDraftId).catch((error) =>
          console.error('销毁陈旧草稿 PTY 失败:', error),
        );
      }
      const initialDraft = emptySessionInputDraft();
      set((state) => {
        const inputDrafts = { ...state.inputDrafts };
        if (previousDraftId && !previousDraftIsSending) delete inputDrafts[previousDraftId];
        inputDrafts[draftSessionId] = initialDraft;
        return {
          isDraft: true,
          activeSessionId: null,
          draftSessionId,
          draftTerminalId: draftSessionId,
          inputDrafts,
          messages: [],
          runStatus: 'idle',
          runSummary: '',
          lastUsage: null,
          tokenStats: null,
          currentPlan: undefined,
          streamingMessageId: null,
          streamingContent: '',
          streamingReasoningContent: '',
          sessionCwd: draftCwd,
          agents: [],
          selectedAgentTab: null,
          reasoningEffort: draftEffort,
        };
      });
      persistDraftInBackground(get().persistInputDraft(draftSessionId, initialDraft));
    } catch (error) {
      console.error('创建草稿会话失败:', error);
    }
  },

  // 切换会话
  switchSession: async (id: string) => {
    createDraftRequestVersion += 1;
    const requestVersion = ++switchRequestVersion;
    try {
      await api.switchSession(id);
      const [snapshot, cwd, persistedDraft, sessionEffort] = await Promise.all([
        api.getRunSnapshot(),
        api.getSessionCwd(),
        api.getInputDraft(id),
        api.getReasoningEffort(id),
      ]);
      if (requestVersion !== switchRequestVersion) return;
      set((state) => ({
        isDraft: false,
        activeSessionId: id,
        draftTerminalId: null,
        workspaceTabsTransfer: null,
        inputDrafts: {
          ...state.inputDrafts,
          [id]: state.inputDrafts[id]?.revision > persistedDraft.revision
            ? state.inputDrafts[id]
            : cloneSessionInputDraft(persistedDraft),
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
      const nextDraft = nextSessionId ? await api.getInputDraft(nextSessionId) : null;
      const newDraftId = nextSessionId ? null : await api.newDraftId();

      set((state) => ({
        sessions,
        isDraft: !nextSessionId,
        activeSessionId: nextSessionId,
        draftSessionId: newDraftId ?? state.draftSessionId,
        draftTerminalId: newDraftId,
        workspaceTabsTransfer: null,
        inputDrafts: nextSessionId && nextDraft
          ? { ...state.inputDrafts, [nextSessionId]: cloneSessionInputDraft(nextDraft) }
          : newDraftId
            ? { ...state.inputDrafts, [newDraftId]: emptySessionInputDraft() }
            : state.inputDrafts,
        messages: snapshot.messages,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
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
      // 删除前记录是否处于草稿态：删除分组不应打断当前草稿
      const wasDraft = get().isDraft;
      await api.deleteSessionsByCwd(cwd);
      const sessions = await api.getSessions();

      if (wasDraft) {
        // 草稿态：仅刷新会话列表，保持草稿不变
        set({ sessions });
        return;
      }

      // 非草稿态：跟随后端活跃会话快照
      const [snapshot, sessionCwd] = await Promise.all([api.getRunSnapshot(), api.getSessionCwd()]);
      const nextSessionId = snapshot.last_session_id ?? sessions[0]?.id ?? null;
      const nextDraft = nextSessionId ? await api.getInputDraft(nextSessionId) : null;

      set((state) => ({
        sessions,
        isDraft: !nextSessionId,
        activeSessionId: nextSessionId,
        draftTerminalId: null,
        workspaceTabsTransfer: null,
        inputDrafts: nextSessionId && nextDraft
          ? { ...state.inputDrafts, [nextSessionId]: cloneSessionInputDraft(nextDraft) }
          : state.inputDrafts,
        messages: snapshot.messages,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
        sessionCwd,
        agents: parseAgentsFromMessages(snapshot.messages),
        selectedAgentTab: null,
      }));
    } catch (error) {
      console.error('删除 workspace 会话失败:', error);
    }
  },

  persistInputDraft: (draftKey, draft, immediate = false, claimRevision) => {
    let queue = draftPersistQueues.get(draftKey);
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
      draftPersistQueues.set(draftKey, queue);
    }
    const enqueueVersion = queue.nextVersion + 1;
    queue.nextVersion = enqueueVersion;
    if (claimRevision !== undefined) {
      // 发送快照必须保留精确 revision，不能被正在排队的 R+1 新输入合并。
      // 更旧的普通 pending 可由该快照覆盖；快照之后的新输入会另存于 pending。
      queue.claimed = {
        draft: cloneSessionInputDraft(draft),
        version: enqueueVersion,
        revision: claimRevision,
      };
      queue.pending = null;
    } else {
      queue.pending = cloneSessionInputDraft(draft);
      queue.pendingVersion = enqueueVersion;
    }
    if (immediate) queue.immediateThroughVersion = enqueueVersion;
    queue.debounceVersion += 1;
    const debounceVersion = queue.debounceVersion;
    const completion = new Promise<SessionInputDraft>((resolve, reject) => {
      queue!.waiters.push({ version: enqueueVersion, resolve, reject });
    });

    if (!queue.running) {
      if (immediate) {
        void get().flushInputDraftQueue(draftKey);
      } else {
        void waitForDraftDebounce().then(() => {
          const latest = draftPersistQueues.get(draftKey);
          if (
            latest === queue
            && !latest.running
            && latest.debounceVersion === debounceVersion
          ) {
            void get().flushInputDraftQueue(draftKey);
          }
        });
      }
    }
    return completion;
  },

  flushInputDraftQueue: async (draftKey) => {
    const queue = draftPersistQueues.get(draftKey);
    if (!queue || queue.running || (!queue.claimed && !queue.pending)) return;
    const claimed = queue.claimed;
    const submitted = claimed?.draft ?? queue.pending!;
    const submittedVersion = claimed?.version ?? queue.pendingVersion;
    const submittedClaimRevision = claimed?.revision;
    if (claimed) {
      queue.claimed = null;
    } else {
      queue.pending = null;
    }
    queue.running = true;
    const requestVersion = (draftPersistRequestVersions.get(draftKey) ?? 0) + 1;
    draftPersistRequestVersions.set(draftKey, requestVersion);
    try {
      const persisted = await api.setInputDraft(
        draftKey,
        submitted,
        submittedClaimRevision ?? undefined,
      );
      if (
        draftPersistQueues.get(draftKey) !== queue
        || draftPersistRequestVersions.get(draftKey) !== requestVersion
      ) {
        return;
      }
      if (!queue.pending) {
        set((state) => ({
          inputDrafts: mergePersistedDraft(
            state.inputDrafts,
            draftKey,
            submitted.revision,
            persisted,
          ),
        }));
      } else {
        queue.pending = rebasePendingDraftOnPersisted(queue.pending, submitted, persisted);
      }
      const completed = queue.waiters.filter((waiter) => waiter.version <= submittedVersion);
      queue.waiters = queue.waiters.filter((waiter) => waiter.version > submittedVersion);
      for (const waiter of completed) waiter.resolve(cloneSessionInputDraft(persisted));
    } catch (error) {
      console.error('保存会话草稿失败:', error);
      const failed = queue.waiters.filter((waiter) => waiter.version <= submittedVersion);
      queue.waiters = queue.waiters.filter((waiter) => waiter.version > submittedVersion);
      for (const waiter of failed) waiter.reject(error);
    } finally {
      queue.running = false;
      if (
        draftPersistQueues.get(draftKey) === queue
        && (queue.claimed || queue.pending)
      ) {
        const nextVersion = queue.claimed?.version ?? queue.pendingVersion;
        if (queue.claimed || nextVersion <= queue.immediateThroughVersion) {
          void get().flushInputDraftQueue(draftKey);
        } else {
          queue.debounceVersion += 1;
          const debounceVersion = queue.debounceVersion;
          void waitForDraftDebounce().then(() => {
            const latest = draftPersistQueues.get(draftKey);
            if (
              latest === queue
              && !latest.running
              && latest.debounceVersion === debounceVersion
            ) {
              void get().flushInputDraftQueue(draftKey);
            }
          });
        }
      }
    }
  },

  // 普通发送：目标草稿、内容、附件与 revision 均由调用方在异步操作前固定。
  sendMessage: async (draftKey, content, attachments, revision, trustMode) => {
    let targetSessionId = draftKey;
    let deliveryAttachments = attachments.map((attachment) => ({ ...attachment }));
    const draftCwd = get().sessionCwd;
    const draftReasoningEffort = get().reasoningEffort;
    const navigationVersion = switchRequestVersion;
    let sendingDraft: SessionInputDraft | undefined;
    set((state) => {
      const inputDrafts = setDraftSending(state.inputDrafts, draftKey, true);
      sendingDraft = inputDrafts[draftKey];
      return { inputDrafts };
    });

    try {
      if (sendingDraft) {
        const persisted = await get().persistInputDraft(
          draftKey,
          sendingDraft,
          true,
          revision,
        );
        if (persisted.revision !== revision) {
          throw new Error('草稿已在发送前发生变化，请重试');
        }
        deliveryAttachments = persisted.attachments.map((attachment) => ({ ...attachment }));
      }
      if (get().draftSessionId === draftKey) {
        const creation = await api.createSessionForDraft(
          draftCwd || get().workspaceDir,
          trustMode || 'supervised',
          draftReasoningEffort,
        );
        const session = creation.session;
        const persistedMigratedDraft = await api.migrateInputDraft(draftKey, session.id);
        await Promise.all([
          api.terminalAttachSession(draftKey, session.id)
            .catch((error) => console.error('草稿终端 PTY 转正迁移失败:', error)),
          api.browserAttachSession(draftKey, session.id)
            .catch((error) => console.error('草稿浏览器 state 转正迁移失败:', error)),
        ]);
        const shouldActivate = switchRequestVersion === navigationVersion
          && get().isDraft
          && get().activeSessionId === null
          && get().draftSessionId === draftKey;
        const activated = shouldActivate
          ? await api.activateDraftSession(
              session.id,
              creation.activation_epoch,
              creation.previous_active_session_id,
            )
          : false;
        let migratedDraft: SessionInputDraft | undefined;
        set((state) => {
          const isViewingDraft = activated
            && switchRequestVersion === navigationVersion
            && state.isDraft
            && state.activeSessionId === null
            && state.draftSessionId === draftKey;
          const inputDrafts = migrateDraftKey(state.inputDrafts, draftKey, session.id);
          if (
            !inputDrafts[session.id]
            || persistedMigratedDraft.revision >= inputDrafts[session.id].revision
          ) {
            inputDrafts[session.id] = {
              ...cloneSessionInputDraft(persistedMigratedDraft),
              is_sending: inputDrafts[session.id]?.is_sending ?? true,
            };
          }
          migratedDraft = inputDrafts[session.id];
          return {
            sessions: state.sessions.some((item) => item.id === session.id)
              ? state.sessions
              : [session, ...state.sessions],
            inputDrafts,
            activeSessionId: isViewingDraft ? session.id : state.activeSessionId,
            isDraft: isViewingDraft ? false : state.isDraft,
            draftSessionId: isViewingDraft ? null : state.draftSessionId,
            draftTerminalId: isViewingDraft ? null : state.draftTerminalId,
            workspaceTabsTransfer: isViewingDraft
              ? {
                  fromSessionId: draftKey,
                  toSessionId: session.id,
                  version: (state.workspaceTabsTransfer?.version ?? 0) + 1,
                }
              : state.workspaceTabsTransfer,
          };
        });
        discardDraftPersistQueue(draftKey);
        targetSessionId = session.id;
        if (migratedDraft) {
          persistDraftInBackground(get().persistInputDraft(session.id, migratedDraft));
        }
      }

      await api.sendMessage(targetSessionId, content, deliveryAttachments, revision);
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = settleDraftSend(
          state.inputDrafts,
          targetSessionId,
          revision,
          true,
        );
        settledDraft = inputDrafts[targetSessionId];
        const isCurrent = state.activeSessionId === targetSessionId;
        return {
          inputDrafts,
          runStatus: isCurrent ? 'executing' : state.runStatus,
          sessionRunStatuses: {
            ...state.sessionRunStatuses,
            [targetSessionId]: 'executing',
          },
        };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(targetSessionId, settledDraft));
      }
      return true;
    } catch (error) {
      console.error('发送消息失败:', error);
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = settleDraftSend(
          state.inputDrafts,
          targetSessionId,
          revision,
          false,
        );
        settledDraft = inputDrafts[targetSessionId];
        return { inputDrafts };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(targetSessionId, settledDraft));
      }
      return false;
    }
  },

  appendMessage: async (sessionId, content, attachments, revision) => {
    let deliveryAttachments = attachments.map((attachment) => ({ ...attachment }));
    let sendingDraft: SessionInputDraft | undefined;
    set((state) => {
      const inputDrafts = setDraftSending(state.inputDrafts, sessionId, true);
      sendingDraft = inputDrafts[sessionId];
      return { inputDrafts };
    });
    try {
      if (sendingDraft) {
        const persisted = await get().persistInputDraft(
          sessionId,
          sendingDraft,
          true,
          revision,
        );
        if (persisted.revision !== revision) {
          throw new Error('草稿已在追加前发生变化，请重试');
        }
        deliveryAttachments = persisted.attachments.map((attachment) => ({ ...attachment }));
      }
      const appended = await api.appendMessage(
        sessionId,
        content,
        deliveryAttachments,
        revision,
      );
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = settleDraftSend(
          state.inputDrafts,
          sessionId,
          revision,
          appended,
        );
        settledDraft = inputDrafts[sessionId];
        return { inputDrafts };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(sessionId, settledDraft));
      }
      return appended;
    } catch (error) {
      console.error('追加消息失败:', error);
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = settleDraftSend(state.inputDrafts, sessionId, revision, false);
        settledDraft = inputDrafts[sessionId];
        return { inputDrafts };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(sessionId, settledDraft));
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
    let sendingDraft: SessionInputDraft | undefined;
    set((state) => {
      const inputDrafts = setDraftSending(state.inputDrafts, sessionId, true);
      sendingDraft = inputDrafts[sessionId];
      return { inputDrafts };
    });
    try {
      if (sendingDraft) {
        await get().persistInputDraft(sessionId, sendingDraft, true);
      }
      await api.editAndResend(
        sessionId,
        messageId,
        newContent,
        attachments,
        revision,
        baseContent,
      );
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = setDraftSending(state.inputDrafts, sessionId, false);
        settledDraft = inputDrafts[sessionId];
        const isCurrent = state.activeSessionId === sessionId;
        return {
          inputDrafts,
          runStatus: isCurrent ? 'executing' : state.runStatus,
          sessionRunStatuses: { ...state.sessionRunStatuses, [sessionId]: 'executing' },
        };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(sessionId, settledDraft));
      }
      return true;
    } catch (error) {
      console.error('编辑重发失败:', error);
      let settledDraft: SessionInputDraft | undefined;
      set((state) => {
        const inputDrafts = setDraftSending(state.inputDrafts, sessionId, false);
        settledDraft = inputDrafts[sessionId];
        return { inputDrafts };
      });
      if (settledDraft) {
        persistDraftInBackground(get().persistInputDraft(sessionId, settledDraft));
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
        const draftKey = selectCurrentDraftKey(get());
        let settledDraft: SessionInputDraft | undefined;
        set((state) => {
          const inputDrafts = draftKey
            ? setDraftSending(state.inputDrafts, draftKey, false)
            : state.inputDrafts;
          settledDraft = draftKey ? inputDrafts[draftKey] : undefined;
          return {
            runStatus: 'idle',
            inputDrafts,
            messages: snapshot.messages,
            currentPlan: snapshot.current_plan,
            lastUsage: snapshot.last_usage ?? null,
            tokenStats: snapshot.token_stats ?? null,
          };
        });
        if (draftKey && settledDraft) {
          persistDraftInBackground(get().persistInputDraft(draftKey, settledDraft));
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

  setDraftText: (draftKey: string, content: string) => {
    let nextDraft: SessionInputDraft | undefined;
    set((state) => {
      const inputDrafts = updateDraftText(state.inputDrafts, draftKey, content);
      nextDraft = inputDrafts[draftKey];
      return { inputDrafts };
    });
    if (nextDraft) persistDraftInBackground(get().persistInputDraft(draftKey, nextDraft));
  },

  setDraftAttachments: (draftKey: string, attachments: RawAttachment[]) => {
    let nextDraft: SessionInputDraft | undefined;
    set((state) => {
      const inputDrafts = updateDraftAttachments(state.inputDrafts, draftKey, attachments);
      nextDraft = inputDrafts[draftKey];
      return { inputDrafts };
    });
    if (nextDraft) persistDraftInBackground(get().persistInputDraft(draftKey, nextDraft));
  },

  // 设置工作目录
  setSessionCwd: async (cwd: string) => {
    try {
      // 草稿模式下只在前端保存，创建会话时再同步到后端
      const { isDraft, activeSessionId } = get();
      if (!isDraft && activeSessionId) {
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
    const draftKey = selectCurrentDraftKey(state);
    const currentDraft = selectCurrentInputDraft(state);
    const nextInput = applyAgentMention(currentDraft.text, tab, state.agents);
    set({ selectedAgentTab: tab });
    if (draftKey) get().setDraftText(draftKey, nextInput);
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
    const { activeSessionId, isDraft, sessionRunStatuses: prevStatuses } = get();
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
    const nextStatus = isDraft
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

    // 草稿模式或不是当前查看的会话 → 不更新消息/流式内容
    if (isDraft || (snapshot.last_session_id && snapshot.last_session_id !== activeSessionId)) {
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
