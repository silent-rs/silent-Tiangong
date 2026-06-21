import { ChevronRight, ChevronDown, Cpu } from "lucide-react";
import { textContent } from "@/api/tauri";
import type { MessageItem } from "./types";
import { formatMessageTime, getToolMessageMeta, summarizeToolGroup } from "./utils";
import type { ExpansionState } from "./useExpansionState";

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
            <pre className="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">{textContent(tool)}</pre>
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
