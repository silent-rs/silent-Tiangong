import { useStore } from "@/store/useStore";
import { ScrollArea } from "./ui/scroll-area";
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
  GitBranch,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeRaw from "rehype-raw";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { vscDarkPlus } from "react-syntax-highlighter/dist/esm/styles/prism";
import { TypingMessage } from "./TypingMessage";
import { ThinkingBlock } from "./ThinkingBlock";
import { api } from "@/api/tauri";

import { useEffect, useRef, useState } from "react";
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
    console.log("handleTts called, playing:", playing, "text length:", text.length);
    if (playing) {
      api.stopAudio().catch(() => {});
      setPlaying(false);
      return;
    }

    setTtsLoading(true);
    try {
      // 先停止可能正在播放的音频（不等待）
      api.stopAudio().catch(() => {});

      console.log("开始 TTS 合成...");
      const result = await api.synthesizeSpeech(text);
      console.log("TTS 合成完成，文件路径:", result.file_path);

      setPlaying(true);
      setTtsLoading(false);
      // 通过系统原生命令播放音频文件（afplay on macOS）
      await api.playAudioFile(result.file_path);
      // playAudioFile 阻塞到播放完成
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

function UserMessageActions({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

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
      <button className={btnClass} title="分叉（开发中）" disabled>
        <GitBranch className="w-3.5 h-3.5" />
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

function WorkerCard({ group, isActive, MarkdownComponents }: {
  group: MessageGroup;
  isActive: boolean;
  MarkdownComponents: any;
}) {
  // 从 "🔧 Worker: xxx" 系统消息中提取标题
  const workerStartMsg = group.messages.find(m => m.role === "system" && m.content.startsWith("🔧 Worker:"));
  const workerTitle = workerStartMsg?.content?.replace("🔧 Worker: ", "") || "Worker";
  // 过滤掉 Worker 标题系统消息（以 "🔧 Worker:" 开头的）
  const contentMessages = group.messages.filter(m => !(m.role === "system" && m.content.startsWith("🔧 Worker:")));
  const systemMsgs = contentMessages.filter(m => m.role === "system");
  // 只显示有内容的 assistant 消息（content 或 reasoning_content 不为空）
  const assistantMsgs = contentMessages.filter(m =>
    m.role === "assistant"
      && (m.content.trim().length > 0
        || msgReasoning(m).length > 0
        || !!m.media?.length)
  );

  // 计算 Worker 耗时（从第一条到最后一条消息的时间差）
  const workerDuration = (() => {
    if (group.messages.length < 2) return null;
    const first = new Date(group.messages[0].created_at).getTime();
    const last = new Date(group.messages[group.messages.length - 1].created_at).getTime();
    const ms = last - first;
    return ms > 0 ? ms : null;
  })();

  // Worker 完成后自动收缩（有 assistant 回复且不活跃时）
  const hasResult = assistantMsgs.length > 0;
  const [collapsed, setCollapsed] = useState(false);
  const prevIsActiveRef = useRef(isActive);

  useEffect(() => {
    // 从活跃变为非活跃且有结果时自动收缩
    if (prevIsActiveRef.current && !isActive && hasResult) {
      setCollapsed(true);
    }
    prevIsActiveRef.current = isActive;
  }, [isActive, hasResult]);

  return (
    <div className="mt-3 border border-border rounded-lg overflow-hidden">
      <button
        className="w-full px-3 py-1.5 bg-muted/50 border-b border-border flex items-center gap-2 hover:bg-muted/80 transition-colors text-left"
        onClick={() => setCollapsed(!collapsed)}
      >
        {collapsed ? (
          <ChevronRight className="w-3 h-3 text-muted-foreground" />
        ) : (
          <ChevronDown className="w-3 h-3 text-muted-foreground" />
        )}
        <Cpu className="w-3.5 h-3.5 text-muted-foreground" />
        <span className="text-xs font-medium text-muted-foreground flex-1">{workerTitle}</span>
        {collapsed && assistantMsgs.length > 0 && (
          <span className="text-xs text-muted-foreground/60 truncate max-w-[200px]">
            {assistantMsgs[assistantMsgs.length - 1].content
              ? `${assistantMsgs[assistantMsgs.length - 1].content.slice(0, 50)}...`
              : "[媒体结果]"}
          </span>
        )}
        {hasResult && workerDuration && (
          <span className="text-xs text-muted-foreground/50">
            {(workerDuration / 1000).toFixed(1)}s
          </span>
        )}
      </button>
      {!collapsed && (
        <div className="p-2">
          {systemMsgs.length > 0 && (
            <AgentTurn
              messages={systemMsgs}
              streamingMessageId={null}
              streamingContent=""
              streamingReasoningContent=""
              MarkdownComponents={MarkdownComponents}
              hasTts={false}
            />
          )}
          {assistantMsgs.map((msg) => (
            <div key={msg.id} className="mt-2">
              <div className="flex justify-start">
                <div className="max-w-[100%] text-foreground">
                  {msg.reasoning_content && (
                    <ThinkingBlock
                      content={msg.reasoning_content}
                      defaultExpanded={false}
                    />
                  )}
                  {renderMessageMedia(msg)}
                  <div className="prose prose-sm max-w-none break-words text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
                    <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw]} components={MarkdownComponents as any}>
                      {msg.content}
                    </ReactMarkdown>
                  </div>
                </div>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function MessageList() {
  const {
    messages,
    runStatus,
    runSummary,
    streamingMessageId,
    streamingContent,
    streamingReasoningContent,
    voiceMessages,
    approvalRequestId,
  } = useStore();
  const scrollRef = useRef<HTMLDivElement>(null);
  const prevMessagesLengthRef = useRef(0);
  const prevStreamingIdRef = useRef<string | null>(null);
  const [hasTts, setHasTts] = useState(false);

  // 检查 TTS 能力
  useEffect(() => {
    api.hasTtsCapability().then(setHasTts).catch(() => setHasTts(false));
  }, []);

  const isThinking = runStatus !== "idle";

  // 计算消息内容总长度，用于检测内容增长
  const totalContentLength = messages.reduce((sum, m) => sum + (m.content?.length || 0), 0);

  // 自动滚动到底部
  useEffect(() => {
    // 消息数量增加、流式状态变化、或消息内容增长（流式输出中）时自动滚动
    const shouldScroll =
      messages.length > prevMessagesLengthRef.current ||
      streamingMessageId !== prevStreamingIdRef.current ||
      isThinking;

    if (shouldScroll) {
      // 使用 setTimeout 确保在 DOM 更新后滚动
      setTimeout(() => {
        scrollRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
      }, 100);
    }

    prevMessagesLengthRef.current = messages.length;
    prevStreamingIdRef.current = streamingMessageId;
  }, [messages.length, streamingMessageId, totalContentLength, isThinking]);

  // 将本地文件路径转换为 Tauri asset URL
  // Markdown 渲染器（用于非流式消息）
  const MarkdownComponents = {
    pre({ children, ...rest }: any) {
      return (
        <pre
          className="rounded-md text-xs !bg-background border border-border p-3 my-1.5 overflow-x-auto"
          {...rest}
        >
          {children}
        </pre>
      );
    },
    code({ className, children, node, ...rest }: any) {
      const match = /language-(\w+)/.exec(className || "");
      // 判断是否是代码块：有语言标记，或者父节点是 pre
      const isBlock = match || node?.parentNode?.tagName === "pre";
      const CodeHighlighter = SyntaxHighlighter as any;
      return isBlock ? (
        <CodeHighlighter
          style={vscDarkPlus}
          language={match?.[1] || "text"}
          PreTag="div"
          className="rounded-md text-xs !bg-background"
          customStyle={{ padding: "0", margin: "0" }}
          codeTagProps={{ style: {} }}
        >
          {String(children).replace(/\n$/, "")}
        </CodeHighlighter>
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
    img({ src, alt }: { src?: string; alt?: string }) {
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
    },
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
  };

  return (
    <ScrollArea className="h-full">
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
            groupMessages(messages).map((group, groupIdx, allGroups) => {
              // Worker 组
              if (group.type === "worker") {
                const isLastGroup = groupIdx === allGroups.length - 1;
                return (
                  <WorkerCard
                    key={group.key}
                    group={group}
                    isActive={isLastGroup && isThinking}
                    MarkdownComponents={MarkdownComponents}
                  />
                );
              }

              // 智能体回合：系统消息 + assistant 消息统一展示
              if (group.type === "agent_turn") {
                return (
                  <div key={group.key} className="mt-3 first:mt-0">
                    <AgentTurn
                      messages={group.messages}
                      streamingMessageId={streamingMessageId}
                      streamingContent={streamingContent}
                      streamingReasoningContent={streamingReasoningContent}
                      MarkdownComponents={MarkdownComponents}
                      hasTts={hasTts}
                    />
                  </div>
                );
              }

              // 用户消息
              const message = group.messages[0];
              const voiceInfo = voiceMessages[message.id];
              return (
                <div key={group.key} className="mt-3 first:mt-0">
                  <div className="flex justify-end" title={formatMessageTime(message.created_at)}>
                    <div className="max-w-[100%] text-muted-foreground">
                      {voiceInfo ? (
                        <VoiceBubble
                          messageId={message.id}
                          audioPath={voiceInfo.audioPath}
                          duration={voiceInfo.duration}
                          showText={voiceInfo.showText}
                          content={message.content}
                        />
                      ) : (
                        <p className="whitespace-pre-wrap break-words text-sm">
                          {message.content}
                        </p>
                      )}
                    </div>
                  </div>
                  {message.content && (
                    <div className="flex justify-end">
                      <UserMessageActions text={message.content} />
                    </div>
                  )}
                </div>
              );
            })
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

          {/* 思考中/执行中指示器（仅在助手尚未回复时显示） */}
          {isThinking && runStatus !== "waiting_approval" && !streamingMessageId && !streamingContent &&
           !(messages.length > 0 && messages[messages.length - 1].role === "assistant") && (
            <div className="flex justify-start">
              <div className="text-foreground">
                <div className="flex items-center gap-2">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  <span className="text-sm text-muted-foreground">
                    {runStatus === "planning" && "正在制定计划..."}
                    {runStatus === "executing" && "正在执行任务..."}
                    {runStatus === "responding" && "正在生成回复..."}
                  </span>
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
  role: "system" | "user" | "assistant";
  content: string;
  reasoning_content: string;
  worker_id?: string;
  media?: {
    kind: "image" | "video" | "audio" | "file";
    url: string;
    mime_type?: string;
    title?: string;
    capability?: string;
  }[];
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

function renderMessageMedia(message: MessageItem) {
  if (!message.media || message.media.length === 0) {
    return null;
  }

  return (
    <div className="space-y-2 my-2">
      {message.media.map((asset, index) => {
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

function groupMessages(messages: MessageItem[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  const workerGroupMap = new Map<string, number>();
  let currentAgentTurn: MessageGroup | null = null;

  for (const msg of messages) {
    if (msg.worker_id) {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      const idx = workerGroupMap.get(msg.worker_id);
      if (idx !== undefined) {
        groups[idx].messages.push(msg);
      } else {
        workerGroupMap.set(msg.worker_id, groups.length);
        groups.push({ key: `worker-${msg.worker_id}`, type: "worker", worker_id: msg.worker_id, messages: [msg] });
      }
    } else if (msg.role === "user") {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      groups.push({ key: msg.id, type: "user", messages: [msg] });
    } else {
      // System + Assistant → 合并到同一个 agent_turn
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
  // 格式: "LLM 输出 [...]\ntokens: ...\ntool_calls: ...\ncontent:\n实际内容"
  const lines = content.split("\n");
  const contentIdx = lines.findIndex((l) => l.startsWith("content:"));
  if (contentIdx >= 0 && contentIdx + 1 < lines.length) {
    return lines.slice(contentIdx + 1).join("\n").trim();
  }
  return "";
}

/** 统一的智能体回合渲染 — 将系统消息（事件）和 assistant 消息（回复）合并展示 */
function AgentTurn({
  messages,
  streamingMessageId,
  streamingContent,
  streamingReasoningContent: _streamingReasoningContent,
  MarkdownComponents,
  hasTts,
}: {
  messages: MessageItem[];
  streamingMessageId: string | null;
  streamingContent: string;
  streamingReasoningContent: string;
  MarkdownComponents: any;
  hasTts: boolean;
}) {
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());
  const [collapsedToolGroups, setCollapsedToolGroups] = useState<Set<string>>(new Set());

  const toggleItem = (id: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  };
  const toggleToolGroup = (key: string) => {
    setCollapsedToolGroups((prev) => {
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
    | { type: "assistant"; msg: MessageItem; isStreaming: boolean }
    | { type: "error_system"; msg: MessageItem }
    | { type: "retry_system"; msg: MessageItem }
    | { type: "other_system"; msg: MessageItem };

  const fragments: Fragment[] = [];
  const shownReasonings = new Set<string>();
  let pendingTools: MessageItem[] = [];

  const flushTools = () => {
    if (pendingTools.length === 0) return;
    const summary = pendingTools.map(t => t.content.includes("ok=true") ? "OK" : "FAIL");
    const key = pendingTools[0].id;
    fragments.push({
      type: "tool_group",
      key,
      brief: `${pendingTools.length} 次调用 (${summary.join(", ")})`,
      tools: [...pendingTools],
    });
    pendingTools = [];
  };

  for (const msg of messages) {
    if (msg.role === "system" && msg.content.startsWith("LLM 输出")) {
      flushTools();
      // 提取 reasoning
      const reasoning = msgReasoning(msg);
      if (reasoning && !shownReasonings.has(reasoning)) {
        shownReasonings.add(reasoning);
        fragments.push({ type: "thinking", content: reasoning, time: msg.created_at });
      }
      // 提取解释文本
      const explanation = extractLlmExplanation(msg.content);
      if (explanation) {
        fragments.push({ type: "explanation", text: explanation, time: msg.created_at });
      }
    } else if (msg.role === "system" && (msg.content.includes("tool_name:") || msg.content.includes("exit_code") || msg.content.startsWith("工具执行 ["))) {
      pendingTools.push(msg);
    } else if (msg.role === "assistant") {
      flushTools();
      const isStreaming = msg.id === streamingMessageId;
      // 跳过与前一个 explanation 完全重复的 assistant 内容
      const prevFrag = fragments[fragments.length - 1];
      if (prevFrag?.type === "explanation" && prevFrag.text === msg.content.trim() && !isStreaming) {
        // 内容重复，移除前面的 explanation，只保留 assistant
        fragments.pop();
      }
      // assistant 自身携带的 reasoning（DirectAnswer 模式等无系统消息场景）
      const assistantReasoning = msgReasoning(msg);
      if (assistantReasoning && !shownReasonings.has(assistantReasoning)) {
        shownReasonings.add(assistantReasoning);
        fragments.push({ type: "thinking", content: assistantReasoning, time: msg.created_at });
      }
      fragments.push({ type: "assistant", msg, isStreaming });
    } else if (msg.role === "system" && msg.content.startsWith("[错误]")) {
      flushTools();
      fragments.push({ type: "error_system", msg });
    } else if (msg.role === "system" && msg.content.startsWith("[重试]")) {
      flushTools();
      fragments.push({ type: "retry_system", msg });
    } else if (msg.role === "system") {
      flushTools();
      fragments.push({ type: "other_system", msg });
    }
  }
  flushTools();

  /** 渲染工具条目 */
  const renderToolItem = (tool: MessageItem) => {
    const meta = getSystemMessageMeta(tool.content);
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
            <pre className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">{tool.content}</pre>
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-1.5">
      {fragments.map((frag, i) => {
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
          const collapsed = collapsedToolGroups.has(frag.key);
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
        if (frag.type === "assistant") {
          const { msg, isStreaming } = frag;
          return (
            <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
              {isStreaming ? (
                <TypingMessage content={streamingContent} reasoningContent={_streamingReasoningContent} speed={300} />
              ) : msg.content || (msg.media && msg.media.length > 0) ? (
                <div>
                  {renderMessageMedia(msg)}
                  <div className="prose prose-sm max-w-none break-words text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
                    <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeRaw]} components={MarkdownComponents as any}>
                      {msg.content}
                    </ReactMarkdown>
                  </div>
                </div>
              ) : null}
              {!isStreaming && msg.content && <MessageActions text={msg.content} showTts={hasTts} />}
            </div>
          );
        }
        if (frag.type === "error_system") {
          return (
            <div key={frag.msg.id} className="text-sm text-destructive bg-destructive/10 rounded-md px-3 py-2 my-1">
              {frag.msg.content.replace("[错误] ", "")}
            </div>
          );
        }
        if (frag.type === "retry_system") {
          return (
            <div key={frag.msg.id} className="text-xs text-yellow-600 dark:text-yellow-400 bg-yellow-500/10 rounded-md px-3 py-1.5 my-0.5">
              {frag.msg.content.replace("[重试] ", "")}
            </div>
          );
        }
        if (frag.type === "other_system") {
          return (
            <p key={frag.msg.id} className="text-xs text-muted-foreground">
              {frag.msg.content.split("\n")[0]}
            </p>
          );
        }
        return null;
      })}
    </div>
  );
}

function getSystemMessageMeta(content: string) {
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
