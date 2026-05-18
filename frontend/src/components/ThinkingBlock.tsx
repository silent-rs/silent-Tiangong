import { useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';

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
          <pre className="whitespace-pre-wrap break-words text-xs leading-relaxed text-muted-foreground font-sans">
            {content}
          </pre>
        </div>
      )}
    </div>
  );
}
