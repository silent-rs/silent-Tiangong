import { useStore } from "@/store/useStore";
import { useSearchStore } from "@/store/useSearchStore";
import { findSearchMatches } from "@/utils/search";
import { SearchBar } from "./SearchBar";
import { ScrollArea } from "./ui/scroll-area";
import {
  Loader2,
  Cpu,
  ArrowUp,
  ArrowDown,
  ArrowDownToLine,
} from "lucide-react";
import 'md-editor-rt/lib/preview.css';
import { open } from '@tauri-apps/plugin-dialog';
import { AgentPanel } from "./AgentPanel";
import { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider } from "@/components/ui/tooltip";
import { api, hasMediaBlocks, textContent, type ContentBlock } from "@/api/tauri";
import {
  type Attachment,
  attachmentKindFromMime,
  imageExtFromMime,
  fileToDataUrl,
  attachmentFromPath,
  attachmentsFromContentBlocks,
  estimatedBase64Size,
} from '@/utils/attachments';
import { parseScheduledTaskMessage } from '@/utils/scheduledTaskMessage';
import { parseWebhookMessage } from '@/utils/webhookMessage';
import { useVirtualizer } from "@tanstack/react-virtual";

import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import {
  groupMessages,
  workerContentMessages,
  workerBelongsToAgent,
  extractAgentRoles,
  UserMessageGroup,
  AgentTurn,
} from "./message";
import { type MentionEditorHandle } from "./MentionEditor";

/** 取路径最后 1-2 级目录用于简短展示，例如 /a/b/tiangong -> b/tiangong */
function shortDir(path: string): string {
  if (!path) return '';
  const trimmed = path.replace(/[\\/]+$/, '');
  const parts = trimmed.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return parts.join('/');
  return parts.slice(-2).join('/');
}

export function MessageList() {
  const messages = useStore(s => s.messages);
  const workspaceDir = useStore(s => s.workspaceDir);
  const sessionCwd = useStore(s => s.sessionCwd);
  const runStatus = useStore(s => s.runStatus);
  const runSummary = useStore(s => s.runSummary);
  const streamingMessageId = useStore(s => s.streamingMessageId);
  const streamingContent = useStore(s => s.streamingContent);
  const streamingReasoningContent = useStore(s => s.streamingReasoningContent);
  const selectedAgentTab = useStore(s => s.selectedAgentTab);
  const agents = useStore(s => s.agents);
  const voiceMessages = useStore(s => s.voiceMessages);
  const editAndResend = useStore(s => s.editAndResend);
  const activeSessionId = useStore(s => s.activeSessionId);

  const searchActive = useSearchStore(s => s.searchActive);

  // 用 ref 持有搜索 query 和 matchIndex，避免每次按键导致 MessageList 重渲染
  const searchQueryRef = useRef(useSearchStore.getState().searchQuery);
  const currentMatchIndexRef = useRef(useSearchStore.getState().currentMatchIndex);
  useEffect(() => {
    const unsub = useSearchStore.subscribe((s) => {
      searchQueryRef.current = s.searchQuery;
      currentMatchIndexRef.current = s.currentMatchIndex;
    });
    return unsub;
  }, []);

  const scrollRef = useRef<HTMLDivElement>(null);
  const viewportRef = useRef<HTMLDivElement>(null);
  const prevMessagesLengthRef = useRef(0);
  const prevStreamingIdRef = useRef<string | null>(null);
  const prevSelectedAgentTabRef = useRef<string | null>(null);
  const [hasTts, setHasTts] = useState(false);
  const [editingMessageId, setEditingMessageId] = useState<string | null>(null);
  const [editingSessionId, setEditingSessionId] = useState<string | null>(null);
  const [editingContent, setEditingContent] = useState("");
  const [editingAttachments, setEditingAttachments] = useState<Attachment[]>([]);
  const editingRevisionRef = useRef(0);
  const editingGenerationRef = useRef(0);
  const editingBaseContentRef = useRef<ContentBlock[]>([]);
  const editingTextareaRef = useRef<MentionEditorHandle>(null!);
  const [isAtBottom, setIsAtBottom] = useState(true);
  const isAtBottomRef = useRef(true);
  // 百分比轨道：鼠标 Y 比例映射到的用户提问序号，-1 表示未在轨道内
  const [hoverUserPos, setHoverUserPos] = useState(-1);
  const hoverUserPosRef = useRef(-1);
  // 正态点显隐：鼠标进入轨道时显示，离开 1.5s 后隐藏
  const [railHovered, setRailHovered] = useState(false);
  const railHideTimerRef = useRef<number | null>(null);
  // 鼠标在轨道内的 Y 比例（0~1），用于点列块平移跟随；-1 表示未在轨道内
  const hoverRatioRef = useRef(-1);
  // 点列块的 top 像素值（已 clamp + 吸附），避免滑到顶/底时块超出轨道被裁
  const [dotsTopPx, setDotsTopPx] = useState<number | null>(null);
  // 点列块与轨道主体，用于计算平移量
  const railTrackRef = useRef<HTMLDivElement>(null);
  const railDotsRef = useRef<HTMLDivElement>(null);

  // 检查 TTS 能力
  useEffect(() => {
    api.hasTtsCapability().then(setHasTts).catch(() => setHasTts(false));
  }, []);

  // 切换会话时关闭搜索
  useEffect(() => {
    useSearchStore.getState().closeSearch();
    // 切换会话视为重新进入，默认在底部
    isAtBottomRef.current = true;
    setIsAtBottom(true);
  }, [activeSessionId]);

  // 卸载时清理正态点隐藏定时器
  useEffect(() => () => {
    if (railHideTimerRef.current) window.clearTimeout(railHideTimerRef.current);
  }, []);

  // 根据鼠标 Y 比例计算点列块 top（像素），含 clamp 与端点吸附，避免滑到顶/底时块被裁
  const calcDotsTop = useCallback((ratio: number): number => {
    const track = railTrackRef.current;
    const dots = railDotsRef.current;
    if (!track || !dots) return 0;
    const trackH = track.clientHeight;
    const dotsH = dots.offsetHeight;
    // 块可活动范围：[0, 轨道高 - 块高]；块高大于轨道高时退化为 0
    const maxTop = Math.max(0, trackH - dotsH);
    // 期望的块顶边 = 目标中心 Y - 块高/2，clamp 到可活动范围防止溢出被裁
    // 不做吸附：直接跟随鼠标到端点贴住，避免"咬"的吸附感
    const top = Math.max(0, Math.min(maxTop, ratio * trackH - dotsH / 2));
    return top;
  }, []);

  // 监听滚动位置，维护 isAtBottom 状态
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const handleScroll = () => {
      const threshold = 80;
      const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
      const next = distance < threshold;
      if (next !== isAtBottomRef.current) {
        isAtBottomRef.current = next;
        setIsAtBottom(next);
      }
    };
    el.addEventListener('scroll', handleScroll, { passive: true });
    return () => el.removeEventListener('scroll', handleScroll);
  }, [activeSessionId]);

  // Cmd/Ctrl+F 全局快捷键
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'f') {
        e.preventDefault();
        e.stopPropagation();
        const store = useSearchStore.getState();
        if (!store.searchActive) {
          store.openSearch();
        } else {
          const input = document.querySelector<HTMLInputElement>('[data-search-input]');
          if (input) input.focus();
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown, true);
    return () => window.removeEventListener('keydown', handleKeyDown, true);
  }, []);

  const isThinking = runStatus !== "idle";
  const isContextCompressing = runSummary.includes("正在压缩");

  // 消息分组
  const messageGroups = useMemo(() => groupMessages(messages), [messages]);
  const selectedAgentId = selectedAgentTab
    ? agents.find((agent) => agent.role === selectedAgentTab)?.agentId
    : undefined;

  // agent_tab 过滤前置：将渲染时的 return null 改为数据层过滤
  const filteredGroups = useMemo(() => {
    if (!selectedAgentTab) {
      // 主对话视图：排除子 Agent 的过程消息（worker_id 以 "agent:" 开头），
      // 这些只在对应的 Agent Tab 中展示。
      return messageGroups.filter(group => {
        if (group.type === "worker") {
          return !group.worker_id?.startsWith("agent:");
        }
        return true;
      });
    }
    return messageGroups.filter(group => {
      if (group.type === "user") return false;
      if (group.type === "worker") {
        return workerBelongsToAgent(
          group.worker_id,
          selectedAgentTab,
          selectedAgentId,
        );
      }
      if (group.type === "agent_turn") {
        return group.messages.some(m =>
          m.role === "system"
          && extractAgentRoles(textContent(m), agents).includes(selectedAgentTab)
        );
      }
      return true;
    });
  }, [messageGroups, selectedAgentTab, selectedAgentId, agents]);

  // 分离流式消息：正在流式输出的 agent_turn 不参与虚拟化
  const { completedGroups, streamingGroup } = useMemo(() => {
    if (!streamingMessageId || filteredGroups.length === 0) {
      return { completedGroups: filteredGroups, streamingGroup: null };
    }
    // 检查最后一个 group 是否包含流式消息
    const lastGroup = filteredGroups[filteredGroups.length - 1];
    if (lastGroup.type === "agent_turn" && lastGroup.messages.some(m => m.id === streamingMessageId)) {
      return {
        completedGroups: filteredGroups.slice(0, -1),
        streamingGroup: lastGroup,
      };
    }
    return { completedGroups: filteredGroups, streamingGroup: null };
  }, [filteredGroups, streamingMessageId]);

  // 提取用户消息分组的索引列表，用于"滚动到上一条/下一条用户提问"
  const userGroupIndices = useMemo(() => {
    const indices: number[] = [];
    completedGroups.forEach((g, i) => {
      if (g.type === 'user') indices.push(i);
    });
    return indices;
  }, [completedGroups]);

  // 每轮 agent_turn 的终态（总时长 + 最终状态）：取该轮用户消息（前置最近
  // user 组）的 elapsed_ms/turn_status；回复底部与轮次末尾状态行据此展示。
  const turnResultByGroupKey = useMemo(() => {
    const map = new Map<string, { elapsedMs?: number; status?: string }>();
    let current: { elapsedMs?: number; status?: string } | undefined;
    for (const group of filteredGroups) {
      if (group.type === 'user') {
        const msg = group.messages[0];
        current = { elapsedMs: msg?.elapsed_ms ?? undefined, status: msg?.turn_status ?? undefined };
      } else if (group.type === 'agent_turn') {
        map.set(group.key, current ?? {});
      }
    }
    return map;
  }, [filteredGroups]);

  // 流式轮的终态：本轮最后一条用户消息的 elapsed_ms/turn_status。
  const streamingTurnResult = useMemo(() => {
    if (!streamingGroup) return undefined;
    for (let i = messages.length - 1; i >= 0; i -= 1) {
      if (messages[i].role === 'user') {
        return {
          elapsedMs: messages[i].elapsed_ms ?? undefined,
          status: messages[i].turn_status ?? undefined,
        };
      }
    }
    return undefined;
  }, [streamingGroup, messages]);

  // 计算 compact 边界
  const nonEditableIds = useMemo(() => {
    const ids = new Set<string>();
    for (const msg of messages) {
      if (msg.compact) {
        ids.add(msg.id);
        break;
      }
    }
    if (ids.size > 0) {
      const compactId = [...ids][0];
      for (const msg of messages) {
        ids.add(msg.id);
        if (msg.id === compactId) break;
      }
    }
    return ids;
  }, [messages]);

  // 虚拟化
  const virtualizer = useVirtualizer({
    count: completedGroups.length,
    getScrollElement: () => viewportRef.current,
    estimateSize: (index) => {
      const group = completedGroups[index];
      if (group.type === "user") {
        const msg = group.messages[0];
        const hasMedia = msg.media?.length || hasMediaBlocks(msg);
        const scheduledTask = parseScheduledTaskMessage(textContent(msg));
        const webhook = parseWebhookMessage(textContent(msg));
        const structured = scheduledTask ?? webhook;
        if (structured) {
          const textLength = structured.name.length
            + structured.description.length
            + structured.payload.length;
          const explicitLines = structured.payload.split(/\r?\n/).length;
          const contentHeight = Math.min(
            Math.max(explicitLines, Math.ceil(textLength / 48)) * 20,
            360,
          );
          return 140 + contentHeight + (hasMedia ? 220 : 0);
        }
        return hasMedia ? 300 : 80;
      }
      if (group.type === "worker") return 120;
      // agent_turn: 根据消息数量、工具调用和内容长度启发式估算
      const msgCount = group.messages.length;
      const hasTools = group.messages.some(m =>
        m.role === "system" && (textContent(m).includes("tool_name:") || textContent(m).includes("exit_code"))
      );
      const totalTextLen = group.messages.reduce((sum, m) => sum + textContent(m).length, 0);
      const textBonus = Math.min(Math.floor(totalTextLen / 200) * 30, 400);
      return (hasTools ? 200 + msgCount * 30 : 100 + msgCount * 40) + textBonus;
    },
    overscan: 5,
  });

  // 搜索导航：通过 store subscription 监听 searchQuery 和 currentMatchIndex 变化
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const filteredGroupsRef = useRef(filteredGroups);
  filteredGroupsRef.current = filteredGroups;
  useEffect(() => {
    if (!searchActive) return;
    let prevIndex = useSearchStore.getState().currentMatchIndex;
    let prevQuery = useSearchStore.getState().searchQuery;
    let prevScope = useSearchStore.getState().searchScope;
    const scrollToMatch = (query: string, index: number, scope: string) => {
      if (!query) return;
      const groups = filteredGroupsRef.current;
      const cs = useSearchStore.getState().caseSensitive;
      const matches = findSearchMatches(messagesRef.current, query, groups, scope as 'messages' | 'withThinking' | 'all', cs);
      if (matches.length === 0 || index >= matches.length) return;
      const match = matches[index];
      const targetIndex = groups.findIndex(g =>
        g.messages.some(m => m.id === match.messageId),
      );
      if (targetIndex >= 0) {
        virtualizer.scrollToIndex(targetIndex, { behavior: 'smooth', align: 'center' });
        requestAnimationFrame(() => {
          requestAnimationFrame(() => {
            const el = document.querySelector('.search-highlight-current');
            if (el) el.scrollIntoView({ block: 'center', behavior: 'smooth' });
          });
        });
      }
    };
    const unsub = useSearchStore.subscribe((s) => {
      const queryChanged = s.searchQuery !== prevQuery;
      const indexChanged = s.currentMatchIndex !== prevIndex;
      const scopeChanged = s.searchScope !== prevScope;
      if (!queryChanged && !indexChanged && !scopeChanged) return;
      prevIndex = s.currentMatchIndex;
      prevQuery = s.searchQuery;
      prevScope = s.searchScope;
      scrollToMatch(s.searchQuery, s.currentMatchIndex, s.searchScope);
    });
    // 首次激活时也滚动到第一个匹配
    const { searchQuery, currentMatchIndex, searchScope } = useSearchStore.getState();
    scrollToMatch(searchQuery, currentMatchIndex, searchScope);
    return unsub;
  }, [searchActive, virtualizer]);

  // 新消息到达时滚动到底部
  useEffect(() => {
    const tabChanged = selectedAgentTab !== prevSelectedAgentTabRef.current;
    const newMessageArrived = messages.length > prevMessagesLengthRef.current;
    const streamingIdChanged = streamingMessageId !== prevStreamingIdRef.current;
    const lastMsg = messages[messages.length - 1];
    const isUserSelfSent = newMessageArrived && lastMsg?.role === 'user';
    // 用户发起对话（发送或编辑重发）时重新进入自动跟随模式；
    // 用户翻动页面离开底部后跟随自动关闭，拉回最底部时由滚动事件重新开启
    if (isUserSelfSent) {
      isAtBottomRef.current = true;
      setIsAtBottom(true);
    }
    // 用户离开底部时，新消息/流式 id 变化不强制拉回；tab 切换与用户主动发送始终跟随
    const shouldScroll =
      tabChanged
      || isUserSelfSent
      || ((newMessageArrived || streamingIdChanged) && isAtBottomRef.current);

    if (shouldScroll) {
      if (completedGroups.length > 0 && !streamingGroup) {
        // 滚动到虚拟列表最后一项
        requestAnimationFrame(() => {
          // 跟随滚动用瞬时定位：平滑动画期间内容持续增长会让滚动事件
          // 误判"离开底部"而中断跟随
          virtualizer.scrollToIndex(completedGroups.length - 1, {
            behavior: "auto",
            align: "end",
          });
        });
      } else if (streamingGroup) {
        requestAnimationFrame(() => {
          scrollRef.current?.scrollIntoView({ behavior: "auto", block: "end" });
        });
      }
    }

    prevMessagesLengthRef.current = messages.length;
    prevStreamingIdRef.current = streamingMessageId;
    prevSelectedAgentTabRef.current = selectedAgentTab;
  }, [messages.length, streamingMessageId, completedGroups.length, streamingGroup, selectedAgentTab]);

  // 流式输出时自动滚动
  useEffect(() => {
    if (!streamingMessageId) return;
    const el = scrollRef.current;
    if (!el) return;

    let ticking = false;
    const observer = new MutationObserver(() => {
      if (ticking) return;
      ticking = true;
      requestAnimationFrame(() => {
        if (isAtBottomRef.current) {
          // 瞬时贴底：流式内容增长期间保持视口贴底，滚动事件判定不受
          // 平滑动画与内容增长的竞态影响；用户翻离底部后自动停止跟随
          el.scrollIntoView({ behavior: "auto", block: "end" });
        }
        ticking = false;
      });
    });
    observer.observe(el.parentElement!, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    return () => observer.disconnect();
  }, [streamingMessageId]);

  // 滚动到底部：瞬时贴底并重新进入跟随模式。平滑动画期间内容增长
  // 会让落点偏离底部导致跟随开不上来
  const scrollToBottom = useCallback(() => {
    const el = viewportRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: 'auto' });
    isAtBottomRef.current = true;
    setIsAtBottom(true);
  }, []);

  // 滚动到指定 group 并对齐到视口顶部：两阶段定位避免虚拟列表估算高度导致的偏差
  const scrollToUserGroupTop = useCallback((targetIndex: number) => {
    virtualizer.scrollToIndex(targetIndex, { align: 'start' });
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const el = viewportRef.current?.querySelector(`[data-index="${targetIndex}"]`);
        if (el) {
          el.scrollIntoView({ block: 'start', behavior: 'smooth' });
        }
      });
    });
    isAtBottomRef.current = false;
    setIsAtBottom(false);
  }, [virtualizer]);

  // 根据参考 index 找到当前"游标"在 userGroupIndices 中的位置
  // 返回 userGroupIndices 中 <= refIndex 的最大位置；都没有则返回 -1
  const findUserCursorPos = useCallback((refIndex: number): number => {
    let pos = -1;
    for (let i = 0; i < userGroupIndices.length; i++) {
      if (userGroupIndices[i] <= refIndex) pos = i;
      else break;
    }
    return pos;
  }, [userGroupIndices]);

  // 获取当前激活的用户提问序号（视口顶部对齐到的那一条）
  // 与轨道点的 activeUserPos 同源，作为上下切换的统一基准
  const getActiveUserPos = useCallback((): number => {
    const item = virtualizer.getVirtualItemForOffset(virtualizer.scrollOffset ?? 0);
    if (!item) return -1;
    return findUserCursorPos(item.index);
  }, [virtualizer, findUserCursorPos]);

  // 检查指定 group 是否在视口内可见（顶部落在视口范围内）
  const isGroupInView = useCallback((groupIndex: number): boolean => {
    const viewport = viewportRef.current;
    if (!viewport) return false;
    const el = viewport.querySelector(`[data-index="${groupIndex}"]`);
    if (!el) return false;
    const viewportRect = viewport.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    return elRect.top >= viewportRect.top && elRect.top < viewportRect.bottom;
  }, []);

  // 滚动到上一条用户提问：
  // 若当前游标对应的用户提问不在视口内（已被滚出顶部），先将其带入视口；
  // 已在视口内时才跳到上一条
  const scrollToPrevUserMessage = useCallback(() => {
    if (userGroupIndices.length === 0) return;
    const cursorPos = getActiveUserPos();
    if (cursorPos < 0) {
      scrollToUserGroupTop(userGroupIndices[0]);
      return;
    }
    // 当前游标提问不在视口内 → 先跳到本条
    const cursorGroupIndex = userGroupIndices[cursorPos];
    if (!isGroupInView(cursorGroupIndex)) {
      scrollToUserGroupTop(cursorGroupIndex);
      return;
    }
    // 已在视口内 → 跳到上一条
    const targetPos = cursorPos <= 0 ? 0 : cursorPos - 1;
    scrollToUserGroupTop(userGroupIndices[targetPos]);
  }, [userGroupIndices, getActiveUserPos, scrollToUserGroupTop, isGroupInView]);

  // 滚动到当前可见区域之下的最近一条用户提问
  const scrollToNextUserMessage = useCallback(() => {
    if (userGroupIndices.length === 0) return;
    const lastPos = userGroupIndices.length - 1;
    const cursorPos = getActiveUserPos();
    // 视口顶部在所有用户提问之前（cursorPos === -1）时跳到第一条
    // 已在最后一条时停在最后一条；否则向后一个
    const targetPos = cursorPos === -1 ? 0 : cursorPos >= lastPos ? lastPos : cursorPos + 1;
    scrollToUserGroupTop(userGroupIndices[targetPos]);
  }, [userGroupIndices, getActiveUserPos, scrollToUserGroupTop]);

  // 编辑相关回调
  const handleStartEdit = useCallback((messageId: string, text: string) => {
    if (runStatus !== "idle" || !activeSessionId) return;
    const msg = messages.find(m => m.id === messageId);
    if (msg && (parseScheduledTaskMessage(textContent(msg)) || parseWebhookMessage(textContent(msg)))) return;
    setEditingMessageId(messageId);
    setEditingSessionId(activeSessionId);
    setEditingContent(text);
    editingRevisionRef.current = 0;
    editingGenerationRef.current += 1;
    if (msg) {
      editingBaseContentRef.current = msg.content.map((block) => structuredClone(block));
      const mediaAttachments = attachmentsFromContentBlocks(
        Array.isArray(msg.content) ? msg.content : [],
      );
      setEditingAttachments(mediaAttachments);
    } else {
      editingBaseContentRef.current = [];
      setEditingAttachments([]);
    }
  }, [activeSessionId, runStatus, messages]);

  const handleConfirmEdit = useCallback(async () => {
    if (!editingSessionId || !editingMessageId || !editingContent.trim()) return;
    const targetSessionId = editingSessionId;
    const messageId = editingMessageId;
    const content = editingContent.trim();
    const attachments = editingAttachments.map((attachment) => ({ ...attachment }));
    const revision = editingRevisionRef.current;
    // editingBaseContentRef 在进入编辑时已深拷贝脱离原消息；invoke 参数序列化是同步的，
    // 且 ref 在 await 完成后才会被清空，故无需再次深拷贝（避免对含图消息复制 base64）。
    const baseContent = [...editingBaseContentRef.current];
    const generation = editingGenerationRef.current;
    const succeeded = await editAndResend(
      targetSessionId,
      messageId,
      content,
      attachments,
      revision,
      baseContent,
    );
    if (
      !succeeded
      || editingGenerationRef.current !== generation
      || editingRevisionRef.current !== revision
    ) return;
    setEditingMessageId(null);
    setEditingSessionId(null);
    setEditingContent("");
    setEditingAttachments([]);
    editingRevisionRef.current = 0;
    editingBaseContentRef.current = [];
  }, [
    editingSessionId,
    editingMessageId,
    editingContent,
    editingAttachments,
    editAndResend,
  ]);

  const handleCancelEdit = useCallback(() => {
    editingGenerationRef.current += 1;
    editingRevisionRef.current = 0;
    editingBaseContentRef.current = [];
    setEditingMessageId(null);
    setEditingSessionId(null);
    setEditingContent("");
    setEditingAttachments([]);
  }, []);

  const handleSetEditingContent = useCallback((content: string) => {
    setEditingContent(content);
    editingRevisionRef.current += 1;
  }, []);

  const handleSetEditingAttachments = useCallback((update: React.SetStateAction<Attachment[]>) => {
    setEditingAttachments((current) => (
      typeof update === 'function' ? update(current) : update
    ));
    editingRevisionRef.current += 1;
  }, []);

  const handleAttachFilesForEdit = useCallback(async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: '选择图片或文件',
        filters: [
          {
            name: '图片、音视频和文件',
            extensions: [
              'png', 'jpg', 'jpeg', 'webp', 'gif',
              'mp3', 'wav', 'm4a', 'ogg', 'flac',
              'mp4', 'mov', 'webm', 'mkv',
              'pdf', 'docx', 'xlsx', 'pptx', 'txt', 'md', 'json', 'csv',
            ],
          },
        ],
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (paths.length === 0) return;
      const newAttachments = paths.map(attachmentFromPath);
      handleSetEditingAttachments(prev => {
        const next = [...prev];
        for (const item of newAttachments) {
          if (!next.some(existing => existing.source === item.source)) {
            next.push(item);
          }
        }
        return next;
      });
    } catch (e) {
      console.error('选择附件失败:', e);
    }
  }, [handleSetEditingAttachments]);

  const handleEditPaste = useCallback(async (e: React.ClipboardEvent<HTMLDivElement>) => {
    const files = Array.from(e.clipboardData.files);
    if (files.length === 0) return;
    e.preventDefault();
    try {
      const pasted = await Promise.all(files.map(async (file, index) => {
        const mimeType = file.type || 'application/octet-stream';
        const title = file.name || (mimeType.startsWith('image/')
          ? `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`
          : `pasted-file-${Date.now()}-${index + 1}`);
        if (estimatedBase64Size(file.size) > 50 * 1024 * 1024) {
          throw new Error(`附件"${title}"超过 50MB，已停止添加。`);
        }
        return {
          kind: attachmentKindFromMime(mimeType),
          source: await fileToDataUrl(file),
          original_name: title,
          mime_type: mimeType,
        };
      }));
      handleSetEditingAttachments(prev => [...prev, ...pasted]);
    } catch (err) {
      console.error('读取粘贴图片失败:', err);
      alert(err instanceof Error ? err.message : '读取粘贴图片失败');
    }
  }, [handleSetEditingAttachments]);

  // 拦截 MdPreview 内的链接点击：左键在嵌入浏览器中打开，右键保留系统默认
  useEffect(() => {
    const handler = (e: MouseEvent) => {
      if (e.button !== 0) return; // 仅拦截左键
      const anchor = (e.target as HTMLElement).closest('a[href]');
      if (!anchor) return;
      const href = anchor.getAttribute('href');
      if (!href || href.startsWith('#') || href.startsWith('javascript:')) return;
      e.preventDefault();
      // 通过自定义事件通知 MainApp 打开浏览器
      window.dispatchEvent(new CustomEvent('tiangong:open-browser', { detail: href }));
    };
    document.addEventListener('click', handler);
    return () => document.removeEventListener('click', handler);
  }, []);

  // 当前视口顶部对应的用户提问序号，用于导航轨道点激活
  // 滚动时 virtualizer 触发 rerender，render 期即可读到最新 scrollOffset
  const activeUserPos = (() => {
    if (userGroupIndices.length === 0) return -1;
    const item = virtualizer.getVirtualItemForOffset(virtualizer.scrollOffset ?? 0);
    if (!item) return -1;
    return findUserCursorPos(item.index);
  })();

  // ---- 百分比磁吸轨道的派生数据 ----
  const userCount = userGroupIndices.length;
  // 每条用户提问的预览文本（前 15 字符，超出补 ...）
  const userPreviews = useMemo(
    () => userGroupIndices.map(idx => {
      const raw = textContent(completedGroups[idx].messages[0]);
      const scheduledTask = parseScheduledTaskMessage(raw);
      const webhook = parseWebhookMessage(raw);
      const preview = scheduledTask
        ? `定时：${scheduledTask.name || '未命名任务'}`
        : webhook
          ? `Webhook：${webhook.name || '未命名触发'}`
          : raw;
      return (preview.length > 15 ? preview.slice(0, 15) + '...' : preview) || '(空消息)';
    }),
    [userGroupIndices, completedGroups],
  );

  // ≤9 条直接平铺；>9 条按鼠标 Y 比例（无 hover 时回退到激活序号）做高斯窗口
  const railSpread = userCount > 9;
  // 游标：鼠标 hover 时跟随鼠标比例，否则回退到当前激活序号
  const railCursor = hoverUserPos >= 0 ? hoverUserPos : Math.max(0, activeUserPos);
  // 正态点仅在鼠标位于轨道内时显示；>9 条且未悬停时只渲染细条背景
  const showRailDots = railHovered;
  // 高斯窗口：σ 越小点收缩越快；此处用 σ=2 让 9 个点呈现明显的大小渐变
  const railPoints = useMemo<{ groupIndex: number; pos: number; size: number }[]>(() => {
    if (!railSpread) {
      // 平铺模式：所有点等大（size=1），均可视为候选最大点
      return userGroupIndices.map((groupIndex, pos) => ({ groupIndex, pos, size: 1 }));
    }
    const sigma = 2;
    const windowSize = 9;
    const half = Math.floor(windowSize / 2);
    // 以游标为中心取 [-half, +half] 范围，裁剪到 [0, userCount-1]
    const points: { groupIndex: number; pos: number; size: number }[] = [];
    for (let off = -half; off <= half; off++) {
      const pos = railCursor + off;
      if (pos < 0 || pos >= userCount) continue;
      // 高斯权重映射到点尺寸（0.5 ~ 1.0），中心 1.0，边缘 ~0.5
      const w = Math.exp(-(off * off) / (2 * sigma * sigma));
      const size = 0.5 + 0.5 * w;
      points.push({ groupIndex: userGroupIndices[pos], pos, size });
    }
    return points;
  }, [railSpread, userGroupIndices, railCursor, userCount]);

  return (
    <div className="relative h-full">
    <ScrollArea className="h-full" viewportRef={viewportRef} viewportClassName="[scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
      <div className="p-4">
        <div className="max-w-3xl mx-auto space-y-2">
          {messages.length === 0 && !isThinking ? (
            <div className="flex flex-col items-center justify-center h-full text-center py-20">
              <div className="w-16 h-16 rounded-full bg-primary flex items-center justify-center mb-4">
                <Cpu className="w-8 h-8 text-primary-foreground" />
              </div>
              <h2 className="text-xl font-medium text-foreground mb-2">
                欢迎使用天工
              </h2>
              <p
                className="text-muted-foreground text-sm"
                title={(sessionCwd || workspaceDir) || undefined}
              >
                我可以帮助您在{' '}
                <span className="text-foreground font-medium font-mono">
                  {shortDir(sessionCwd || workspaceDir) || '当前工作区'}
                </span>
                {' '}中完成各种任务
              </p>
            </div>
          ) : (
            <>
              {agents.length > 0 && (
                <div className="sticky top-0 z-10 bg-background/95 py-1 backdrop-blur">
                  <AgentPanel />
                </div>
              )}

              {searchActive && <SearchBar />}

              {/* 虚拟化渲染已完成消息 */}
              <div
                style={{
                  height: virtualizer.getTotalSize(),
                  width: "100%",
                  position: "relative",
                }}
              >
                {virtualizer.getVirtualItems().map((virtualItem) => {
                  const group = completedGroups[virtualItem.index];
                  if (group.type === "user") {
                    return (
                      <div
                        key={virtualItem.key}
                        data-index={virtualItem.index}
                        ref={virtualizer.measureElement}
                        style={{
                          position: "absolute",
                          top: 0,
                          left: 0,
                          width: "100%",
                          transform: `translateY(${virtualItem.start}px)`,
                          scrollMarginTop: '0.5rem',
                        }}
                      >
                        <UserMessageGroup
                          group={group}
                          runStatus={runStatus}
                          nonEditableIds={nonEditableIds}
                          voiceMessages={voiceMessages}
                          editingMessageId={editingMessageId}
                          editingContent={editingContent}
                          editingAttachments={editingAttachments}
                          editingTextareaRef={editingTextareaRef}
                          onStartEdit={handleStartEdit}
                          onConfirmEdit={handleConfirmEdit}
                          onCancelEdit={handleCancelEdit}
                          onSetEditingContent={handleSetEditingContent}
                          onSetEditingAttachments={handleSetEditingAttachments}
                          onAttachFiles={handleAttachFilesForEdit}
                          onEditPaste={handleEditPaste}
                        />
                      </div>
                    );
                  }

                  if (group.type === "worker") {
                    const contentMessages = workerContentMessages(group.messages);
                    return (
                      <div
                        key={virtualItem.key}
                        data-index={virtualItem.index}
                        ref={virtualizer.measureElement}
                        style={{
                          position: "absolute",
                          top: 0,
                          left: 0,
                          width: "100%",
                          transform: `translateY(${virtualItem.start}px)`,
                        }}
                      >
                        <AgentTurn
                          messages={contentMessages}
                          streamingMessageId={null}
                          streamingContent=""
                          streamingReasoningContent=""
                          hasTts={hasTts}
                          selectedAgentTab={null}
                        />
                      </div>
                    );
                  }

                  // agent_turn
                  // 末尾轮次且无单独的 streamingGroup、整轮仍在进行中时，视为当前活动轮（工具调用阶段）
                  const isLiveTurn =
                    isThinking
                    && !streamingGroup
                    && virtualItem.index === completedGroups.length - 1;
                  return (
                    <div
                      key={virtualItem.key}
                      data-index={virtualItem.index}
                      ref={(el) => {
                        if (el) virtualizer.measureElement(el);
                      }}
                      style={{
                        position: "absolute",
                        top: 0,
                        left: 0,
                        width: "100%",
                        transform: `translateY(${virtualItem.start}px)`,
                      }}
                    >
                      <AgentTurn
                        key={`turn-virtual-${virtualItem.key}-${isLiveTurn ? 'active' : 'done'}`}
                        messages={group.messages}
                        streamingMessageId={null}
                        streamingContent=""
                        streamingReasoningContent=""
                        hasTts={hasTts}
                        selectedAgentTab={selectedAgentTab}
                        isActive={isLiveTurn}
                        turnElapsedMs={turnResultByGroupKey.get(group.key)?.elapsedMs}
                        turnStatus={turnResultByGroupKey.get(group.key)?.status}
                      />
                    </div>
                  );
                })}
              </div>

              {/* 流式消息区域（始终渲染，不参与虚拟化） */}
              {streamingGroup && (
                <div className="mt-3">
                  <AgentTurn
                    key={`turn-streaming-${isThinking ? 'active' : 'done'}`}
                    messages={streamingGroup.messages}
                    streamingMessageId={streamingMessageId}
                    streamingContent={streamingContent}
                    streamingReasoningContent={streamingReasoningContent}
                    hasTts={hasTts}
                    selectedAgentTab={selectedAgentTab}
                    isActive={isThinking}
                    turnElapsedMs={streamingTurnResult?.elapsedMs}
                    turnStatus={streamingTurnResult?.status}
                  />
                </div>
              )}

              {/* 无流式但有思考中 */}
              {!streamingGroup && isThinking && (
                isContextCompressing ||
                (!streamingMessageId && !streamingContent &&
                  !(messages.length > 0 && messages[messages.length - 1].role === "assistant"))
              ) && (
                <div className="flex justify-start mt-3">
                  <div className="text-foreground">
                    <div className="flex items-center gap-2">
                      <Loader2 className="w-4 h-4 animate-spin" />
                      <span className="text-sm text-muted-foreground">
                        {runSummary || (
                          <>
                            {runStatus === "planning" && "正在制定计划..."}
                            {runStatus === "executing" && "正在执行任务..."}
                            {runStatus === "responding" && "正在生成回复..."}
                          </>
                        )}
                      </span>
                    </div>
                  </div>
                </div>
              )}
            </>
          )}

          {/* 滚动锚点 */}
          <div ref={scrollRef} />
        </div>
      </div>
    </ScrollArea>

    {/* 右侧百分比磁吸轨道：离开底部时，鼠标移入右侧区域才显示。替代原生滚动条，贴右边缘。
        ≤9 条平铺等间距；>9 条按游标（hover/激活）做高斯窗口，渲染离游标最近的 9 个点，
        中心点最大并向两侧正态衰减；每点显示预览，点 hover 可点选跳转，
        点击轨道空白按百分比跳转；背景条带滚动位置 thumb 指示当前视图。
        外层为 group + 透明感应条（w-6）捕获 hover；内部交互层用 group-hover 控制显隐，
        避免感应区过大遮挡消息内容。 */}
    {userCount > 0 && (
      <div
        className={`group absolute inset-y-0 right-0 z-20 flex flex-col items-end transition-opacity duration-200 ${
          isAtBottom ? 'opacity-0' : 'opacity-100'
        }`}
      >
        {/* 透明 hover 感应条：贴右边缘（24px 宽），仅捕获鼠标进出触发 group-hover。
            不影响消息内容交互（消息内容不延伸到最右 24px 内的概率极低）。 */}
        <div className="absolute inset-y-0 right-0 w-6" />
        {/* 轨道主体：百分比磁吸。点列块整体跟随鼠标 Y 平移（中心点贴鼠标），
            内容随游标变化 —— 跟随、可点选、百分比三者统一 */}
        <div
          ref={railTrackRef}
          className={`pointer-events-none relative flex min-h-0 flex-1 flex-col items-end py-1 transition-opacity duration-150 opacity-0 group-hover:pointer-events-auto group-hover:opacity-100 ${
            railHovered ? 'pointer-events-auto !opacity-100' : ''
          }`}
          onMouseMove={(e) => {
            const rect = e.currentTarget.getBoundingClientRect();
            const ratio = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
            // 更新点列块平移比例
            if (ratio !== hoverRatioRef.current) {
              hoverRatioRef.current = ratio;
              // 计算 clamp + 吸附后的 top 像素，避免滑到顶/底时块超出轨道被裁
              setDotsTopPx(calcDotsTop(ratio));
            }
            // 鼠标 Y 比例映射到用户提问序号
            const pos = Math.round(ratio * (userCount - 1));
            if (pos >= 0 && pos < userCount && pos !== hoverUserPosRef.current) {
              hoverUserPosRef.current = pos;
              setHoverUserPos(pos);
            }
          }}
          onMouseEnter={() => {
            if (railHideTimerRef.current) {
              window.clearTimeout(railHideTimerRef.current);
              railHideTimerRef.current = null;
            }
            setRailHovered(true);
          }}
          onMouseLeave={() => {
            // 冻结在鼠标移出时的最后位置：不重置 hoverRatio/hoverUserPos，
            // 保证鼠标移到正态点上点选时仍是正确的消息位置；
            // 仅启动 1.5s 隐藏定时器
            if (railHideTimerRef.current) window.clearTimeout(railHideTimerRef.current);
            railHideTimerRef.current = window.setTimeout(() => {
              setRailHovered(false);
              railHideTimerRef.current = null;
            }, 1500);
          }}
        >
          {/* 轨道背景条：贴右边缘，替代原生滚动条 */}
          <div className="absolute inset-y-2 right-1 w-[15px] rounded-full bg-muted-foreground/15" />
          <button
            type="button"
            data-rail="bg"
            aria-label="按百分比跳转到用户提问"
            onClick={(e) => {
              const rect = e.currentTarget.getBoundingClientRect();
              const ratio = Math.min(1, Math.max(0, (e.clientY - rect.top) / rect.height));
              const pos = Math.round(ratio * (userCount - 1));
              if (pos >= 0 && pos < userCount) {
                scrollToUserGroupTop(userGroupIndices[pos]);
              }
            }}
            className="absolute inset-y-2 right-1 w-[15px] cursor-pointer"
          />
          {(railSpread ? showRailDots : true) && (
          <TooltipProvider delayDuration={200}>
            {/* 点列块：absolute 定位，top 按鼠标 Y 计算（clamp 防溢出），
                内容随游标变化。right-7 让点列位于 15px 滑轨左侧并留出间距 */}
            <div
              ref={railDotsRef}
              className="absolute right-7 flex flex-col items-end gap-1.5"
              onMouseMove={(e) => e.stopPropagation()}
              style={{
                // 顶边定位（已 clamp + 吸附），避免 translateY(-50%) 在端点溢出被裁；
                // dotsTopPx 为 null（首次未移动）时居中兜底
                top: dotsTopPx != null ? `${dotsTopPx}px` : '50%',
                transform: dotsTopPx != null ? 'none' : 'translateY(-50%)',
                transition: 'top 0.18s cubic-bezier(0.22, 0.61, 0.36, 1), opacity 0.15s',
              }}
            >
              {railPoints.map((p, slotIdx) => {
                const preview = userPreviews[p.pos];
                const active = p.pos === activeUserPos;
                // 每个点都显示消息预览：按高斯权重调整字号与透明度，中心点最醒目
                const opacity = 0.45 + p.size * 0.55;
                const fontSize = 9 + Math.round(p.size * 3); // 9~12px
                return (
                  <div key={slotIdx} className="flex items-center gap-1.5">
                    <span
                      className="max-w-[140px] truncate text-muted-foreground"
                      style={{
                        opacity,
                        fontSize: `${fontSize}px`,
                        // 文本透明度与字号变化平滑过渡，与点动画同步
                        transition: 'opacity 0.2s ease-out, font-size 0.2s ease-out',
                      }}
                    >
                      {preview}
                    </span>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <button
                          type="button"
                          onClick={() => scrollToUserGroupTop(p.groupIndex)}
                          aria-label={`跳转到用户提问：${preview}`}
                          style={{
                            // 固定基础尺寸 12px（中心点最大），用 transform: scale 按高斯权重
                            // 缩放（0.5~1.0）。transform 走 GPU 合成层，过渡比 width/height 更流畅
                            width: 12,
                            height: 12,
                            transform: `scale(${p.size})`,
                            transition: 'transform 0.2s cubic-bezier(0.34, 1.56, 0.64, 1), background-color 0.15s ease-out',
                          }}
                          className={`rounded-full ${
                            active
                              ? 'bg-primary'
                              : 'bg-muted-foreground/50 hover:bg-muted-foreground'
                          }`}
                        />
                      </TooltipTrigger>
                      <TooltipContent side="left" className="max-w-[200px] text-xs break-all">
                        {preview}
                      </TooltipContent>
                    </Tooltip>
                  </div>
                );
              })}
            </div>
          </TooltipProvider>
          )}
        </div>

        {/* 滚动按钮组：独立钉在右下角，不透明背景遮挡溢出 */}
        <div className="pointer-events-auto relative z-30 mt-2 flex flex-col items-center gap-2 rounded-lg bg-background/80 p-1 shadow-md backdrop-blur">
          <button
            type="button"
            onClick={scrollToPrevUserMessage}
            className="flex h-9 w-9 items-center justify-center rounded-full border border-border/60 bg-background/80 text-foreground transition-colors hover:bg-accent"
            title="滚动到上一条用户提问"
            aria-label="滚动到上一条用户提问"
          >
            <ArrowUp className="h-4 w-4" />
          </button>
          <button
            type="button"
            onClick={scrollToNextUserMessage}
            className="flex h-9 w-9 items-center justify-center rounded-full border border-border/60 bg-background/80 text-foreground transition-colors hover:bg-accent"
            title="滚动到下一条用户提问"
            aria-label="滚动到下一条用户提问"
          >
            <ArrowDown className="h-4 w-4" />
          </button>
          <button
            type="button"
            onClick={scrollToBottom}
            className="flex h-9 w-9 items-center justify-center rounded-full border border-border/60 bg-background/80 text-foreground transition-colors hover:bg-accent"
            title="滚动到底部"
            aria-label="滚动到底部"
          >
            <ArrowDownToLine className="h-4 w-4" />
          </button>
        </div>
      </div>
    )}
    </div>
  );
}
