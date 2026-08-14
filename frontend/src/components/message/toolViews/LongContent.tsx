import { useState } from "react";

/**
 * 超长内容统一渲染：默认截取预览避免一次性渲染超大字符串卡顿，
 * 点击「展开全部」回看完整内容。所有工具展开卡片共用。
 */
export function LongContent({
  content,
  limit = 2000,
  className = "",
  as = "pre",
}: {
  content: string;
  limit?: number;
  className?: string;
  as?: "pre" | "div";
}) {
  const [showAll, setShowAll] = useState(false);
  const overLimit = content.length > limit;
  const preview = overLimit && !showAll ? content.slice(0, limit) : content;
  const Tag = as;

  return (
    <div className="min-w-0">
      <Tag className={className}>{preview}</Tag>
      {overLimit && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          className="mt-1 text-[11px] text-primary hover:underline"
        >
          {showAll
            ? "收起"
            : `展开全部（${(content.length / 1000).toFixed(1)}k 字符，已截取前 ${limit}）`}
        </button>
      )}
    </div>
  );
}
