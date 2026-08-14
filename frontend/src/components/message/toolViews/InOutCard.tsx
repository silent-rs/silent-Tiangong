import { LongContent } from "./LongContent";

/** 小节标签（IN / OUT / 写入内容）统一样式。 */
function SectionLabel({ children }: { children: string }) {
  return (
    <div className="text-[10px] font-semibold tracking-wider text-muted-foreground/70 uppercase mb-1">
      {children}
    </div>
  );
}

/**
 * 参数 / 结果分区卡片：通用工具的展开体。
 * IN 段为调用参数，OUT 段为工具输出，两者独立截断。
 */
export function InOutCard({
  argsText,
  outputText,
  isError,
}: {
  argsText: string | null;
  outputText: string | null;
  isError?: boolean;
}) {
  if (!argsText && !outputText) return null;
  return (
    <div className="rounded-md border border-border/60 bg-muted/30 px-3 py-2 space-y-2">
      {argsText && (
        <div>
          <SectionLabel>IN</SectionLabel>
          <LongContent
            content={argsText}
            className="text-xs font-mono text-muted-foreground whitespace-pre-wrap break-words max-h-40 overflow-y-auto"
          />
        </div>
      )}
      {outputText && (
        <div>
          <SectionLabel>OUT</SectionLabel>
          <LongContent
            content={outputText}
            className={`text-xs font-mono whitespace-pre-wrap break-words max-h-56 overflow-y-auto ${
              isError ? "text-destructive" : "text-foreground/85"
            }`}
          />
        </div>
      )}
    </div>
  );
}

/**
 * 写入文件卡片：目标路径标题 + 写入内容预览 + 工具结果确认。
 * 后端不产出 diff，以内容预览替代（需求文档已列为非目标）。
 */
export function WritePreviewCard({
  path,
  content,
  outputText,
}: {
  path: string | null;
  content: string | null;
  outputText: string | null;
}) {
  return (
    <div className="space-y-2">
      {content && (
        <div className="rounded-md border border-border/60 bg-muted/30 overflow-hidden">
          <div className="px-3 py-1.5 border-b border-border/50 text-xs font-mono text-muted-foreground truncate">
            {path || "写入内容"}
          </div>
          <LongContent
            content={content}
            className="px-3 py-2 text-xs font-mono text-foreground/90 whitespace-pre-wrap break-words max-h-64 overflow-y-auto"
          />
        </div>
      )}
      {outputText && (
        <div className="text-xs font-mono text-muted-foreground whitespace-pre-wrap break-words">
          {outputText.split("\n")[0]}
        </div>
      )}
    </div>
  );
}
