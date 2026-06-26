import { useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';

interface ThinkingBlockProps {
  content: string;
  /** 是否处于活跃（推理/流式）态。活跃时默认展开并实时计时；结束后自动收起为一行。 */
  isActive?: boolean;
  /** 推理总时长（毫秒）。结束后用于展示「推理总时长」，未提供时回退到组件自身计时。 */
  durationMs?: number | null;
  defaultExpanded?: boolean;
}

/**
 * 将毫秒格式化为人类可读的时长：小于 1s 显示 ms，否则显示 s（保留 1 位小数）。
 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function ThinkingBlock({ content, isActive = false, durationMs, defaultExpanded }: ThinkingBlockProps) {
  // 推理完成后默认收起为一行摘要；活跃态保持展开（便于用户跟随推理过程）。
  const [isExpanded, setIsExpanded] = useState(defaultExpanded ?? isActive);
  // 本地实时计时器：仅在活跃态且未提供外部 durationMs 时启用。
  const [elapsedMs, setElapsedMs] = useState(0);
  const startRef = useRef<number | null>(null);

  useEffect(() => {
    if (!isActive) return;
    startRef.current = performance.now();
    const timer = window.setInterval(() => {
      if (startRef.current != null) {
        setElapsedMs(performance.now() - startRef.current);
      }
    }, 100);
    return () => window.clearInterval(timer);
  }, [isActive]);

  // 推理结束（isActive 由 true 变 false）时，自动收起为一行摘要。
  useEffect(() => {
    if (!isActive) {
      setIsExpanded(false);
    }
  }, [isActive]);

  const displayDuration = durationMs != null ? durationMs : elapsedMs;

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
        {displayDuration > 0 && (
          <span className="opacity-70 tabular-nums">· {formatDuration(displayDuration)}</span>
        )}
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
