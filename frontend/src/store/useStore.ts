import { create } from 'zustand';
import { api, Session, Message, RunSnapshot, McpServer, Skill, TaskPlan } from '../api/tauri';

interface AppState {
  // 状态
  sessions: Session[];
  activeSessionId: string | null;
  messages: Message[];
  runStatus: string;
  currentPlan: TaskPlan | undefined;
  inputContent: string;
  mcpServers: McpServer[] | null;
  skills: Skill[] | null;

  // 流式消息状态
  streamingMessageId: string | null;
  streamingContent: string;
  streamingReasoningContent: string; // 流式思考过程内容

  // 加载状态
  isLoadingSessions: boolean;
  isSending: boolean;

  // 操作
  loadSessions: () => Promise<void>;
  createSession: () => Promise<void>;
  switchSession: (id: string) => Promise<void>;
  deleteSession: () => Promise<void>;

  sendMessage: (content: string) => Promise<void>;
  cancelTurn: () => Promise<boolean>;

  setInputContent: (content: string) => void;

  loadMcpServers: () => Promise<void>;
  loadSkills: () => Promise<void>;

  // 内部方法
  updateFromSnapshot: (snapshot: RunSnapshot) => void;
}

export const useStore = create<AppState>((set, get) => ({
  // 初始状态
  sessions: [],
  activeSessionId: null as string | null,
  messages: [],
  runStatus: 'idle',
  currentPlan: undefined,
  inputContent: '',
  mcpServers: null,
  skills: null,
  streamingMessageId: null,
  streamingContent: '',
  streamingReasoningContent: '',
  isLoadingSessions: false,
  isSending: false,

  // 加载会话列表
  loadSessions: async () => {
    set({ isLoadingSessions: true });
    try {
      const sessions = await api.getSessions();
      set({ sessions, isLoadingSessions: false });

      // 如果有会话但没有活动会话，设置第一个为活动会话
      const { activeSessionId } = get();
      if (!activeSessionId && sessions.length > 0) {
        const firstSession = sessions[0];
        set({ activeSessionId: firstSession.id });
      }
    } catch (error) {
      console.error('加载会话失败:', error);
      set({ isLoadingSessions: false });
    }
  },

  // 创建新会话
  createSession: async () => {
    try {
      const session = await api.createSession();
      set(state => ({
        sessions: [session, ...state.sessions],
        activeSessionId: session.id,
        messages: [],
        inputContent: '',
        runStatus: 'idle',
        currentPlan: undefined,
      }));
    } catch (error) {
      console.error('创建会话失败:', error);
    }
  },

  // 切换会话
  switchSession: async (id: string) => {
    try {
      await api.switchSession(id);
      const snapshot = await api.getRunSnapshot();
      set({
        activeSessionId: id,
        messages: snapshot.messages,
        inputContent: snapshot.input_draft,
        runStatus: snapshot.status,
        currentPlan: snapshot.current_plan,
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
        activeSessionId: snapshot.messages.length > 0 ? snapshot.messages[0].id : null,
        messages: snapshot.messages,
        inputContent: snapshot.input_draft,
        runStatus: snapshot.status,
      });
    } catch (error) {
      console.error('删除会话失败:', error);
    }
  },

  // 发送消息
  sendMessage: async (content: string) => {
    const { runStatus } = get();
    if (runStatus !== 'idle') {
      console.warn('当前有任务正在执行，请等待完成或取消');
      return;
    }

    set({ inputContent: '', isSending: true });

    try {
      await api.sendMessage(content);
      set({ runStatus: 'planning' });
    } catch (error) {
      console.error('发送消息失败:', error);
      set({ inputContent: content, isSending: false });
    }
  },

  // 取消当前执行
  cancelTurn: async () => {
    try {
      const cancelled = await api.cancelTurn();
      if (cancelled) {
        set({ runStatus: 'idle', isSending: false });
      }
      return cancelled;
    } catch (error) {
      console.error('取消执行失败:', error);
      return false;
    }
  },

  // 设置输入内容
  setInputContent: (content: string) => {
    set({ inputContent: content });
    // 同时同步到后端
    api.setInputDraft(content).catch(console.error);
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

  // 从快照更新状态
  updateFromSnapshot: (snapshot: RunSnapshot) => {
    const { messages: oldMessages, streamingMessageId: oldStreamingId } = get();
    const newMessages = snapshot.messages;

    // 检测最后一条消息是否是新的或内容在更新
    let streamingId: string | null = null;
    let streamingContent = '';
    let streamingReasoningContent = '';

    if (newMessages.length > 0) {
      const lastMessage = newMessages[newMessages.length - 1];

      // 只对助手消息进行流式处理
      if (lastMessage.role === 'Assistant') {
        // 检查是否是新的最后一条消息
        const isNewLastMessage = oldMessages.length === 0 ||
          oldMessages[oldMessages.length - 1].id !== lastMessage.id;

        // 检查是否是同一条消息但内容在增长
        const isGrowing = oldMessages.length > 0 &&
          oldMessages[oldMessages.length - 1].id === lastMessage.id &&
          oldMessages[oldMessages.length - 1].content !== lastMessage.content &&
          lastMessage.content.length > oldMessages[oldMessages.length - 1].content.length;

        if (isNewLastMessage || isGrowing) {
          streamingId = lastMessage.id;
          streamingContent = lastMessage.content;
          // 同时获取思考过程
          streamingReasoningContent = lastMessage.reasoning_content || '';
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
      runStatus: snapshot.status,
      currentPlan: snapshot.current_plan,
      isSending: snapshot.status !== 'idle',
      streamingMessageId: streamingId,
      streamingContent,
      streamingReasoningContent,
    });
  },
}));
