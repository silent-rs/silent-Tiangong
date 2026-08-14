import { useState } from "react";

/** 文件内容块默认渲染的最大行数，超出截断防止大文件卡顿。 */
const MAX_LINES = 500;

/**
 * 带行号的文件内容块（read_file 结果展示）。
 * 行号列固定宽度、不可选中，内容等宽字体，行数多时截断并内部滚动。
 */
export function NumberedFileCard({ content, title }: { content: string; title?: string | null }) {
  const [showAll, setShowAll] = useState(false);
  const allLines = content.replace(/\n$/, "").split("\n");
  const overLimit = allLines.length > MAX_LINES;
  const lines = overLimit && !showAll ? allLines.slice(0, MAX_LINES) : allLines;

  return (
    <div className="rounded-md border border-border/60 bg-muted/30 overflow-hidden">
      {title && (
        <div className="px-3 py-1.5 border-b border-border/50 text-xs font-mono text-muted-foreground truncate">
          {title}
        </div>
      )}
      <div className="max-h-72 overflow-y-auto px-3 py-2 text-xs font-mono leading-5">
        {lines.map((line, i) => (
          <div key={i} className="flex gap-3 hover:bg-muted/40 rounded-sm">
            <span className="select-none text-muted-foreground/50 w-8 shrink-0 text-right tabular-nums">
              {i + 1}
            </span>
            <span className="whitespace-pre-wrap break-all min-w-0 flex-1 text-foreground/90">
              {line || " "}
            </span>
          </div>
        ))}
      </div>
      {overLimit && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          className="w-full px-3 py-1 text-[11px] text-primary hover:underline border-t border-border/50"
        >
          {showAll ? "收起" : `展开全部（共 ${allLines.length} 行，已截取前 ${MAX_LINES}）`}
        </button>
      )}
    </div>
  );
}
