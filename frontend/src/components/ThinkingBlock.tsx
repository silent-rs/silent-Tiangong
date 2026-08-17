import { useEffect, useState } from 'react';
import { ChevronDown, ChevronRight } from 'lucide-react';
import { formatDuration } from './message/utils';

interface ThinkingBlockProps {
  content: string;
  /** 是否处于活跃（推理/流式）态。活跃时默认展开，结束后自动收起为一行。 */
  isActive?: boolean;
  defaultExpanded?: boolean;
  /** 思考耗时（毫秒）：完成后使用后端持久化值；流式期间组件本地计时兜底。 */
  elapsedMs?: number | null;
}

export function ThinkingBlock({ content, isActive = false, defaultExpanded, elapsedMs }: ThinkingBlockProps) {
  // 推理完成后默认收起为一行摘要；活跃态保持展开（便于用户跟随推理过程）。
  const [isExpanded, setIsExpanded] = useState(defaultExpanded ?? isActive);
  // 流式思考中本地计时（从思考开始展示起算，与后端首增量计时基本一致）。
  // 结束后保留最后一次计时作为冻结值展示，待后端持久化值到达后被替换。
  const [liveMs, setLiveMs] = useState(0);

  useEffect(() => {
    if (!isActive) return;
    const startedAt = Date.now();
    setLiveMs(0);
    const timer = setInterval(() => setLiveMs(Date.now() - startedAt), 200);
    return () => clearInterval(timer);
  }, [isActive]);

  // 推理结束（isActive 由 true 变 false）时，自动收起为一行摘要。
  useEffect(() => {
    if (!isActive) {
      setIsExpanded(false);
    }
  }, [isActive]);

  const displayMs = elapsedMs ?? liveMs;

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
        {displayMs != null && displayMs > 0 && (
          <span className="tabular-nums">{formatDuration(displayMs)}</span>
        )}
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
