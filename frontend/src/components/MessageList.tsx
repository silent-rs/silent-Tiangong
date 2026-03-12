import { useStore } from '@/store/useStore';
import { ScrollArea } from './ui/scroll-area';
import { User, Bot, Loader2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { vscDarkPlus } from 'react-syntax-highlighter/dist/esm/styles/prism';
import { TypingMessage } from './TypingMessage';
import { ThinkingBlock } from './ThinkingBlock';
import { useEffect, useRef } from 'react';
import type { ReactNode } from 'react';

export function MessageList() {
  const { messages, runStatus, streamingMessageId, streamingContent, streamingReasoningContent } = useStore();
  const scrollRef = useRef<HTMLDivElement>(null);
  const prevMessagesLengthRef = useRef(0);
  const prevStreamingIdRef = useRef<string | null>(null);

  // 角色名称映射
  const getRoleDisplayName = (role: string): string => {
    const roleMap: Record<string, string> = {
      'User': '用户',
      'Assistant': '助手',
      'System': '系统',
    };
    return roleMap[role] || role;
  };

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
    code({ className, children, ...props }: any) {
      const match = /language-(\w+)/.exec(className || '');
      const hasLanguage = match && match[1];
      return hasLanguage ? (
        (SyntaxHighlighter as any)(
          {
            style: vscDarkPlus,
            language: match[1],
            PreTag: "div",
            className: "rounded-md text-sm",
            customStyle: {
              background: '#1E1E1E',
              padding: '16px',
              borderRadius: '8px',
              margin: '8px 0',
            },
          },
          String(children).replace(/\n$/, '')
        )
      ) : (
        <code
          className="bg-[#2D2D30] text-[#E9E9E9] px-1.5 py-0.5 rounded text-sm font-mono"
          {...props}
        >
          {children}
        </code>
      );
    },
    p({ children }: { children: ReactNode }) {
      return <p className="mb-3 last:mb-0 leading-7">{children}</p>;
    },
    ul({ children }: { children: ReactNode }) {
      return <ul className="list-disc list-inside mb-3 space-y-1.5">{children}</ul>;
    },
    ol({ children }: { children: ReactNode }) {
      return <ol className="list-decimal list-inside mb-3 space-y-1.5">{children}</ol>;
    },
    h1({ children }: { children: ReactNode }) {
      return <h1 className="text-xl font-bold mb-4 mt-6 first:mt-0">{children}</h1>;
    },
    h2({ children }: { children: ReactNode }) {
      return <h2 className="text-lg font-bold mb-3 mt-5 first:mt-0">{children}</h2>;
    },
    h3({ children }: { children: ReactNode }) {
      return <h3 className="text-base font-bold mb-2 mt-4 first:mt-0">{children}</h3>;
    },
    blockquote({ children }: { children: ReactNode }) {
      return (
        <blockquote className="border-l-4 border-[#10A37F] pl-4 py-2 my-3 text-[#CCCCCC] italic">
          {children}
        </blockquote>
      );
    },
    strong({ children }: { children: ReactNode }) {
      return <strong className="font-bold text-white">{children}</strong>;
    },
    a({ href, children }: { href: string; children: ReactNode }) {
      return (
        <a
          href={href}
          className="text-[#10A37F] hover:text-[#0D8A6A] underline"
          target="_blank"
          rel="noopener noreferrer"
        >
          {children}
        </a>
      );
    },
  };

  return (
    <ScrollArea className="flex-1">
      <div className="p-4">
        <div className="max-w-3xl mx-auto space-y-6">
          {messages.length === 0 && !isThinking ? (
            <div className="flex flex-col items-center justify-center h-full text-center py-20">
              <div className="w-16 h-16 rounded-full bg-[#10A37F] flex items-center justify-center mb-4">
                <Bot className="w-8 h-8 text-white" />
              </div>
              <h2 className="text-xl font-medium text-white mb-2">欢迎使用天工</h2>
              <p className="text-[#858585] text-sm">我可以帮助您完成各种编程任务</p>
            </div>
          ) : (
            messages.map((message) => {
              const isStreaming = message.id === streamingMessageId;
              const isUser = message.role === 'User';
              const isAssistant = message.role === 'Assistant';

              return (
                <div
                  key={message.id}
                  className={`flex gap-3 ${
                    isUser ? 'justify-end' : 'justify-start'
                  }`}
                >
                  {isAssistant && (
                    <div className="w-8 h-8 rounded bg-[#10A37F] flex items-center justify-center flex-shrink-0">
                      <Bot className="w-5 h-5 text-white" />
                    </div>
                  )}
                  <div
                    className={`rounded-lg px-4 py-2.5 max-w-[80%] ${
                      isUser
                        ? 'bg-[#10A37F] text-white'
                        : 'bg-[#2D2D30] text-[#F3F4F6]'
                    }`}
                  >
                    {/* 角色标签（仅在助手消息时显示） */}
                    {isAssistant && !isStreaming && (
                      <div className="text-xs text-[#10A37F] font-medium mb-2">
                        {getRoleDisplayName(message.role)}
                      </div>
                    )}
                    {isAssistant ? (
                      <>
                        {/* 流式消息使用打字机效果 */}
                        {isStreaming ? (
                          <TypingMessage
                            content={streamingContent}
                            reasoningContent={streamingReasoningContent}
                            speed={300} // 每分钟 300 字符
                          />
                        ) : (
                          <div>
                            {/* 正常输出 */}
                            <div className="prose prose-invert prose-sm max-w-none">
                              <ReactMarkdown components={MarkdownComponents as any}>
                                {message.content}
                              </ReactMarkdown>
                            </div>

                            {/* 思考过程 - 独立区块显示 */}
                            {message.reasoning_content && (
                              <ThinkingBlock content={message.reasoning_content} defaultExpanded={false} />
                            )}
                          </div>
                        )}
                      </>
                    ) : (
                      <p className="whitespace-pre-wrap text-sm">{message.content}</p>
                    )}
                  </div>
                  {isUser && (
                    <div className="w-8 h-8 rounded bg-[#6B7280] flex items-center justify-center flex-shrink-0">
                      <User className="w-5 h-5 text-white" />
                    </div>
                  )}
                </div>
              );
            })
          )}

          {/* 思考中/执行中指示器（仅在没有流式消息时显示） */}
          {isThinking && !streamingMessageId && (
            <div className="flex gap-3 justify-start">
              <div className="w-8 h-8 rounded bg-[#10A37F] flex items-center justify-center flex-shrink-0">
                <Bot className="w-5 h-5 text-white" />
              </div>
              <div className="bg-[#2D2D30] text-[#F3F4F6] rounded-lg px-4 py-2.5">
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
