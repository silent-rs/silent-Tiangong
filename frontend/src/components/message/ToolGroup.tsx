import {
  Brain,
  ChevronDown,
  ChevronRight,
  Cpu,
  FilePenLine,
  FileText,
  Globe,
  Plug,
  Search,
  SquareTerminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import type { MessageItem } from "./types";
import { formatMessageTime, summarizeToolGroup } from "./utils";
import type { ExpansionState } from "./useExpansionState";
import {
  buildRunningToolModel,
  buildToolDisplayModel,
  writeContentFromArgs,
  type ToolDisplayModel,
  type ToolVariant,
} from "./toolDisplayModel";
import { TerminalCard } from "./toolViews/TerminalCard";
import { NumberedFileCard } from "./toolViews/NumberedFileCard";
import { InOutCard, WritePreviewCard } from "./toolViews/InOutCard";

/** 运行中工具调用（结果未到达）。 */
export interface RunningToolCall {
  id: string;
  name: string;
  arguments?: unknown;
}

interface ToolGroupProps {
  tools: MessageItem[];
  expansion: ExpansionState;
  /** 按消息取配对的调用参数（assistant.tool_calls 按 tool_call_id 配对）。 */
  argsOf?: (msg: MessageItem) => unknown;
  /** 本组之后仍在执行、尚无结果的工具调用，渲染为组尾运行行。 */
  runningCalls?: RunningToolCall[];
}

const VARIANT_ICONS: Record<ToolVariant, LucideIcon> = {
  terminal: SquareTerminal,
  "file-read": FileText,
  "file-write": FilePenLine,
  search: Search,
  web: Globe,
  memory: Brain,
  plugin: Plug,
  other: Wrench,
};

/** 变体的展开体：terminal → 终端卡；读文件 → 行号内容块；写文件 → 内容预览；其余 → IN/OUT。 */
function ToolCardBody({ model, args }: { model: ToolDisplayModel; args?: unknown }) {
  if (model.variant === "terminal" && model.terminal) {
    return <TerminalCard terminal={model.terminal} />;
  }
  if (model.variant === "file-read" && model.outputText) {
    return <NumberedFileCard content={model.outputText} title={model.filePath} />;
  }
  if (model.variant === "file-write") {
    return (
      <WritePreviewCard
        path={model.filePath}
        content={writeContentFromArgs(args)}
        outputText={model.outputText}
      />
    );
  }
  return (
    <InOutCard
      argsText={model.argsText}
      outputText={model.outputText}
      isError={model.state === "error"}
    />
  );
}

/** 该调用是否有详情卡片可展开。 */
function isExpandable(model: ToolDisplayModel): boolean {
  if (model.state === "running") return false;
  if (model.variant === "terminal") return !!model.terminal?.command || !!model.terminal?.stdout || !!model.terminal?.stderr;
  if (model.variant === "file-read") return !!model.outputText;
  return model.argsText !== null || model.outputText !== null;
}

/**
 * 单条工具行：图标 + 类别 + 摘要，点击行展开/收起详情卡片。
 * 失败行摘要为错误首行（错误色）；运行行带扫光动画、不可展开。
 */
function ToolRunRow({
  model,
  args,
  expanded = false,
  onToggle,
  time,
}: {
  model: ToolDisplayModel;
  args?: unknown;
  expanded?: boolean;
  onToggle?: () => void;
  time?: string;
}) {
  const Icon = VARIANT_ICONS[model.variant];
  const isError = model.state === "error";
  const isRunning = model.state === "running";
  const summaryText = model.errorSummary ?? model.summary;
  const expandable = isExpandable(model);
  const open = expandable && expanded;

  return (
    <div title={time}>
      <button
        type="button"
        className={`w-full flex items-center gap-2 px-2 py-0.5 rounded text-xs transition-colors text-left ${
          isRunning
            ? "tool-run-row text-foreground/90"
            : "text-muted-foreground hover:bg-muted/50"
        }`}
        onClick={expandable ? onToggle : undefined}
      >
        {open ? (
          <ChevronDown className="w-3 h-3 shrink-0" />
        ) : (
          <ChevronRight className={`w-3 h-3 shrink-0 ${expandable ? "" : "invisible"}`} />
        )}
        <Icon className={`w-3 h-3 shrink-0 ${isError ? "text-destructive" : ""}`} />
        <span className="font-medium shrink-0">{model.title}</span>
        {summaryText && (
          <span
            className={`truncate ${
              isError ? "text-destructive" : "opacity-60"
            }`}
          >
            {summaryText}
          </span>
        )}
      </button>
      {open && (
        <div className="ml-5 mt-0.5 mb-1">
          <ToolCardBody model={model} args={args} />
        </div>
      )}
    </div>
  );
}

export function ToolGroup({ tools, expansion, argsOf, runningCalls }: ToolGroupProps) {
  if (tools.length === 0 && !runningCalls?.length) return null;
  const brief = summarizeToolGroup(tools);
  const groupTime = tools.length > 0 ? formatMessageTime(tools[0].created_at) : undefined;

  const renderToolItem = (tool: MessageItem) => {
    const args = argsOf?.(tool);
    return (
      <ToolRunRow
        key={tool.id}
        model={buildToolDisplayModel(tool, args)}
        args={args}
        expanded={expansion.isExpanded(tool.id)}
        onToggle={() => expansion.toggle(tool.id)}
        time={formatMessageTime(tool.created_at)}
      />
    );
  };

  return (
    <div title={groupTime}>
      {/* 组统计为静态摘要行：不再折叠包裹工具行，工具行始终直接列出。 */}
      <div className="flex items-center gap-2 px-2 py-0.5 text-xs text-muted-foreground">
        <Cpu className="w-3 h-3 shrink-0" />
        <span>{brief}</span>
      </div>
      <div className="ml-4 space-y-0">{tools.map(renderToolItem)}</div>
      {runningCalls?.map((call) => (
        <div key={`running-${call.id}`} className="ml-4">
          <ToolRunRow model={buildRunningToolModel(call.name, call.arguments)} />
        </div>
      ))}
    </div>
  );
}
