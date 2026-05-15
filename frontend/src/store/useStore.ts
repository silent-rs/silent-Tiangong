import { create } from 'zustand';
import { api, Session, Message, RunSnapshot, McpServer, Skill, TaskPlan, MediaAsset, TokenStats } from '../api/tauri';
import { notifyBackgroundSessionCompleted } from '../utils/desktopNotification';

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
    // [Agent] {label} ({role}) 已加入团队
    const createMatch = msg.content.match(/^\[Agent\] (.+?) \((.+?)\) 已加入团队.*?id=([^\s]+)/);
    if (createMatch) {
      const [, label, role, agentId] = createMatch;
      agents.set(role, { agentId, role, label, status: 'idle' });
      continue;
    }
    // [Agent] {label} 状态变更: {status}
    const statusMatch = msg.content.match(/^\[Agent\] (.+?) 状态变更: (\w+).*?id=([^\s]+)/);
    if (statusMatch) {
      const [, label, status, agentId] = statusMatch;
      if (status === 'terminated') {
        for (const [role, info] of agents) {
          if (info.agentId === agentId || info.label === label) {
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
          if (info.agentId === agentId || info.label === label) {
            info.status = status;
            break;
          }
        }
      }
    }
  }
  return Array.from(agents.values());
}

function sameJsonValue(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (left == null || right == null) return left == right;
  return JSON.stringify(left) === JSON.stringify(right);
}

function sameMessage(left: Message, right: Message): boolean {
  return left.id === right.id
    && left.role === right.role
    && left.content === right.content
    && left.reasoning_content === right.reasoning_content
    && left.worker_id === right.worker_id
    && left.tool_call_id === right.tool_call_id
    && left.tool_name === right.tool_name
    && left.tool_result_is_error === right.tool_result_is_error
    && left.compact === right.compact
    && left.created_at === right.created_at
    && sameJsonValue(left.media, right.media)
    && sameJsonValue(left.tool_calls, right.tool_calls);
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

interface AppState {
  // 状态
  sessions: Session[];
  activeSessionId: string | null;
  messages: Message[];
  runStatus: string;
  runSummary: string;
  lastDurationMs: number | null;
  lastUsage: { prompt_tokens: number; completion_tokens: number; total_tokens: number } | null;
  tokenStats: TokenStats | null;
  approvalRequestId: string | null;
  currentPlan: TaskPlan | undefined;
  inputContent: string;
  mcpServers: McpServer[] | null;
  skills: Skill[] | null;

  // 草稿模式
  isDraft: boolean;

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
  isSending: boolean;

  // 操作
  loadSessions: () => Promise<void>;
  createSession: () => void;
  switchSession: (id: string) => Promise<void>;
  deleteSession: () => Promise<void>;

  sendMessage: (content: string, media?: MediaAsset[]) => Promise<void>;
  editAndResend: (messageId: string, newContent: string) => Promise<void>;
  cancelTurn: () => Promise<boolean>;
  cancelAgent: (role: string) => Promise<boolean>;

  setInputContent: (content: string) => void;

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

export const useStore = create<AppState>((set, get) => ({
  // 初始状态
  sessions: [],
  activeSessionId: null as string | null,
  messages: [],
  runStatus: 'idle',
  runSummary: '',
  lastDurationMs: null,
  lastUsage: null,
  tokenStats: null,
  approvalRequestId: null,
  currentPlan: undefined,
  inputContent: '',
  mcpServers: null,
  skills: null,
  isDraft: false,
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
  isSending: false,
  agents: [],
  selectedAgentTab: null,

  // 加载会话列表
  loadSessions: async () => {
    set({ isLoadingSessions: true });
    try {
      const sessions = await api.getSessions();
      set({ sessions, isLoadingSessions: false });

      // 首次加载时默认进入新对话（草稿模式）
      const { activeSessionId, isDraft } = get();
      if (!activeSessionId && !isDraft) {
        set({ isDraft: true, sessionCwd: get().workspaceDir });
      }
    } catch (error) {
      console.error('加载会话失败:', error);
      set({ isLoadingSessions: false });
    }
  },

  // 创建新会话 — 纯前端草稿模式，不立即调后端
  createSession: () => {
    const workspaceDir = get().workspaceDir;
    set({
      isDraft: true,
      activeSessionId: null,
      messages: [],
      inputContent: '',
      runStatus: 'idle',
      runSummary: '',
      lastUsage: null,
      tokenStats: null,
      currentPlan: undefined,
      streamingMessageId: null,
      streamingContent: '',
      streamingReasoningContent: '',
      sessionCwd: workspaceDir,
      agents: [],
      selectedAgentTab: null,
    });
  },

  // 切换会话
  switchSession: async (id: string) => {
    try {
      await api.switchSession(id);
      const [snapshot, cwd] = await Promise.all([
        api.getRunSnapshot(),
        api.getSessionCwd(),
      ]);
      set({
        isDraft: false,
        activeSessionId: id,
        messages: snapshot.messages,
        inputContent: snapshot.input_draft,
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
      });
    } catch (error) {
      console.error('切换会话失败:', error);
    }
  },

  // 删除当前会话
  deleteSession: async () => {
    try {
      await api.deleteSession();
      const sessions = await api.getSessions();
      const snapshot = await api.getRunSnapshot();

      set({
        sessions,
        isDraft: false,
        activeSessionId: snapshot.messages.length > 0 ? snapshot.messages[0].id : null,
        messages: snapshot.messages,
        inputContent: snapshot.input_draft,
        runStatus: snapshot.status,
        runSummary: snapshot.summary || '',
        lastDurationMs: snapshot.last_duration_ms ?? null,
        lastUsage: snapshot.last_usage ?? null,
        tokenStats: snapshot.token_stats ?? null,
        approvalRequestId: snapshot.approval_request_id || null,
        agents: parseAgentsFromMessages(snapshot.messages),
        selectedAgentTab: null,
      });
    } catch (error) {
      console.error('删除会话失败:', error);
    }
  },

  // 发送消息
  sendMessage: async (content: string, media: MediaAsset[] = []) => {
    const { isDraft } = get();

    // 草稿模式时先创建后端会话
    if (isDraft) {
      try {
        const session = await api.createSession();
        // 把草稿模式下设置的 cwd 同步到新创建的会话
        const draftCwd = get().sessionCwd;
        if (draftCwd) {
          await api.setSessionCwd(draftCwd).catch(console.error);
        }
        set(state => ({
          sessions: [session, ...state.sessions],
          activeSessionId: session.id,
          isDraft: false,
        }));
      } catch (error) {
        console.error('创建会话失败:', error);
        return;
      }
    }

    const selectedTab = get().selectedAgentTab;
    const nextDraft = selectedTab ? `@${selectedTab} ` : '';
    set({ inputContent: nextDraft, isSending: true });

    try {
      if (media.length > 0) {
        await api.sendMessageWithMedia(content, media);
      } else {
        await api.sendMessage(content);
      }
      // 消息已提交到后端，释放发送锁，允许用户切换对话等操作
      const runningSessionId = get().activeSessionId;
      set(state => ({
        runStatus: 'executing',
        isSending: false,
        inputContent: nextDraft,
        sessionRunStatuses: runningSessionId
          ? { ...state.sessionRunStatuses, [runningSessionId]: 'executing' }
          : state.sessionRunStatuses,
      }));
      if (nextDraft) {
        api.setInputDraft(nextDraft).catch(console.error);
      }
    } catch (error) {
      console.error('发送消息失败:', error);
      set({ inputContent: content, isSending: false });
    }
  },

  // 编辑用户消息并从该节点重新发送
  editAndResend: async (messageId: string, newContent: string) => {
    set({ isSending: true });
    try {
      await api.editAndResend(messageId, newContent);
      const runningSessionId = get().activeSessionId;
      set(state => ({
        runStatus: 'executing',
        isSending: false,
        inputContent: '',
        sessionRunStatuses: runningSessionId
          ? { ...state.sessionRunStatuses, [runningSessionId]: 'executing' }
          : state.sessionRunStatuses,
      }));
    } catch (error) {
      console.error('编辑重发失败:', error);
      set({ isSending: false });
    }
  },

  // 取消当前执行
  cancelTurn: async () => {
    try {
      const cancelled = await api.cancelTurn();
      if (cancelled) {
        // 立即获取最新快照确保状态一致（避免轮询线程的旧快照覆盖）
        const snapshot = await api.getRunSnapshot();
        set({
          runStatus: 'idle',
          isSending: false,
          messages: snapshot.messages,
          currentPlan: snapshot.current_plan,
          lastUsage: snapshot.last_usage ?? null,
          tokenStats: snapshot.token_stats ?? null,
        });
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

  // 设置输入内容
  setInputContent: (content: string) => {
    set({ inputContent: content });
    // 草稿模式下不同步到后端
    if (!get().isDraft) {
      api.setInputDraft(content).catch(console.error);
    }
  },

  // 设置工作目录
  setSessionCwd: async (cwd: string) => {
    try {
      // 草稿模式下只在前端保存，创建会话时再同步到后端
      if (!get().isDraft) {
        await api.setSessionCwd(cwd);
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
    const { inputContent, agents, isDraft } = get();
    const nextInput = applyAgentMention(inputContent, tab, agents);
    set({ selectedAgentTab: tab, inputContent: nextInput });
    if (!isDraft) {
      api.setInputDraft(nextInput).catch(console.error);
    }
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

    // 始终同步当前活动会话的 runStatus（基于 snapshot.status）
    // snapshot.status 已由后端 build_session_snapshot 按 session 修正
    const { runStatus: prevStatus, isSending: prevSending } = get();
    const snapshotStatus = snapshot.status;
    const snapshotApprovalRequestId = (snapshot as any).approval_request_id
      || (snapshot as any).approvalRequestId
      || null;
    const isContextManagementSnapshot = (snapshot.summary || '').includes('上下文')
      || (snapshot.summary || '').includes('正在压缩');
    // 防止取消后被旧快照覆盖
    const effectiveStatus = (
      prevStatus === 'idle'
      && !prevSending
      && snapshotStatus !== 'idle'
      && snapshotStatus !== 'waiting_approval'
      && !snapshotApprovalRequestId
      && !isContextManagementSnapshot
    )
      ? 'idle'
      : snapshotStatus;
    set({
      runStatus: effectiveStatus,
      runSummary: snapshot.summary || '',
      lastDurationMs: snapshot.last_duration_ms ?? null,
      lastUsage: snapshot.last_usage ?? null,
      tokenStats: snapshot.token_stats ?? null,
      approvalRequestId: snapshotApprovalRequestId,
    });

    // 草稿模式或不是当前查看的会话 → 不更新消息/流式内容
    if (isDraft || (snapshot.last_session_id && snapshot.last_session_id !== activeSessionId)) {
      return;
    }

    const { messages: oldMessages, streamingMessageId: oldStreamingId } = get();
    const newMessages = mergeSnapshotMessages(oldMessages, snapshot.messages);

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
        const isContentGrowing = oldAssistant &&
          oldAssistant.content !== lastAssistant.content &&
          lastAssistant.content.length > oldAssistant.content.length;
        const isReasoningGrowing = oldAssistant &&
          oldAssistant.reasoning_content !== lastAssistant.reasoning_content &&
          lastAssistant.reasoning_content.length > oldAssistant.reasoning_content.length;

        if ((isNew || isContentGrowing || isReasoningGrowing) && !hasRenderableMedia) {
          streamingId = lastAssistant.id;
          streamingContent = lastAssistant.content;
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
      agents: parseAgentsFromMessages(newMessages),
    });

    // 状态变为 idle 时刷新会话列表（更新 message_count、标题等）
    if (effectiveStatus === 'idle' && prevStatus !== 'idle') {
      api.getSessions().then((sessions) => {
        set({ sessions });
      }).catch(console.error);
    }
  },
}));
