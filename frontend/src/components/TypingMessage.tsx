import { useEffect, useState, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { vscDarkPlus } from 'react-syntax-highlighter/dist/esm/styles/prism';
import type { ReactNode } from 'react';

import { ThinkingBlock } from './ThinkingBlock';

interface TypingMessageProps {
  content: string;
  reasoningContent?: string; // 思考过程内容
  speed?: number; // 打字速度（每分钟字符数）
  onComplete?: () => void;
}

export function TypingMessage({ content, reasoningContent, speed = 300, onComplete }: TypingMessageProps) {
  const [displayedContent, setDisplayedContent] = useState('');
  const [isComplete, setIsComplete] = useState(false);
  const intervalRef = useRef<number | null>(null);
  const currentIndexRef = useRef(0);
  const prevContentRef = useRef('');

  useEffect(() => {
    // 判断内容是否只是在增长（流式追加）
    const isAppending = content.startsWith(prevContentRef.current) && prevContentRef.current.length > 0;
    prevContentRef.current = content;

    if (!isAppending) {
      // 内容完全不同，重置
      setDisplayedContent('');
      setIsComplete(false);
      currentIndexRef.current = 0;
    } else {
      // 内容是追加的，保持当前进度，取消完成状态
      setIsComplete(false);
    }

    // 如果内容为空，直接完成
    if (!content) {
      setIsComplete(true);
      onComplete?.();
      return;
    }

    // 计算每次更新的字符数
    const charsPerUpdate = Math.max(1, Math.floor(speed / 60)); // 每秒更新 60 次

    // 清除之前的 interval
    if (intervalRef.current) {
      clearInterval(intervalRef.current);
    }

    // 启动打字机效果
    intervalRef.current = setInterval(() => {
      const currentIndex = currentIndexRef.current;

      if (currentIndex >= content.length) {
        // 完成
        setIsComplete(true);
        if (intervalRef.current) {
          clearInterval(intervalRef.current);
          intervalRef.current = null;
        }
        onComplete?.();
        return;
      }

      // 计算本次要显示的字符数
      const nextIndex = Math.min(currentIndex + charsPerUpdate, content.length);
      const newContent = content.slice(0, nextIndex);

      setDisplayedContent(newContent);
      currentIndexRef.current = nextIndex;
    }, 1000 / 60); // 60 FPS

    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [content, speed, onComplete]);

  // Markdown 渲染器
  const MarkdownComponents = {
    code({ className, children, ...rest }: any) {
      const match = /language-(\w+)/.exec(className || '');
      const CodeHighlighter = SyntaxHighlighter as any;
      return match ? (
        <CodeHighlighter
          style={vscDarkPlus}
          language={match[1]}
          PreTag="div"
          className="rounded-md text-xs !bg-background border border-border"
          customStyle={{ padding: '12px', borderRadius: '6px', margin: '6px 0' }}
          codeTagProps={{ style: {} }}
          {...rest}
        >
          {String(children).replace(/\n$/, '')}
        </CodeHighlighter>
      ) : (
        <code className={className || "bg-muted text-foreground px-1 py-0.5 rounded text-xs font-mono"} {...rest}>
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
    <div>
      {reasoningContent && (
        <ThinkingBlock content={reasoningContent} defaultExpanded={false} />
      )}

      <div className="prose prose-invert prose-sm max-w-none text-[13px]">
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={MarkdownComponents as any}>
          {displayedContent}
        </ReactMarkdown>
        {!isComplete && (
          <span className="inline-block w-2 h-4 bg-[#10A37F] ml-1 animate-pulse" />
        )}
      </div>
    </div>
  );
}
