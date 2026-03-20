import { useState } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';
import ReactMarkdown from 'react-markdown';

interface ThinkingBlockProps {
  content: string;
  defaultExpanded?: boolean;
}

export function ThinkingBlock({ content, defaultExpanded = false }: ThinkingBlockProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);

  return (
    <div className="mt-4 rounded-lg border border-[#3C3C3C] bg-gradient-to-br from-[#1A1A1A] to-[#252526] overflow-hidden">
      {/* 标题栏 */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="w-full px-4 py-2.5 flex items-center justify-between hover:bg-[#2A2D2E] transition-colors duration-200"
      >
        <div className="flex items-center gap-2">
          <Brain className="w-4 h-4 text-[#9CA3AF]" />
          <span className="text-sm font-medium text-[#9CA3AF]">深度思考</span>
          <span className="text-xs text-[#6B7280]">
            ({content.length} 字符)
          </span>
        </div>
        {isExpanded ? (
          <ChevronDown className="w-4 h-4 text-[#6B7280] transition-transform duration-200" />
        ) : (
          <ChevronRight className="w-4 h-4 text-[#6B7280] transition-transform duration-200" />
        )}
      </button>

      {/* 内容区域 */}
      {isExpanded && (
        <div className="px-4 pb-4 pt-2">
          <div className="rounded-md bg-[#0D0D0D] border border-[#2A2D2E] p-3">
            <div className="prose prose-invert prose-sm max-w-none">
              <ReactMarkdown
                components={{
                  p: (props: any) => (
                    <p className="text-[#858585] mb-2 last:mb-0 leading-relaxed">
                      {props.children}
                    </p>
                  ),
                  ul: (props: any) => (
                    <ul className="list-disc list-inside mb-2 text-[#858585] space-y-1">
                      {props.children}
                    </ul>
                  ),
                  ol: (props: any) => (
                    <ol className="list-decimal list-inside mb-2 text-[#858585] space-y-1">
                      {props.children}
                    </ol>
                  ),
                  code: (props: any) => (
                    <code className="bg-[#1E1E1E] text-[#A78BFA] px-1.5 py-0.5 rounded text-xs font-mono">
                      {props.children}
                    </code>
                  ),
                  strong: (props: any) => (
                    <strong className="text-[#A78BFA] font-semibold">
                      {props.children}
                    </strong>
                  ),
                } as any}
              >
                {content}
              </ReactMarkdown>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
