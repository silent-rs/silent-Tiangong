import { useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import { CopyableCodeBlock } from './CopyableCodeBlock';

interface ThinkingBlockProps {
  content: string;
  defaultExpanded?: boolean;
}

export function ThinkingBlock({ content, defaultExpanded = false }: ThinkingBlockProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);

  return (
    <div className="mb-2">
      {/* 标题栏 — 轻量内联样式 */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
      >
        {isExpanded ? (
          <ChevronDown className="w-3 h-3" />
        ) : (
          <ChevronRight className="w-3 h-3" />
        )}
        <span>深度思考</span>
        <span className="opacity-50">({content.length} 字符)</span>
      </button>

      {/* 内容区域 — 左侧竖线分隔 */}
      {isExpanded && (
        <div className="ml-1.5 mt-1 pl-3 border-l-2 border-muted-foreground/20">
          <div className="prose prose-sm max-w-none">
            <ReactMarkdown
              components={{
                p: (props: any) => (
                  <p className="text-muted-foreground text-xs mb-1.5 last:mb-0 leading-relaxed">
                    {props.children}
                  </p>
                ),
                ul: (props: any) => (
                  <ul className="list-disc list-inside mb-1.5 text-muted-foreground text-xs space-y-0.5">
                    {props.children}
                  </ul>
                ),
                ol: (props: any) => (
                  <ol className="list-decimal list-inside mb-1.5 text-muted-foreground text-xs space-y-0.5">
                    {props.children}
                  </ol>
                ),
                pre: (props: any) => <>{props.children}</>,
                code: (props: any) => {
                  const match = /language-(\w+)/.exec(props.className || '');
                  const isBlock = match || props.node?.parentNode?.tagName === 'pre';
                  if (isBlock) {
                    return (
                      <CopyableCodeBlock
                        code={String(props.children).replace(/\n$/, '')}
                        language={match?.[1] || 'text'}
                      />
                    );
                  }
                  return (
                    <code className="bg-muted text-foreground/70 px-1 py-0.5 rounded text-xs font-mono">
                      {props.children}
                    </code>
                  );
                },
                strong: (props: any) => (
                  <strong className="text-foreground/80 font-semibold">
                    {props.children}
                  </strong>
                ),
              } as any}
            >
              {content}
            </ReactMarkdown>
          </div>
        </div>
      )}
    </div>
  );
}
