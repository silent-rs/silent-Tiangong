import { useStore } from '@/store/useStore';
import { ScrollArea } from './ui/scroll-area';
import { User, Bot, Loader2, ChevronRight, ChevronDown, Terminal, Cpu, FileText } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { vscDarkPlus } from 'react-syntax-highlighter/dist/esm/styles/prism';
import { TypingMessage } from './TypingMessage';
import { ThinkingBlock } from './ThinkingBlock';

import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

export function MessageList() {
  const { messages, runStatus, streamingMessageId, streamingContent, streamingReasoningContent } = useStore();
  const scrollRef = useRef<HTMLDivElement>(null);
  const prevMessagesLengthRef = useRef(0);
  const prevStreamingIdRef = useRef<string | null>(null);

  // 自动滚动到底部
  useEffect(() => {
    // 当消息数量增加或流式消息状态变化时，自动滚动
    const shouldScroll =
      messages.length > prevMessagesLengthRef.current ||
      streamingMessageId !== prevStreamingIdRef.current;

    if (shouldScroll) {
      // 使用 setTimeout 确保在 DOM 更新后滚动
      setTimeout(() => {
        scrollRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' });
      }, 100);
    }

    prevMessagesLengthRef.current = messages.length;
    prevStreamingIdRef.current = streamingMessageId;
  }, [messages.length, streamingMessageId]);

  const isThinking = runStatus !== 'idle';

  // Markdown 渲染器（用于非流式消息）
  const MarkdownComponents = {
    pre({ children, ...rest }: any) {
      return (
        <pre className="rounded-md text-xs bg-background border border-border p-3 my-1.5 overflow-x-auto" {...rest}>
          {children}
        </pre>
      );
    },
    code({ className, children, node, ...rest }: any) {
      const match = /language-(\w+)/.exec(className || '');
      // 判断是否是代码块：有语言标记，或者父节点是 pre
      const isBlock = match || node?.parentNode?.tagName === 'pre';
      const CodeHighlighter = SyntaxHighlighter as any;
      return isBlock ? (
        <CodeHighlighter
          style={vscDarkPlus}
          language={match?.[1] || 'text'}
          PreTag="div"
          className="rounded-md text-xs !bg-background border border-border"
          customStyle={{ padding: '12px', borderRadius: '6px', margin: '6px 0' }}
          codeTagProps={{ style: {} }}
        >
          {String(children).replace(/\n$/, '')}
        </CodeHighlighter>
      ) : (
        <code className="bg-muted text-foreground px-1 py-0.5 rounded text-xs font-mono" {...rest}>
          {children}
        </code>
      );
    },
    p({ children }: { children: ReactNode }) {
      return <p className="mb-2 last:mb-0 leading-6">{children}</p>;
    },
    ul({ children }: { children: ReactNode }) {
      return <ul className="list-disc pl-5 mb-2 space-y-1 [&_p]:mb-0 [&_p]:inline">{children}</ul>;
    },
    ol({ children }: { children: ReactNode }) {
      return <ol className="list-decimal pl-5 mb-2 space-y-1 [&_p]:mb-0 [&_p]:inline">{children}</ol>;
    },
    li({ children }: { children: ReactNode }) {
      return <li className="leading-6">{children}</li>;
    },
    h1({ children }: { children: ReactNode }) {
      return <h1 className="text-lg font-bold mb-3 mt-5 first:mt-0">{children}</h1>;
    },
    h2({ children }: { children: ReactNode }) {
      return <h2 className="text-base font-bold mb-2 mt-4 first:mt-0">{children}</h2>;
    },
    h3({ children }: { children: ReactNode }) {
      return <h3 className="text-sm font-bold mb-2 mt-3 first:mt-0">{children}</h3>;
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
      return <th className="border border-border px-3 py-1.5 text-left font-semibold">{children}</th>;
    },
    td({ children }: { children: ReactNode }) {
      return <td className="border border-border px-3 py-1.5">{children}</td>;
    },
  };

  return (
    <ScrollArea className="h-full">
      <div className="p-4">
        <div className="max-w-3xl mx-auto space-y-2">
          {messages.length === 0 && !isThinking ? (
            <div className="flex flex-col items-center justify-center h-full text-center py-20">
              <div className="w-16 h-16 rounded-full bg-primary flex items-center justify-center mb-4">
                <Bot className="w-8 h-8 text-primary-foreground" />
              </div>
              <h2 className="text-xl font-medium text-foreground mb-2">欢迎使用天工</h2>
              <p className="text-muted-foreground text-sm">我可以帮助您完成各种编程任务</p>
            </div>
          ) : (
            groupMessages(messages).map((group, groupIdx, allGroups) => {
              // 系统消息组
              if (group.type === 'system') {
                // 最后一组系统消息且正在执行时默认展开
                const isLastGroup = groupIdx === allGroups.length - 1;
                const isActive = isLastGroup && isThinking;
                return <SystemMessageGroup key={group.key} messages={group.messages} defaultExpanded={isActive} />;
              }

              const message = group.messages[0];
              const isStreaming = message.id === streamingMessageId;
              const isUser = message.role === 'User';
              const isAssistant = message.role === 'Assistant';

              return (
                <div
                  key={group.key}
                  className={`flex gap-3 mt-3 first:mt-0 ${
                    isUser ? 'justify-end' : 'justify-start'
                  }`}
                >
                  {isAssistant && (
                    <div className="w-8 h-8 rounded bg-primary flex items-center justify-center flex-shrink-0">
                      <Bot className="w-5 h-5 text-primary-foreground" />
                    </div>
                  )}
                  <div
                    className={`rounded-lg px-4 py-2.5 max-w-[80%] ${
                      isUser
                        ? 'bg-primary text-primary-foreground'
                        : 'bg-muted text-foreground'
                    }`}
                  >
                    {isAssistant ? (
                      <>
                        {isStreaming ? (
                          <TypingMessage
                            content={streamingContent}
                            reasoningContent={streamingReasoningContent}
                            speed={300}
                          />
                        ) : (
                          <div>
                            {message.reasoning_content && (
                              <ThinkingBlock content={message.reasoning_content} defaultExpanded={false} />
                            )}
                            <div className="prose prose-sm max-w-none text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
                              <ReactMarkdown remarkPlugins={[remarkGfm]} components={MarkdownComponents as any}>
                                {message.content}
                              </ReactMarkdown>
                            </div>
                          </div>
                        )}
                      </>
                    ) : (
                      <p className="whitespace-pre-wrap text-sm">{message.content}</p>
                    )}
                  </div>
                  {isUser && (
                    <div className="w-8 h-8 rounded bg-muted-foreground flex items-center justify-center flex-shrink-0">
                      <User className="w-5 h-5 text-background" />
                    </div>
                  )}
                </div>
              );
            })
          )}

          {/* 思考中/执行中指示器（仅在没有流式消息时显示） */}
          {isThinking && !streamingMessageId && (
            <div className="flex gap-3 justify-start">
              <div className="w-8 h-8 rounded bg-primary flex items-center justify-center flex-shrink-0">
                <Bot className="w-5 h-5 text-primary-foreground" />
              </div>
              <div className="bg-muted text-foreground rounded-lg px-4 py-2.5">
                <div className="flex items-center gap-2">
                  <Loader2 className="w-4 h-4 animate-spin" />
                  <span className="text-sm">
                    {runStatus === 'planning' && '正在制定计划...'}
                    {runStatus === 'executing' && '正在执行任务...'}
                    {runStatus === 'responding' && '正在生成回复...'}
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
// 消息分组：连续系统消息归为一组
// ---------------------------------------------------------------------------

interface MessageGroup {
  key: string;
  type: 'system' | 'normal';
  messages: { id: string; role: string; content: string; reasoning_content?: string; created_at: string }[];
}

function groupMessages(messages: MessageGroup['messages']): MessageGroup[] {
  const groups: MessageGroup[] = [];
  let currentSystemGroup: MessageGroup | null = null;

  for (const msg of messages) {
    if (msg.role === 'System') {
      if (!currentSystemGroup) {
        currentSystemGroup = { key: `sys-${msg.id}`, type: 'system', messages: [] };
      }
      currentSystemGroup.messages.push(msg);
    } else {
      if (currentSystemGroup) {
        groups.push(currentSystemGroup);
        currentSystemGroup = null;
      }
      groups.push({ key: msg.id, type: 'normal', messages: [msg] });
    }
  }
  if (currentSystemGroup) {
    groups.push(currentSystemGroup);
  }
  return groups;
}

// ---------------------------------------------------------------------------
// 系统消息组：可整体折叠/展开
// ---------------------------------------------------------------------------

interface RoundGroup {
  key: string;
  label: string;
  llm: MessageGroup['messages'][0] | null;
  tools: MessageGroup['messages'];
  others: MessageGroup['messages'];
}

/** 从 LLM 输出系统消息中提取 thinking 内容作为 round 标题 */
function extractThinkingFromLlm(content: string): string {
  // 格式: "LLM 输出 [...]\ntokens: ...\ntool_calls: ...\ncontent:\n实际内容"
  const lines = content.split('\n');
  const contentIdx = lines.findIndex((l) => l.startsWith('content:'));
  if (contentIdx >= 0 && contentIdx + 1 < lines.length) {
    const thinking = lines
      .slice(contentIdx + 1)
      .join(' ')
      .trim();
    if (thinking) return thinking;
  }
  // fallback: 直接取 reasoning_content 或工具调用信息
  const toolMatch = content.match(/tool_calls:\s*(.+)/);
  if (toolMatch) return `调用 ${toolMatch[1]}`;
  return '思考中...';
}

/** 将系统消息按 round 分组：LLM 输出 [react-round-N] + 后续工具执行归为一组 */
function groupByRound(messages: MessageGroup['messages']): RoundGroup[] {
  const rounds: RoundGroup[] = [];
  let current: RoundGroup | null = null;

  for (const msg of messages) {
    if (msg.content.startsWith('LLM 输出')) {
      if (current) rounds.push(current);
      const thinking = extractThinkingFromLlm(msg.content);
      current = { key: msg.id, label: thinking, llm: msg, tools: [], others: [] };
    } else if (current && (msg.content.includes('exit_code') || msg.content.includes('tool_name:'))) {
      current.tools.push(msg);
    } else if (current) {
      current.others.push(msg);
    } else {
      if (!current) {
        current = { key: msg.id, label: '执行', llm: null, tools: [], others: [msg] };
      }
    }
  }
  if (current) rounds.push(current);
  return rounds;
}

function SystemMessageGroup({ messages }: { messages: MessageGroup['messages']; defaultExpanded?: boolean }) {
  const [expandedRounds, setExpandedRounds] = useState<Set<string>>(new Set());
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

  const toggleRound = (key: string) => {
    setExpandedRounds((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const toggleItem = (id: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const rounds = groupByRound(messages);

  const renderItem = (msg: MessageGroup['messages'][0]) => {
    const meta = getSystemMessageMeta(msg.content);
    const itemExpanded = expandedItems.has(msg.id);
    return (
      <div key={msg.id}>
        <button
          className="w-full flex items-center gap-2 px-2 py-0.5 rounded text-xs text-muted-foreground hover:bg-muted/50 transition-colors text-left"
          onClick={() => toggleItem(msg.id)}
        >
          {itemExpanded ? (
            <ChevronDown className="w-3 h-3 shrink-0" />
          ) : (
            <ChevronRight className="w-3 h-3 shrink-0" />
          )}
          <meta.icon className="w-3 h-3 shrink-0" />
          <span className="font-medium">{meta.label}</span>
          {!itemExpanded && (
            <span className="truncate opacity-60">{meta.summary}</span>
          )}
        </button>
        {itemExpanded && (
          <div className="ml-5 mt-0.5 px-3 py-2 rounded-md bg-muted/30 border border-border/50">
            <pre className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">
              {msg.content}
            </pre>
          </div>
        )}
      </div>
    );
  };

  // 截断 thinking 文本
  const truncLabel = (text: string, max: number) =>
    text.length > max ? text.slice(0, max) + '...' : text;

  return (
    <div className="max-w-3xl space-y-0.5">
      {rounds.map((round) => {
        const roundExpanded = expandedRounds.has(round.key);
        const toolSummary = round.tools.map((t) =>
          t.content.includes('ok=true') ? 'OK' : 'FAIL'
        );
        const toolBrief = round.tools.length > 0
          ? `${round.tools.length} 次调用 (${toolSummary.join(', ')})`
          : '';

        return (
          <div key={round.key}>
            {/* Round 标题：thinking 内容 + 工具摘要 */}
            <button
              className="w-full flex items-center gap-2 px-3 py-1 rounded-md text-xs text-muted-foreground hover:bg-muted/50 transition-colors text-left"
              onClick={() => toggleRound(round.key)}
            >
              {roundExpanded ? (
                <ChevronDown className="w-3 h-3 shrink-0" />
              ) : (
                <ChevronRight className="w-3 h-3 shrink-0" />
              )}
              <Cpu className="w-3 h-3 shrink-0" />
              <span className="truncate">{truncLabel(round.label, 60)}</span>
              {!roundExpanded && toolBrief && (
                <span className="shrink-0 opacity-60">{toolBrief}</span>
              )}
            </button>

            {/* 展开：工具调用详情 */}
            {roundExpanded && (
              <div className="ml-7 mt-0.5 space-y-0.5">
                {round.tools.map((t) => renderItem(t))}
                {round.others.map((o) => renderItem(o))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function getSystemMessageMeta(content: string) {
  if (content.startsWith('LLM 输出')) {
    const match = content.match(/^LLM 输出 \[(.+?)\]/);
    const label = match ? match[1] : 'LLM';
    return { icon: Cpu, label, summary: content.split('\n')[0] };
  }
  if (content.includes('tool_name:') || content.includes('exit_code')) {
    const nameMatch = content.match(/tool_name:\s*(\S+)/);
    const codeMatch = content.match(/exit_code=(\d+)/);
    const okMatch = content.match(/ok=(\w+)/);
    const parts = [];
    if (nameMatch) parts.push(nameMatch[1]);
    if (codeMatch) parts.push(`exit=${codeMatch[1]}`);
    if (okMatch) parts.push(okMatch[1] === 'true' ? 'OK' : 'FAIL');
    return { icon: Terminal, label: '工具执行', summary: parts.join(' · ') || content.split('\n')[0] };
  }
  if (content.startsWith('Plan 执行总结') || content.includes('plan_execution_summary')) {
    return { icon: FileText, label: 'Plan 总结', summary: content.split('\n')[0] };
  }
  const firstLine = content.split('\n')[0];
  return { icon: FileText, label: '系统', summary: firstLine.length > 80 ? firstLine.slice(0, 80) + '...' : firstLine };
}
