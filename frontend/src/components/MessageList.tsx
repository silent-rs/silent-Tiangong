import { useStore } from "@/store/useStore";
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
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from '@tauri-apps/plugin-dialog';
import { ThinkingBlock } from "./ThinkingBlock";
import { AgentPanel } from "./AgentPanel";
import { api, textContent, type ContentBlock } from "@/api/tauri";
import { CopyableCodeBlock } from "./CopyableCodeBlock";
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
import type { ReactNode } from "react";

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

/** 独立的 Markdown 图片组件，避免在 useMemo 内部使用 useState */
function MarkdownImage({ src, alt }: { src?: string; alt?: string }) {
  const [fullscreen, setFullscreen] = useState(false);
  const resolvedSrc = resolveAssetUrl(src || "");
  return (
    <>
      <img
        src={resolvedSrc}
        alt={alt || "生成的图片"}
        className="max-w-full max-h-96 rounded-md my-2 cursor-pointer hover:opacity-90 transition-opacity"
        loading="lazy"
        onClick={() => setFullscreen(true)}
      />
      {fullscreen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 cursor-pointer"
          onClick={() => setFullscreen(false)}
        >
          <img
            src={resolvedSrc}
            alt={alt || "生成的图片"}
            className="max-w-[90vw] max-h-[90vh] object-contain rounded-md"
          />
        </div>
      )}
    </>
  );
}

function useMarkdownComponents() {
  return useMemo(() => ({
    pre({ children }: any) {
      return <>{children}</>;
    },
    code({ className, children, node, ...rest }: any) {
      const match = /language-(\w+)/.exec(className || "");
      const isBlock = match || node?.parentNode?.tagName === "pre";
      return isBlock ? (
        <CopyableCodeBlock
          code={String(children).replace(/\n$/, "")}
          language={match?.[1] || "text"}
        />
      ) : (
        <code
          className="bg-muted text-foreground px-1 py-0.5 rounded text-xs font-mono"
          {...rest}
        >
          {children}
        </code>
      );
    },
    p({ children }: { children: ReactNode }) {
      return <p className="mb-2 last:mb-0 leading-6">{children}</p>;
    },
    ul({ children }: { children: ReactNode }) {
      return (
        <ul className="list-disc pl-5 mb-2 space-y-1 [&_p]:mb-0 [&_p]:inline">
          {children}
        </ul>
      );
    },
    ol({ children }: { children: ReactNode }) {
      return (
        <ol className="list-decimal pl-5 mb-2 space-y-1 [&_p]:mb-0 [&_p]:inline">
          {children}
        </ol>
      );
    },
    li({ children }: { children: ReactNode }) {
      return <li className="leading-6">{children}</li>;
    },
    h1({ children }: { children: ReactNode }) {
      return (
        <h1 className="text-lg font-bold mb-3 mt-5 first:mt-0">{children}</h1>
      );
    },
    h2({ children }: { children: ReactNode }) {
      return (
        <h2 className="text-base font-bold mb-2 mt-4 first:mt-0">{children}</h2>
      );
    },
    h3({ children }: { children: ReactNode }) {
      return (
        <h3 className="text-sm font-bold mb-2 mt-3 first:mt-0">{children}</h3>
      );
    },
    blockquote({ children }: { children: ReactNode }) {
      return (
        <blockquote className="border-l-4 border-accent-foreground/30 pl-4 py-2 my-3 text-foreground/80 italic">
          {children}
        </blockquote>
      );
    },
    strong({ children }: { children: ReactNode }) {
      return <strong className="font-bold text-foreground">{children}</strong>;
    },
    a({ href, children }: { href: string; children: ReactNode }) {
      return (
        <a
          href={href}
          className="text-blue-400 hover:text-blue-300 underline"
          target="_blank"
          rel="noopener noreferrer"
        >
          {children}
        </a>
      );
    },
    img: MarkdownImage,
    table({ children }: { children: ReactNode }) {
      return (
        <div className="my-3 overflow-x-auto">
          <table className="min-w-full border-collapse border border-border text-sm">
            {children}
          </table>
        </div>
      );
    },
    thead({ children }: { children: ReactNode }) {
      return <thead className="bg-muted/50">{children}</thead>;
    },
    th({ children }: { children: ReactNode }) {
      return (
        <th className="border border-border px-3 py-1.5 text-left font-semibold">
          {children}
        </th>
      );
    },
    td({ children }: { children: ReactNode }) {
      return <td className="border border-border px-3 py-1.5">{children}</td>;
    },
    video({ src, children, ...rest }: any) {
      return (
        <video
          src={src ? resolveAssetUrl(src) : undefined}
          controls
          className="max-w-full max-h-96 rounded-md my-2"
          preload="metadata"
          {...rest}
        >
          {children}
        </video>
      );
    },
    source({ src, type, ...rest }: any) {
      return <source src={resolveAssetUrl(src)} type={type} {...rest} />;
    },
    audio({ src, children, ...rest }: any) {
      return (
        <audio
          src={src ? resolveAssetUrl(src) : undefined}
          controls
          className="w-full my-2"
          preload="metadata"
          {...rest}
        >
          {children}
        </audio>
      );
    },
  }), []);
}

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
        <div className="max-w-[100%] text-muted-foreground">
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
                  {textContent(message)}
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

  // 检查 TTS 能力
  useEffect(() => {
    api.hasTtsCapability().then(setHasTts).catch(() => setHasTts(false));
  }, []);

  // 检查多模态能力
  useEffect(() => {
    api.hasModelCapability('multimodal').then(setHasMultimodal).catch(() => setHasMultimodal(false));
  }, []);

  const isThinking = runStatus !== "idle";
  const isContextCompressing = runSummary.includes("正在压缩");
  const MarkdownComponents = useMarkdownComponents();

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

  // 新消息到达时滚动到底部
  useEffect(() => {
    const tabChanged = selectedAgentTab !== prevSelectedAgentTabRef.current;
    const shouldScroll =
      messages.length > prevMessagesLengthRef.current ||
      streamingMessageId !== prevStreamingIdRef.current ||
      tabChanged;

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
        el.scrollIntoView({ behavior: "smooth", block: "end" });
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

  return (
    <ScrollArea className="h-full" viewportRef={viewportRef}>
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
                          messages={contentMessages}
                          streamingMessageId={null}
                          streamingContent=""
                          streamingReasoningContent=""
                          MarkdownComponents={MarkdownComponents}
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
                        MarkdownComponents={MarkdownComponents}
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
                    MarkdownComponents={MarkdownComponents}
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
  MarkdownComponents,
}: {
  content: string;
  reasoningContent: string;
  MarkdownComponents: any;
}) {
  return (
    <div>
      {reasoningContent && (
        <ThinkingBlock content={reasoningContent} defaultExpanded={false} />
      )}
      <div className="prose prose-sm max-w-none break-words text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
        <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw]} components={MarkdownComponents as any}>
          {content}
        </ReactMarkdown>
        {content.length > 0 && (
          <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse align-text-bottom" />
        )}
      </div>
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
  MarkdownComponents: any;
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
  MarkdownComponents,
  hasTts,
  selectedAgentTab,
}: AgentTurnProps) {
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());
  const [expandedToolGroups, setExpandedToolGroups] = useState<Set<string>>(new Set());
  const agents = useStore((state) => state.agents);

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
              <div className="max-w-[92%] rounded-md border border-border/70 bg-muted/40 px-3 py-1.5 text-sm text-muted-foreground whitespace-pre-wrap break-words">
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
                    <div className="prose prose-sm max-w-none break-words text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
                      <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw]} components={MarkdownComponents as any}>
                        {agentReply.body}
                      </ReactMarkdown>
                    </div>
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
                  MarkdownComponents={MarkdownComponents}
                />
              ) : textContent(msg) || (msg.media && msg.media.length > 0) || msg.content.some(b => b.type === "media") ? (
                <div>
                  {renderContentMedia(msg)}
                  <div className="prose prose-sm max-w-none break-words text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
                    <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw]} components={MarkdownComponents as any}>
                      {textContent(msg)}
                    </ReactMarkdown>
                  </div>
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
    || prev.MarkdownComponents !== next.MarkdownComponents
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
