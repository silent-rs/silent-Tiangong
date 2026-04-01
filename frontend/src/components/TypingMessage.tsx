import { useEffect, useState, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkBreaks from 'remark-breaks';
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

export function TypingMessage({ content, reasoningContent, speed: _speed = 300, onComplete }: TypingMessageProps) {
  // 流式模式：直接渲染到达的完整内容，不使用打字机效果
  // 流式输出本身就是逐步到达的，无需前端额外逐字追加
  const [isComplete, setIsComplete] = useState(false);
  const prevContentRef = useRef('');

  useEffect(() => {
    if (!content) {
      setIsComplete(true);
      onComplete?.();
      return;
    }

    // 内容在增长 → 流式进行中
    if (content !== prevContentRef.current) {
      prevContentRef.current = content;
      setIsComplete(false);
    }
  }, [content, onComplete]);

  // Markdown 渲染器
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

      <div className="prose prose-sm max-w-none text-[13px] text-foreground prose-p:text-foreground prose-li:text-foreground prose-strong:text-foreground prose-headings:text-foreground prose-a:text-blue-400 prose-blockquote:text-foreground/80 prose-code:text-foreground">
        <ReactMarkdown remarkPlugins={[remarkGfm, remarkBreaks]} components={MarkdownComponents as any}>
          {content}
        </ReactMarkdown>
        {!isComplete && content.length > 0 && (
          <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse align-text-bottom" />
        )}
      </div>
    </div>
  );
}
