import { useState } from "react";
import { ChevronRight, ChevronDown, Cpu } from "lucide-react";
import { textContent } from "@/api/tauri";
import type { MessageItem } from "./types";
import { formatMessageTime, getToolMessageMeta, summarizeToolGroup } from "./utils";
import type { ExpansionState } from "./useExpansionState";

/**
 * 单条工具输出在前端截取的最大字符数。
 * 超过阈值时默认折叠为摘要预览，并提供「展开全部」入口，
 * 避免超长输出（大文件读取、冗长命令回显）撑爆渲染、卡顿 UI。
 */
const TOOL_OUTPUT_PREVIEW_LIMIT = 500;

export function ToolGroup({ tools, expansion }: { tools: MessageItem[]; expansion: ExpansionState }) {
  if (tools.length === 0) return null;
  const key = tools[0].id;
  const brief = summarizeToolGroup(tools);
  const collapsed = !expansion.isExpanded(key);
  const groupTime = formatMessageTime(tools[0].created_at);

  const renderToolItem = (tool: MessageItem) => {
    const meta = getToolMessageMeta(tool);
    const expanded = expansion.isExpanded(tool.id);
    return (
      <div key={tool.id} title={formatMessageTime(tool.created_at)}>
        <button className="w-full flex items-center gap-2 px-2 py-0.5 rounded text-xs text-muted-foreground hover:bg-muted/50 transition-colors text-left" onClick={() => expansion.toggle(tool.id)}>
          {expanded ? <ChevronDown className="w-3 h-3 shrink-0" /> : <ChevronRight className="w-3 h-3 shrink-0" />}
          <meta.icon className="w-3 h-3 shrink-0" />
          <span className="font-medium">{meta.label}</span>
          {!expanded && <span className="truncate opacity-60">{meta.summary}</span>}
        </button>
        {expanded && (
          <div className="ml-5 mt-0.5 px-3 py-2 rounded-md bg-muted/30 border border-border/50">
            <ToolOutput content={textContent(tool)} />
          </div>
        )}
      </div>
    );
  };

  return (
    <div title={groupTime}>
      <button className="flex items-center gap-2 px-2 py-0.5 text-xs text-muted-foreground hover:bg-muted/50 rounded transition-colors" onClick={() => expansion.toggle(key)}>
        {collapsed ? <ChevronRight className="w-3 h-3 shrink-0" /> : <ChevronDown className="w-3 h-3 shrink-0" />}
        <Cpu className="w-3 h-3 shrink-0" />
        <span>{brief}</span>
      </button>
      {!collapsed && <div className="ml-4 space-y-0">{tools.map((t) => renderToolItem(t))}</div>}
    </div>
  );
}

/**
 * 工具输出渲染：超长内容在前端截取为预览，避免一次性渲染超大字符串卡顿。
 * 默认仅展示 `TOOL_OUTPUT_PREVIEW_LIMIT` 字符的预览，点击「展开全部」回看完整内容。
 */
function ToolOutput({ content }: { content: string }) {
  const [showAll, setShowAll] = useState(false);
  const overLimit = content.length > TOOL_OUTPUT_PREVIEW_LIMIT;
  const preview = overLimit && !showAll ? content.slice(0, TOOL_OUTPUT_PREVIEW_LIMIT) : content;

  return (
    <div className="space-y-1">
      <pre className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">{preview}</pre>
      {overLimit && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          className="text-xs text-primary hover:underline"
        >
          {showAll
            ? "收起"
            : `展开全部（${(content.length / 1000).toFixed(1)}k 字符，已截取前 ${TOOL_OUTPUT_PREVIEW_LIMIT}）`}
        </button>
      )}
    </div>
  );
}
