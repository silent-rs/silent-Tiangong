import { useStore } from "@/store/useStore";
import { useSearchStore } from "@/store/useSearchStore";
import { findSearchMatches, findTextOccurrences } from "@/utils/search";
import { HighlightText } from "./HighlightText";
import { SearchBar } from "./SearchBar";
import { ScrollArea } from "./ui/scroll-area";
import { Textarea } from "./ui/textarea";
import {
  Loader2,
  ChevronRight,
  ChevronDown,
  Terminal,
  Cpu,
  FileText,
  Volume2,
  Square,
  Copy,
  Check,
  Play,
  ChevronUp,
  ShieldCheck,
  ShieldX,
  Brain,
  Pencil,
  X,
  Paperclip,
  ArrowUp,
  ArrowDown,
  ArrowDownToLine,
} from "lucide-react";
import { MdPreview } from 'md-editor-rt';
import 'md-editor-rt/lib/preview.css';
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from '@tauri-apps/plugin-dialog';
import { ThinkingBlock } from "./ThinkingBlock";
import { AgentPanel } from "./AgentPanel";
import { Tooltip, TooltipTrigger, TooltipContent, TooltipProvider } from "@/components/ui/tooltip";
import { api, textContent, type ContentBlock } from "@/api/tauri";
import {
  type Attachment,
  imageExtFromMime,
  fileToDataUrl,
  attachmentFromPath,
  estimatedBase64Size,
  resolveAttachmentUrl,
  attachmentsToBase64Media,
} from '@/utils/attachments';
import { useVirtualizer } from "@tanstack/react-virtual";

import { memo, useEffect, useMemo, useRef, useState, useCallback } from "react";
import { useResolvedTheme } from "@/hooks/useTheme";

/** 格式化消息时间（hover 显示） */
function formatMessageTime(createdAt?: string): string {
  if (!createdAt) return "";
  try {
    const d = new Date(createdAt);
    if (isNaN(d.getTime())) return createdAt;
    const pad = (n: number) => n.toString().padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  } catch {
    return createdAt;
  }
}

function MessageActions({ text, showTts }: { text: string; showTts: boolean }) {
  const [copied, setCopied] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [ttsLoading, setTtsLoading] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      console.error("复制失败:", e);
    }
  };

  const handleTts = async () => {
    if (playing) {
      api.stopAudio().catch(() => {});
      setPlaying(false);
      return;
    }

    setTtsLoading(true);
    try {
      api.stopAudio().catch(() => {});
      const result = await api.synthesizeSpeech(text);
      setPlaying(true);
      setTtsLoading(false);
      await api.playAudioFile(result.file_path);
      setPlaying(false);
    } catch (e: any) {
      console.error("TTS 播放失败:", e);
      alert(`语音播放失败：${e?.message || e}`);
      setPlaying(false);
      setTtsLoading(false);
    }
  };

  const btnClass = "p-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors";

  return (
    <div className="flex items-center gap-0.5 mt-1">
      <button onClick={handleCopy} className={btnClass} title={copied ? "已复制" : "复制"}>
        {copied ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />}
      </button>
      {showTts && (
        <button onClick={handleTts} className={btnClass} title={playing ? "停止播放" : "朗读"}>
          {ttsLoading ? (
            <Loader2 className="w-3.5 h-3.5 animate-spin" />
          ) : playing ? (
            <Square className="w-3.5 h-3.5" />
          ) : (
            <Volume2 className="w-3.5 h-3.5" />
          )}
        </button>
      )}
    </div>
  );
}

function UserMessageActions({ text, messageId, runStatus, canEdit, onStartEdit }: {
  text: string;
  messageId: string;
  runStatus: string;
  canEdit: boolean;
  onStartEdit: (messageId: string, text: string) => void;
}) {
  const [copied, setCopied] = useState(false);
  const idle = runStatus === "idle";

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      console.error("复制失败:", e);
    }
  };

  const btnClass = "p-1 rounded text-muted-foreground/50 hover:text-muted-foreground hover:bg-accent transition-colors";

  return (
    <div className="flex items-center gap-0.5 mt-1">
      <button onClick={handleCopy} className={btnClass} title={copied ? "已复制" : "复制"}>
        {copied ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />}
      </button>
      <button
        onClick={() => onStartEdit(messageId, text)}
        className={`${btnClass} ${(!idle || !canEdit) ? 'opacity-30 cursor-not-allowed' : ''}`}
        title={!canEdit ? "已压缩消息无法编辑" : !idle ? "执行中无法编辑" : "编辑并重发"}
        disabled={!idle || !canEdit}
      >
        <Pencil className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}

function VoiceBubble({ messageId, audioPath, duration, showText, content }: {
  messageId: string;
  audioPath: string;
  duration?: number;
  showText: boolean;
  content: string;
}) {
  const [playing, setPlaying] = useState(false);
  const { toggleVoiceText } = useStore();

  const handlePlay = async () => {
    if (playing) {
      await api.stopAudio().catch(() => {});
      setPlaying(false);
      return;
    }
    setPlaying(true);
    try {
      await api.playAudioFile(audioPath);
    } catch (e) {
      console.error("播放语音失败:", e);
    }
    setPlaying(false);
  };

  return (
    <div>
      <button
        className="flex items-center gap-2 text-sm hover:opacity-80 transition-opacity"
        onClick={handlePlay}
        title={playing ? "停止播放" : "点击播放语音"}
      >
        {playing ? (
          <Square className="w-4 h-4 shrink-0" />
        ) : (
          <Play className="w-4 h-4 shrink-0 fill-current" />
        )}
        <div className="flex items-center gap-1">
          <span className="inline-block w-16 h-[3px] rounded bg-foreground/40" />
          <span className="text-xs text-muted-foreground">
            {duration ? `${Math.round(duration)}″` : '语音'}
          </span>
        </div>
      </button>
      <div className="mt-1">
        <button
          className="text-xs text-muted-foreground hover:text-foreground transition-colors"
          onClick={() => toggleVoiceText(messageId)}
        >
          {showText ? (
            <ChevronUp className="w-3 h-3 inline mr-0.5" />
          ) : (
            <ChevronDown className="w-3 h-3 inline mr-0.5" />
          )}
          {showText ? '隐藏文字' : '显示文字'}
        </button>
      </div>
      {showText && (
        <p className="whitespace-pre-wrap break-words text-sm mt-1 pt-1 border-t border-border">
          {content}
        </p>
      )}
    </div>
  );
}

function workerContentMessages(messages: MessageItem[]): MessageItem[] {
  return messages.filter(m => !(m.role === "system" && textContent(m).startsWith("🔧 Worker:")));
}

// ---------------------------------------------------------------------------
// Markdown 渲染组件
// ---------------------------------------------------------------------------
// 用户消息组渲染
// ---------------------------------------------------------------------------

function UserMessageGroup({ group, runStatus, nonEditableIds, voiceMessages, editingMessageId, editingContent, editingAttachments, editingTextareaRef, hasMultimodal, onStartEdit, onConfirmEdit, onCancelEdit, onSetEditingContent, onSetEditingAttachments, onAttachFiles, onEditPaste }: {
  group: MessageGroup;
  runStatus: string;
  nonEditableIds: Set<string>;
  voiceMessages: Record<string, { audioPath: string; duration?: number; showText: boolean }>;
  editingMessageId: string | null;
  editingContent: string;
  editingAttachments: Attachment[];
  editingTextareaRef: React.RefObject<HTMLTextAreaElement>;
  hasMultimodal: boolean;
  onStartEdit: (messageId: string, text: string) => void;
  onConfirmEdit: () => void;
  onCancelEdit: () => void;
  onSetEditingContent: (v: string) => void;
  onSetEditingAttachments: React.Dispatch<React.SetStateAction<Attachment[]>>;
  onAttachFiles: () => void;
  onEditPaste: (e: React.ClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const message = group.messages[0];
  const voiceInfo = voiceMessages[message.id];
  const isEditing = editingMessageId === message.id;
  const searchQuery = useSearchStore(s => s.searchQuery);
  const currentMessageId = useSearchStore(s => s.currentMessageId);
  const currentMatchStart = useSearchStore(s => s.currentMatchStart);
  const caseSensitive = useSearchStore(s => s.caseSensitive);

  const renderUserText = (text: string) => {
    if (!searchQuery) return text;
    const occurrences = findTextOccurrences(text, searchQuery, caseSensitive);
    if (occurrences.length === 0) return text;
    const isCurrent = message.id === currentMessageId;
    return <HighlightText text={text} matches={occurrences} currentMatchStart={isCurrent ? currentMatchStart : null} />;
  };

  return (
    <div className="mt-3 first:mt-0">
      {isEditing ? (
        <div className="w-full">
          {editingAttachments.length > 0 && (
            <div className="mb-2 flex flex-wrap gap-1.5">
              {editingAttachments.map(item => (
                <span
                  key={item.title + item.url.slice(0, 40)}
                  className="inline-flex h-9 max-w-[260px] items-center gap-1.5 rounded-md border bg-muted/40 px-2 text-xs"
                  title={item.title}
                >
                  {item.kind === 'image' ? (
                    <img
                      src={resolveAttachmentUrl(item.url)}
                      alt={item.title}
                      className="h-6 w-6 shrink-0 rounded object-cover"
                    />
                  ) : (
                    <Paperclip className="h-3 w-3 shrink-0" />
                  )}
                  <span className="truncate">{item.title}</span>
                  <button
                    type="button"
                    onClick={() => onSetEditingAttachments(prev => prev.filter(a => a.url !== item.url))}
                    className="ml-1 text-muted-foreground hover:text-foreground"
                    title="移除附件"
                  >
                    <X className="h-3 w-3" />
                  </button>
                </span>
              ))}
            </div>
          )}
          <Textarea
            ref={editingTextareaRef}
            value={editingContent}
            onChange={(e) => {
              onSetEditingContent(e.target.value);
              const textarea = editingTextareaRef.current;
              if (textarea) {
                textarea.style.height = '60px';
                textarea.style.height = Math.min(textarea.scrollHeight, 200) + 'px';
              }
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && e.keyCode !== 229) {
                e.preventDefault();
                onConfirmEdit();
              }
              if (e.key === "Escape") {
                onCancelEdit();
              }
            }}
            onPaste={onEditPaste}
            className="min-h-[60px] max-h-[200px] resize-none text-sm w-full"
            autoFocus
          />
          <div className="flex justify-between items-center mt-1">
            <span className="text-[10px] text-muted-foreground">Enter 发送 · Shift+Enter 换行 · Esc 取消</span>
            <div className="flex gap-1.5">
              {hasMultimodal && (
                <button
                  onClick={onAttachFiles}
                  className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
                  title="添加附件"
                >
                  <Paperclip className="w-3 h-3" />
                </button>
              )}
              <button
                onClick={onCancelEdit}
                className="flex items-center gap-1 px-2 py-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
              >
                <X className="w-3 h-3" />
                取消
              </button>
              <button
                onClick={onConfirmEdit}
                className="px-2.5 py-1 text-xs bg-green-600 hover:bg-green-700 text-white rounded transition-colors"
              >
                发送
              </button>
            </div>
          </div>
        </div>
      ) : (
      <div className="flex justify-end" title={formatMessageTime(message.created_at)}>
        <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-foreground">
          {voiceInfo ? (
            <VoiceBubble
              messageId={message.id}
              audioPath={voiceInfo.audioPath}
              duration={voiceInfo.duration}
              showText={voiceInfo.showText}
              content={textContent(message)}
            />
          ) : (
            <div>
              {renderContentMedia(message)}
              {textContent(message) && (
                <p className="whitespace-pre-wrap break-words text-sm">
                  {renderUserText(textContent(message))}
                </p>
              )}
            </div>
          )}
        </div>
      </div>
      )}
      {textContent(message) && !isEditing && (
        <div className="flex justify-end">
          <UserMessageActions
            text={textContent(message)}
            messageId={message.id}
            runStatus={runStatus}
            canEdit={!nonEditableIds.has(message.id)}
            onStartEdit={onStartEdit}
          />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 主组件
// ---------------------------------------------------------------------------

export function MessageList() {
  const messages = useStore(s => s.messages);
  const runStatus = useStore(s => s.runStatus);
  const runSummary = useStore(s => s.runSummary);
  const streamingMessageId = useStore(s => s.streamingMessageId);
  const streamingContent = useStore(s => s.streamingContent);
  const streamingReasoningContent = useStore(s => s.streamingReasoningContent);
  const selectedAgentTab = useStore(s => s.selectedAgentTab);
  const agents = useStore(s => s.agents);
  const voiceMessages = useStore(s => s.voiceMessages);
  const approvalRequestId = useStore(s => s.approvalRequestId);
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
  const [editingContent, setEditingContent] = useState("");
  const [editingAttachments, setEditingAttachments] = useState<Attachment[]>([]);
  const editingTextareaRef = useRef<HTMLTextAreaElement>(null!);
  const [hasMultimodal, setHasMultimodal] = useState(false);
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

  // 检查多模态能力
  useEffect(() => {
    api.hasModelCapability('multimodal').then(setHasMultimodal).catch(() => setHasMultimodal(false));
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

  // agent_tab 过滤前置：将渲染时的 return null 改为数据层过滤
  const filteredGroups = useMemo(() => {
    if (!selectedAgentTab) return messageGroups;
    return messageGroups.filter(group => {
      if (group.type === "user") return false;
      if (group.type === "worker") {
        return group.worker_id?.startsWith(`agent:${selectedAgentTab}:`);
      }
      if (group.type === "agent_turn") {
        return group.messages.some(m =>
          m.role === "system"
          && extractAgentRoles(textContent(m), agents).includes(selectedAgentTab)
        );
      }
      return true;
    });
  }, [messageGroups, selectedAgentTab, agents]);

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
        const hasMedia = msg.media?.length || msg.content.some(b => b.type === "media");
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
    // 用户离开底部时，新消息/流式 id 变化不强制拉回；tab 切换与用户主动发送始终跟随
    const shouldScroll =
      tabChanged
      || isUserSelfSent
      || ((newMessageArrived || streamingIdChanged) && isAtBottomRef.current);

    if (shouldScroll) {
      if (completedGroups.length > 0 && !streamingGroup) {
        // 滚动到虚拟列表最后一项
        requestAnimationFrame(() => {
          virtualizer.scrollToIndex(completedGroups.length - 1, {
            behavior: tabChanged ? "auto" : "smooth",
            align: "end",
          });
        });
      } else if (streamingGroup) {
        requestAnimationFrame(() => {
          scrollRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
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
          el.scrollIntoView({ behavior: "smooth", block: "end" });
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

  // 滚动到底部
  const scrollToBottom = useCallback(() => {
    const el = viewportRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
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

  // 滚动到当前可见区域之上的最近一条用户提问
  // 以"当前激活的提问序号"为基准，与轨道点击完全同源，避免基于视口中心导致的跨节点/无法到首尾
  const scrollToPrevUserMessage = useCallback(() => {
    if (userGroupIndices.length === 0) return;
    const cursorPos = getActiveUserPos();
    const targetPos = cursorPos <= 0 ? 0 : cursorPos - 1;
    scrollToUserGroupTop(userGroupIndices[targetPos]);
  }, [userGroupIndices, getActiveUserPos, scrollToUserGroupTop]);

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
    if (runStatus !== "idle") return;
    setEditingMessageId(messageId);
    setEditingContent(text);
    const msg = messages.find(m => m.id === messageId);
    if (msg && hasMultimodal) {
      const mediaAttachments: Attachment[] = (Array.isArray(msg.content) ? msg.content : [])
        .filter((b: ContentBlock) => b.type === 'media' && b.url)
        .map((b: ContentBlock) => ({
          kind: b.kind === 'image' ? 'image' : 'file',
          url: b.url!,
          title: b.title || '',
          mime_type: b.mime_type,
        }));
      setEditingAttachments(mediaAttachments);
    } else {
      setEditingAttachments([]);
    }
    setTimeout(() => {
      const textarea = editingTextareaRef.current;
      if (textarea) {
        textarea.style.height = '60px';
        textarea.style.height = Math.min(textarea.scrollHeight, 200) + 'px';
      }
    }, 0);
  }, [runStatus, messages, hasMultimodal]);

  const handleConfirmEdit = useCallback(async () => {
    if (!editingMessageId || !editingContent.trim()) return;
    let media: Awaited<ReturnType<typeof attachmentsToBase64Media>> = [];
    if (editingAttachments.length > 0 && hasMultimodal) {
      try {
        media = await attachmentsToBase64Media(editingAttachments);
      } catch (err) {
        console.error('附件转换失败:', err);
        alert(err instanceof Error ? err.message : '附件转换失败');
        return;
      }
    }
    editAndResend(editingMessageId, editingContent.trim(), media);
    setEditingMessageId(null);
    setEditingContent("");
    setEditingAttachments([]);
  }, [editingMessageId, editingContent, editingAttachments, hasMultimodal, editAndResend]);

  const handleCancelEdit = useCallback(() => {
    setEditingMessageId(null);
    setEditingContent("");
    setEditingAttachments([]);
  }, []);

  const handleAttachFilesForEdit = useCallback(async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        title: '选择图片或文件',
        filters: [
          { name: '图片和文件', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'pdf', 'txt', 'md', 'json', 'csv'] },
        ],
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (paths.length === 0) return;
      const newAttachments = paths.map(attachmentFromPath);
      setEditingAttachments(prev => {
        const next = [...prev];
        for (const item of newAttachments) {
          if (!next.some(existing => existing.url === item.url)) {
            next.push(item);
          }
        }
        return next;
      });
    } catch (e) {
      console.error('选择附件失败:', e);
    }
  }, []);

  const handleEditPaste = useCallback(async (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    if (!hasMultimodal) return;
    const files = Array.from(e.clipboardData.files).filter(file =>
      file.type.startsWith('image/')
    );
    if (files.length === 0) return;
    e.preventDefault();
    try {
      const pasted = await Promise.all(files.map(async (file, index) => {
        const mimeType = file.type || 'image/png';
        const title = file.name || `pasted-image-${Date.now()}-${index + 1}.${imageExtFromMime(mimeType)}`;
        if (estimatedBase64Size(file.size) > 50 * 1024 * 1024) {
          throw new Error(`附件"${title}"超过 50MB，已停止添加。`);
        }
        return {
          kind: 'image' as const,
          url: await fileToDataUrl(file),
          title,
          mime_type: mimeType,
        };
      }));
      setEditingAttachments(prev => [...prev, ...pasted]);
    } catch (err) {
      console.error('读取粘贴图片失败:', err);
      alert(err instanceof Error ? err.message : '读取粘贴图片失败');
    }
  }, [hasMultimodal]);

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
      return (raw.length > 15 ? raw.slice(0, 15) + '...' : raw) || '(空消息)';
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
              <p className="text-muted-foreground text-sm">
                我可以帮助您完成各种编程任务
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
                          hasMultimodal={hasMultimodal}
                          onStartEdit={handleStartEdit}
                          onConfirmEdit={handleConfirmEdit}
                          onCancelEdit={handleCancelEdit}
                          onSetEditingContent={setEditingContent}
                          onSetEditingAttachments={setEditingAttachments}
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
                        messages={group.messages}
                        streamingMessageId={null}
                        streamingContent=""
                        streamingReasoningContent=""
                        hasTts={hasTts}
                        selectedAgentTab={selectedAgentTab}
                      />
                    </div>
                  );
                })}
              </div>

              {/* 流式消息区域（始终渲染，不参与虚拟化） */}
              {streamingGroup && (
                <div className="mt-3">
                  <AgentTurn
                    messages={streamingGroup.messages}
                    streamingMessageId={streamingMessageId}
                    streamingContent={streamingContent}
                    streamingReasoningContent={streamingReasoningContent}
                    hasTts={hasTts}
                    selectedAgentTab={selectedAgentTab}
                  />
                </div>
              )}

              {/* 无流式但有思考中 */}
              {!streamingGroup && isThinking && runStatus !== "waiting_approval" && (
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

          {/* 审批请求 */}
          {runStatus === "waiting_approval" && (
            <div className="flex justify-start">
              <div className="text-foreground max-w-[100%]">
                <div className="text-sm font-medium mb-2">需要您的确认</div>
                <div className="text-xs text-muted-foreground mb-3">
                  {runSummary}
                </div>
                <div className="flex items-center gap-2">
                  <button
                    className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600 hover:bg-green-700 text-white text-xs transition-colors"
                    onClick={() => {
                      if (approvalRequestId) {
                        api.respondApproval(approvalRequestId, true).catch(console.error);
                      }
                    }}
                  >
                    <ShieldCheck className="w-3.5 h-3.5" />
                    允许
                  </button>
                  <button
                    className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-destructive hover:bg-destructive/90 text-destructive-foreground text-xs transition-colors"
                    onClick={() => {
                      if (approvalRequestId) {
                        api.respondApproval(approvalRequestId, false).catch(console.error);
                      }
                    }}
                  >
                    <ShieldX className="w-3.5 h-3.5" />
                    拒绝
                  </button>
                </div>
              </div>
            </div>
          )}

          {/* 滚动锚点 */}
          <div ref={scrollRef} />
        </div>
      </div>
    </ScrollArea>

    {/* 右侧百分比磁吸轨道：离开底部时显示。替代原生滚动条，贴右边缘。
        ≤9 条平铺等间距；>9 条按游标（hover/激活）做高斯窗口，渲染离游标最近的 9 个点，
        中心点最大并向两侧正态衰减；每点显示预览，点 hover 可点选跳转，
        点击轨道空白按百分比跳转；背景条带滚动位置 thumb 指示当前视图 */}
    {userCount > 0 && (
      <div
        className={`pointer-events-none absolute inset-y-0 right-0 z-20 flex flex-col items-end transition-all duration-200 ${
          isAtBottom
            ? 'opacity-0 translate-x-2'
            : 'opacity-100 translate-x-0'
        }`}
      >
        {/* 轨道主体：百分比磁吸。点列块整体跟随鼠标 Y 平移（中心点贴鼠标），
            内容随游标变化 —— 跟随、可点选、百分比三者统一 */}
        <div
          ref={railTrackRef}
          className="pointer-events-auto relative flex min-h-0 flex-1 flex-col items-end py-1"
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

// ---------------------------------------------------------------------------
// 消息分组：User 消息单独成组，其余合并为 agent_turn
// ---------------------------------------------------------------------------

interface MessageItem {
  id: string;
  role: "system" | "user" | "assistant" | "tool";
  content: ContentBlock[];
  reasoning_content: string;
  worker_id?: string;
  media?: {
    kind: "image" | "video" | "audio" | "file";
    url: string;
    mime_type?: string;
    title?: string;
    capability?: string;
  }[];
  tool_calls?: { id: string; name: string; arguments?: unknown }[];
  tool_call_id?: string;
  tool_name?: string;
  tool_result_is_error?: boolean;
  compact?: boolean;
  created_at: string;
}

interface MessageGroup {
  key: string;
  type: "user" | "agent_turn" | "worker";
  worker_id?: string;
  messages: MessageItem[];
}

function msgReasoning(message: MessageItem): string {
  return message.reasoning_content.trim();
}

function resolveAssetUrl(url: string): string {
  if (!url) return "";
  if (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("asset://")) {
    return url;
  }
  if (url.startsWith("/")) {
    return convertFileSrc(url);
  }
  return url;
}

function resolveMarkdownImages(md: string): string {
  return md.replace(
    /(!\[[^\]]*\]\()(\/[^\s)]+)(\))/g,
    (_, prefix, path, suffix) => prefix + resolveAssetUrl(path) + suffix,
  );
}

function renderContentMedia(message: MessageItem) {
  const content = Array.isArray(message.content) ? message.content : [];
  const mediaBlocks = content.filter((b) => b.type === 'media');
  const legacyMedia = message.media || [];
  const allMedia = [
    ...mediaBlocks.map((b) => ({ kind: b.kind!, url: b.url!, title: b.title, mime_type: b.mime_type })),
    ...legacyMedia,
  ];
  if (allMedia.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2 my-2">
      {allMedia.map((asset, index) => {
        const src = resolveAssetUrl(asset.url);
        if (asset.kind === "image") {
          return (
            <img
              key={`${message.id}-media-${index}`}
              src={src}
              alt={asset.title || "生成的图片"}
              className="max-w-full max-h-96 rounded-md cursor-pointer hover:opacity-90 transition-opacity"
              loading="lazy"
            />
          );
        }

        if (asset.kind === "video") {
          return (
            <video
              key={`${message.id}-media-${index}`}
              src={src}
              controls
              className="max-w-full max-h-96 rounded-md"
              preload="metadata"
            >
              {asset.title || "生成的视频"}
            </video>
          );
        }

        if (asset.kind === "audio") {
          return (
            <audio
              key={`${message.id}-media-${index}`}
              src={src}
              controls
              className="w-full"
              preload="metadata"
            >
              {asset.title || "生成的音频"}
            </audio>
          );
        }

        return (
          <a
            key={`${message.id}-media-${index}`}
            href={src}
            className="text-blue-400 hover:text-blue-300 underline text-sm"
            target="_blank"
            rel="noopener noreferrer"
          >
            {asset.title || asset.url}
          </a>
        );
      })}
    </div>
  );
}

function StreamingMessage({
  content,
  reasoningContent,
}: {
  content: string;
  reasoningContent: string;
}) {
  const resolvedTheme = useResolvedTheme();
  return (
    <div>
      {reasoningContent && (
        <ThinkingBlock content={reasoningContent} defaultExpanded={false} />
      )}
      <MdPreview modelValue={resolveMarkdownImages(content)} theme={resolvedTheme} previewTheme="github" />
      {content.length > 0 && (
        <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />
      )}
    </div>
  );
}

function groupMessages(messages: MessageItem[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  let currentAgentTurn: MessageGroup | null = null;

  for (const msg of messages) {
    if (msg.worker_id) {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      const previous = groups[groups.length - 1];
      if (previous?.type === "worker" && previous.worker_id === msg.worker_id) {
        previous.messages.push(msg);
      } else {
        groups.push({ key: `worker-${msg.worker_id}-${msg.id}`, type: "worker", worker_id: msg.worker_id, messages: [msg] });
      }
    } else if (msg.role === "user") {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      groups.push({ key: msg.id, type: "user", messages: [msg] });
    } else {
      if (!currentAgentTurn) {
        currentAgentTurn = { key: `turn-${msg.id}`, type: "agent_turn", messages: [] };
      }
      currentAgentTurn.messages.push(msg);
    }
  }
  if (currentAgentTurn) groups.push(currentAgentTurn);
  return groups;
}

// ---------------------------------------------------------------------------
// 从 LLM 输出系统消息中提取解释文本
// ---------------------------------------------------------------------------

function extractLlmExplanation(content: string): string {
  const lines = content.split("\n");
  const contentIdx = lines.findIndex((l) => l.startsWith("content:"));
  if (contentIdx >= 0 && contentIdx + 1 < lines.length) {
    return lines.slice(contentIdx + 1).join("\n").trim();
  }
  return "";
}

function llmOutputHasToolCalls(content: string): boolean {
  return content
    .split("\n")
    .some((line) => line.trim().startsWith("tool_calls:"));
}

function toolItemSucceeded(tool: MessageItem): boolean {
  return !textContent(tool).includes("ok=false") && !tool.tool_result_is_error;
}

function summarizeToolGroup(tools: MessageItem[]): string {
  const total = tools.length;
  const failed = tools.filter((tool) => !toolItemSucceeded(tool)).length;
  const succeeded = total - failed;
  const names = Array.from(
    new Set(
      tools
        .map((tool) => {
          const meta = getSystemMessageMeta(textContent(tool));
          return meta.toolName || meta.summary.split(" · ")[0] || "";
        })
        .filter(Boolean)
    )
  );
  const nameSummary = names.length > 0
    ? ` · ${names.slice(0, 3).join(", ")}${names.length > 3 ? ` 等 ${names.length} 类` : ""}`
    : "";
  const statusSummary = failed > 0 ? `成功 ${succeeded} / 失败 ${failed}` : `成功 ${succeeded}`;
  return `工具调用 ${total} 次 · ${statusSummary}${nameSummary}`;
}

/** 统一的智能体回合渲染 — 将系统消息（事件）和 assistant 消息（回复）合并展示 */
interface AgentTurnProps {
  messages: MessageItem[];
  streamingMessageId: string | null;
  streamingContent: string;
  streamingReasoningContent: string;
  hasTts: boolean;
  selectedAgentTab: string | null;
}

/** 从 agent_event 内容中提取相关 agent role */
function extractAgentRoles(content: string, agents: { role: string; label: string }[]): string[] {
  const roles = new Set<string>();
  const addByLabel = (label?: string) => {
    if (!label || label === "User") return;
    const agent = agents.find((item) => item.label === label);
    if (agent) roles.add(agent.role);
  };
  const createMatch = content.match(/^\[Agent\] .+? \((.+?)\)/);
  if (createMatch) roles.add(createMatch[1]);
  const statusMatch = content.match(/^\[Agent\] (.+?) 状态变更:/);
  if (statusMatch) addByLabel(statusMatch[1]);
  const lockMatch = content.match(/^\[文件锁\] .+ by (.+)$/);
  if (lockMatch) addByLabel(lockMatch[1]);
  return Array.from(roles);
}

function parseAgentReply(content: string): { label: string; body: string } | null {
  const match = content.match(/^<!-- tiangong-agent-reply -->\n<!-- label:([^\n]*) -->\n\n?([\s\S]*)$/);
  if (!match) return null;
  return {
    label: match[1].trim() || "Agent",
    body: match[2].trim(),
  };
}

function AgentTurnView({
  messages,
  streamingMessageId,
  streamingContent,
  streamingReasoningContent,
  hasTts,
  selectedAgentTab,
}: AgentTurnProps) {
  const searchQuery = useSearchStore(s => s.searchQuery);
  const currentMessageId = useSearchStore(s => s.currentMessageId);
  const currentMatchStart = useSearchStore(s => s.currentMatchStart);
  const caseSensitive = useSearchStore(s => s.caseSensitive);
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());
  const [expandedToolGroups, setExpandedToolGroups] = useState<Set<string>>(new Set());
  const agents = useStore((state) => state.agents);
  const resolvedTheme = useResolvedTheme();

  const toggleItem = (id: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  };
  const toggleToolGroup = (key: string) => {
    setExpandedToolGroups((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  };

  /** 带搜索高亮渲染文本（搜索激活时使用 HighlightText，否则返回原始文本） */
  const renderWithHighlight = (msgId: string, text: string) => {
    if (!searchQuery) return text;
    const occurrences = findTextOccurrences(text, searchQuery, caseSensitive);
    if (occurrences.length === 0) return text;
    const isCurrent = msgId === currentMessageId;
    return <HighlightText text={text} matches={occurrences} currentMatchStart={isCurrent ? currentMatchStart : null} />;
  };

  // 将消息序列解析为渲染片段
  type Fragment =
    | { type: "explanation"; text: string; time?: string }
    | { type: "thinking"; content: string; time?: string }
    | { type: "tool_group"; key: string; brief: string; tools: MessageItem[] }
    | { type: "memory_recall"; key: string; strategy: string; brief: string; hits: string[] }
    | { type: "user"; msg: MessageItem }
    | { type: "assistant"; msg: MessageItem; isStreaming: boolean }
    | { type: "error_system"; msg: MessageItem }
    | { type: "retry_system"; msg: MessageItem }
    | { type: "context_management"; msg: MessageItem }
    | { type: "agent_event"; category: string; content: string; agentRoles: string[] }
    | { type: "other_system"; msg: MessageItem };

  const fragments: Fragment[] = [];
  const shownReasonings = new Set<string>();
  let pendingTools: MessageItem[] = [];
  let pendingRecall: { strategy: string; key: string } | null = null;

  const flushTools = () => {
    if (pendingTools.length === 0) return;
    const key = pendingTools[0].id;
    fragments.push({
      type: "tool_group",
      key,
      brief: summarizeToolGroup(pendingTools),
      tools: [...pendingTools],
    });
    pendingTools = [];
  };

  const flushRecall = (resultMsg?: MessageItem) => {
    if (!pendingRecall) return;
    const strategy = pendingRecall.strategy;
    const key = pendingRecall.key;
    let brief: string;
    let hits: string[] = [];

    if (!resultMsg) {
      brief = `记忆检索 (${strategy})`;
    } else if (textContent(resultMsg).includes("无相关记忆")) {
      brief = `记忆检索 (${strategy}) · 无命中`;
    } else {
      const countMatch = textContent(resultMsg).match(/命中 (\d+) 条/);
      const count = countMatch ? countMatch[1] : "?";
      brief = `记忆检索 (${strategy}) · ${count} 条命中`;
      const lines = textContent(resultMsg).split("\n").slice(1);
      hits = lines.filter(l => l.startsWith("- "));
    }
    fragments.push({ type: "memory_recall", key, strategy, brief, hits });
    pendingRecall = null;
  };

  for (const msg of messages) {
    if (msg.role === "user") {
      flushTools();
      flushRecall();
      fragments.push({ type: "user", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[记忆检索] 策略:")) {
      flushTools();
      flushRecall();
      const strategyMatch = textContent(msg).match(/策略:\s*(.+)/);
      pendingRecall = {
        strategy: strategyMatch ? strategyMatch[1].trim() : "auto",
        key: msg.id,
      };
    } else if (msg.role === "system" && textContent(msg).startsWith("[记忆检索]") && pendingRecall) {
      flushRecall(msg);
    } else if (msg.role === "system" && textContent(msg).startsWith("LLM 输出")) {
      const reasoning = msgReasoning(msg);
      const explanation = extractLlmExplanation(textContent(msg));
      if (!reasoning && !explanation && llmOutputHasToolCalls(textContent(msg))) {
        continue;
      }
      flushTools();
      if (reasoning && !shownReasonings.has(reasoning)) {
        shownReasonings.add(reasoning);
        fragments.push({ type: "thinking", content: reasoning, time: msg.created_at });
      }
      if (explanation) {
        fragments.push({ type: "explanation", text: explanation, time: msg.created_at });
      }
    } else if (msg.role === "system" && (textContent(msg).includes("tool_name:") || textContent(msg).includes("exit_code") || textContent(msg).startsWith("工具执行 ["))) {
      pendingTools.push(msg);
    } else if (msg.role === "tool") {
      const toolName = msg.tool_name || "";
      if (toolName === "recall_memory") {
        flushTools();
        const isStart = textContent(msg).startsWith("[记忆检索] 策略:");
        if (isStart && !pendingRecall) {
          flushRecall();
          const strategyMatch = textContent(msg).match(/策略:\s*(\S+)/);
          pendingRecall = {
            strategy: strategyMatch ? strategyMatch[1] : "recall",
            key: msg.id,
          };
        } else if (pendingRecall) {
          flushRecall(msg);
        } else {
          flushRecall();
          pendingRecall = { strategy: "recall", key: msg.id };
          flushRecall(msg);
        }
      }
      continue;
    } else if (msg.role === "assistant") {
      const isStreaming = msg.id === streamingMessageId;
      const assistantReasoning = msgReasoning(msg);
      const hasVisibleAssistantContent =
        isStreaming ||
        textContent(msg).trim().length > 0 ||
        assistantReasoning.length > 0 ||
        !!msg.media?.length ||
        msg.content.some(b => b.type === "media");
      if (!hasVisibleAssistantContent) {
        continue;
      }

      flushTools();
      const prevFrag = fragments[fragments.length - 1];
      if (prevFrag?.type === "explanation" && prevFrag.text === textContent(msg).trim() && !isStreaming) {
        fragments.pop();
      }
      if (!isStreaming && assistantReasoning && !shownReasonings.has(assistantReasoning)) {
        shownReasonings.add(assistantReasoning);
        fragments.push({ type: "thinking", content: assistantReasoning, time: msg.created_at });
      }
      fragments.push({ type: "assistant", msg, isStreaming });
    } else if (msg.role === "system" && textContent(msg).startsWith("[错误]")) {
      flushTools();
      fragments.push({ type: "error_system", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[重试]")) {
      flushTools();
      fragments.push({ type: "retry_system", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[上下文管理]")) {
      if (textContent(msg).includes("正在压缩")) {
        continue;
      }
      flushTools();
      fragments.push({ type: "context_management", msg });
    } else if (msg.role === "system" && (
      textContent(msg).startsWith("[Agent]")
      || textContent(msg).startsWith("[文件锁]")
    )) {
      flushTools();
      let category = "info";
      if (textContent(msg).startsWith("[文件锁]")) category = "lock";
      fragments.push({ type: "agent_event", category, content: textContent(msg), agentRoles: extractAgentRoles(textContent(msg), agents) });
    } else if (msg.role === "system") {
      flushTools();
      fragments.push({ type: "other_system", msg });
    }
  }
  flushTools();
  flushRecall();

  const mergedFragments: Fragment[] = [];
  for (const frag of fragments) {
    const previous = mergedFragments[mergedFragments.length - 1];
    if (frag.type === "tool_group" && previous?.type === "tool_group") {
      previous.tools.push(...frag.tools);
      previous.brief = summarizeToolGroup(previous.tools);
      continue;
    }
    mergedFragments.push(frag);
  }

  /** 渲染工具条目 */
  const renderToolItem = (tool: MessageItem) => {
    const meta = getSystemMessageMeta(textContent(tool));
    const expanded = expandedItems.has(tool.id);
    return (
      <div key={tool.id} title={formatMessageTime(tool.created_at)}>
        <button
          className="w-full flex items-center gap-2 px-2 py-0.5 rounded text-xs text-muted-foreground hover:bg-muted/50 transition-colors text-left"
          onClick={() => toggleItem(tool.id)}
        >
          {expanded ? <ChevronDown className="w-3 h-3 shrink-0" /> : <ChevronRight className="w-3 h-3 shrink-0" />}
          <meta.icon className="w-3 h-3 shrink-0" />
          <span className="font-medium">{meta.label}</span>
          {!expanded && <span className="truncate opacity-60">{meta.summary}</span>}
        </button>
        {expanded && (
          <div className="ml-5 mt-0.5 px-3 py-2 rounded-md bg-muted/30 border border-border/50">
            <pre className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">{textContent(tool)}</pre>
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-1.5">
      {mergedFragments.map((frag, i) => {
        if (selectedAgentTab && frag.type !== "agent_event") {
          return null;
        }
        if (selectedAgentTab && frag.type === "agent_event" && frag.agentRoles.length > 0 && !frag.agentRoles.includes(selectedAgentTab)) {
          return null;
        }
        if (frag.type === "thinking") {
          return (
            <div key={`think-${i}`} title={formatMessageTime(frag.time)}>
              <ThinkingBlock content={frag.content} defaultExpanded={false} />
            </div>
          );
        }
        if (frag.type === "explanation") {
          return (
            <p key={`expl-${i}`} className="text-sm text-muted-foreground leading-relaxed whitespace-pre-wrap break-words" title={formatMessageTime(frag.time)}>
              {frag.text}
            </p>
          );
        }
        if (frag.type === "tool_group") {
          const collapsed = !expandedToolGroups.has(frag.key);
          const groupTime = frag.tools.length > 0 ? formatMessageTime(frag.tools[0].created_at) : "";
          return (
            <div key={`tools-${frag.key}`} title={groupTime}>
              <button
                className="flex items-center gap-2 px-2 py-0.5 text-xs text-muted-foreground hover:bg-muted/50 rounded transition-colors"
                onClick={() => toggleToolGroup(frag.key)}
              >
                {collapsed ? <ChevronRight className="w-3 h-3 shrink-0" /> : <ChevronDown className="w-3 h-3 shrink-0" />}
                <Cpu className="w-3 h-3 shrink-0" />
                <span>{frag.brief}</span>
              </button>
              {!collapsed && <div className="ml-4 space-y-0">{frag.tools.map(t => renderToolItem(t))}</div>}
            </div>
          );
        }
        if (frag.type === "memory_recall") {
          const expanded = expandedItems.has(frag.key);
          return (
            <div key={`recall-${frag.key}`}>
              <button
                className="flex items-center gap-2 px-2 py-0.5 text-xs text-muted-foreground hover:bg-muted/50 rounded transition-colors"
                onClick={() => toggleItem(frag.key)}
              >
                {expanded ? <ChevronDown className="w-3 h-3 shrink-0" /> : <ChevronRight className="w-3 h-3 shrink-0" />}
                <Brain className="w-3 h-3 shrink-0" />
                <span>{frag.brief}</span>
              </button>
              {expanded && frag.hits.length > 0 && (
                <div className="ml-6 mt-0.5 space-y-0.5">
                  {frag.hits.map((hit, idx) => {
                    const trimmed = hit.replace(/^-\s*/, "");
                    const scoreMatch = trimmed.match(/^\[([0-9.]+)\]\s*/);
                    const score = scoreMatch ? scoreMatch[1] : null;
                    const rest = scoreMatch ? trimmed.slice(scoreMatch[0].length) : trimmed;
                    const colonIdx = rest.indexOf(": ");
                    const title = colonIdx > 0 ? rest.slice(0, colonIdx) : rest;
                    const summary = colonIdx > 0 ? rest.slice(colonIdx + 2) : "";
                    const displaySummary = summary.length > 80 ? summary.slice(0, 77) + "..." : summary;
                    return (
                      <div key={idx} className="flex items-start gap-1.5 text-xs text-muted-foreground px-2 py-0.5">
                        {score && (
                          <span className="shrink-0 text-[10px] font-mono opacity-60">[{score}]</span>
                        )}
                        <span className="font-medium shrink-0">{title}</span>
                        {displaySummary && <span className="opacity-70 truncate">{displaySummary}</span>}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        }
        if (frag.type === "user") {
          return (
            <div key={frag.msg.id} className="flex justify-end" title={formatMessageTime(frag.msg.created_at)}>
              <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-sm text-foreground whitespace-pre-wrap break-words">
                {textContent(frag.msg)}
              </div>
            </div>
          );
        }
        if (frag.type === "assistant") {
          const { msg, isStreaming } = frag;
          const agentReply = !isStreaming ? parseAgentReply(textContent(msg)) : null;
          if (agentReply) {
            return (
              <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
                <div className="inline-flex items-center gap-1.5 rounded-full border border-green-500/30 bg-green-500/10 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-300 mb-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-green-500" />
                  {agentReply.label}
                </div>
                <div className="border-l-2 border-green-500/50 pl-3">
                  {agentReply.body ? (
                    searchQuery && findTextOccurrences(textContent(msg), searchQuery, caseSensitive).length > 0
                      ? <div className="text-sm whitespace-pre-wrap break-words">{renderWithHighlight(msg.id, agentReply.body)}</div>
                      : <MdPreview modelValue={resolveMarkdownImages(agentReply.body)} theme={resolvedTheme} previewTheme="github" />
                  ) : null}
                </div>
                {agentReply.body && <MessageActions text={agentReply.body} showTts={hasTts} />}
              </div>
            );
          }
          return (
            <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
              {isStreaming ? (
                <StreamingMessage
                  content={streamingContent}
                  reasoningContent={streamingReasoningContent}
                />
              ) : textContent(msg) || (msg.media && msg.media.length > 0) || msg.content.some(b => b.type === "media") ? (
                <div>
                  {renderContentMedia(msg)}
                  {searchQuery && findTextOccurrences(textContent(msg), searchQuery, caseSensitive).length > 0
                    ? <div className="text-sm whitespace-pre-wrap break-words">{renderWithHighlight(msg.id, textContent(msg))}</div>
                    : <MdPreview modelValue={resolveMarkdownImages(textContent(msg))} theme={resolvedTheme} previewTheme="github" />
                  }
                </div>
              ) : null}
              {!isStreaming && msg.content && <MessageActions text={textContent(msg)} showTts={hasTts} />}
            </div>
          );
        }
        if (frag.type === "error_system") {
          return (
            <div key={frag.msg.id} className="text-sm text-destructive bg-destructive/10 rounded-md px-3 py-2 my-1">
              {textContent(frag.msg).replace("[错误] ", "")}
            </div>
          );
        }
        if (frag.type === "retry_system") {
          return (
            <div key={frag.msg.id} className="text-xs text-yellow-600 dark:text-yellow-400 bg-yellow-500/10 rounded-md px-3 py-1.5 my-0.5">
              {textContent(frag.msg).replace("[重试] ", "")}
            </div>
          );
        }
        if (frag.type === "context_management") {
          const text = textContent(frag.msg).replace("[上下文管理] ", "");
          return (
            <div
              key={frag.msg.id}
              className="inline-flex max-w-full items-center gap-2 rounded-md border border-border/70 bg-muted/30 px-2.5 py-1 text-xs text-muted-foreground"
              title={formatMessageTime(frag.msg.created_at)}
            >
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span className="truncate">{text}</span>
            </div>
          );
        }
        if (frag.type === "agent_event") {
          const colorMap: Record<string, string> = {
            lock: "border-yellow-500/30 bg-yellow-500/5",
            info: "border-border bg-muted/30",
          };
          return (
            <div key={`agent-${i}`} className={`text-xs text-muted-foreground border rounded px-2 py-1 my-0.5 ${colorMap[frag.category] || colorMap.info}`}>
              {frag.content}
            </div>
          );
        }
        if (frag.type === "other_system") {
          return (
            <p key={frag.msg.id} className="text-xs text-muted-foreground">
              {textContent(frag.msg).split("\n")[0]}
            </p>
          );
        }
        return null;
      })}
    </div>
  );
}

function sameMessageRefs(left: MessageItem[], right: MessageItem[]): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i++) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}

function hasMessage(messages: MessageItem[], id: string | null): boolean {
  return !!id && messages.some((message) => message.id === message.id);
}

const AgentTurn = memo(AgentTurnView, (prev, next) => {
  if (
    prev.hasTts !== next.hasTts
    || !sameMessageRefs(prev.messages, next.messages)
    || prev.selectedAgentTab !== next.selectedAgentTab
  ) {
    return false;
  }

  const touchesStreamingMessage =
    hasMessage(prev.messages, prev.streamingMessageId)
    || hasMessage(prev.messages, next.streamingMessageId);
  if (!touchesStreamingMessage) {
    return true;
  }

  return prev.streamingMessageId === next.streamingMessageId
    && prev.streamingContent === next.streamingContent
    && prev.streamingReasoningContent === next.streamingReasoningContent;
});

interface SystemMessageMeta {
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  summary: string;
  toolName?: string;
}

function getSystemMessageMeta(content: string): SystemMessageMeta {
  if (content.startsWith("LLM 输出")) {
    const match = content.match(/^LLM 输出 \[(.+?)\]/);
    const label = match ? match[1] : "LLM";
    return { icon: Cpu, label, summary: content.split("\n")[0] };
  }
  if (content.includes("tool_name:") || content.includes("exit_code") || content.startsWith("工具执行 [")) {
    const nameMatch = content.match(/tool_name:\s*(\S+)/) || content.match(/^工具执行 \[(.+?)\]/);
    const okMatch = content.match(/ok=(\w+)/);
    const cmdMatch = content.match(/命令:\s*(.+)/);
    const parts = [];
    if (nameMatch) parts.push(nameMatch[1]);
    if (okMatch) parts.push(okMatch[1] === "true" ? "OK" : "FAIL");
    if (cmdMatch) {
      const cmd = cmdMatch[1];
      parts.push(cmd.length > 60 ? cmd.slice(0, 57) + "..." : cmd);
    }
    return {
      icon: Terminal,
      label: "工具执行",
      summary: parts.join(" · ") || content.split("\n")[0],
      toolName: nameMatch?.[1],
    };
  }
  if (
    content.startsWith("Plan 执行总结") ||
    content.includes("plan_execution_summary")
  ) {
    return {
      icon: FileText,
      label: "Plan 总结",
      summary: content.split("\n")[0],
    };
  }
  const firstLine = content.split("\n")[0];
  return {
    icon: FileText,
    label: "系统",
    summary: firstLine.length > 80 ? firstLine.slice(0, 80) + "..." : firstLine,
  };
}
