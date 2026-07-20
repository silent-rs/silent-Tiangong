import { memo, useEffect, useState } from "react";
import { MdPreview } from "md-editor-rt";
import { FileText, ChevronRight, ChevronDown } from "lucide-react";
import { useSearchStore } from "@/store/useSearchStore";
import { useStore } from "@/store/useStore";
import { findTextOccurrences } from "@/utils/search";
import { HighlightText } from "../HighlightText";
import { ThinkingBlock } from "../ThinkingBlock";
import { AgentReplyCard } from "./AgentReplyCard";
import { useResolvedTheme } from "@/hooks/useTheme";
import { hasMediaBlocks, textContent } from "@/api/tauri";
import {
  formatMessageTime,
  msgReasoning,
  resolveMarkdownImages,
  extractLlmExplanation,
  llmOutputHasToolCalls,
  sameMessageRefs,
  hasMessage,
  extractAgentRoles,
  parseAgentReply,
  displayTextContent,
  stripSummaryStatusMarker,
  isNeedMoreWorkMessage,
} from "./utils";
import type { MessageItem } from "./types";
import { useExpansionState } from "./useExpansionState";
import { StreamingMessage } from "./StreamingMessage";
import { MessageActions } from "./MessageActions";
import { ContentMedia } from "./ContentMedia";
import { ToolGroup } from "./ToolGroup";

interface AgentTurnProps {
  messages: MessageItem[];
  streamingMessageId: string | null;
  streamingContent: string;
  streamingReasoningContent: string;
  hasTts: boolean;
  selectedAgentTab: string | null;
  isActive?: boolean;
}

/** 将毫秒格式化为人类可读时长：< 1s 显示 ms，否则显示 s（保留 1 位小数）。 */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

/** 轮次状态对应的中文标签与颜色（失败/取消直观醒目，成功保持低调）。 */
const TURN_STATUS_META: Record<string, { label: string; className: string; dot: string }> = {
  failed: { label: "失败", className: "text-destructive", dot: "bg-destructive" },
  cancelled: { label: "已取消", className: "text-muted-foreground", dot: "bg-muted-foreground" },
};

function AgentTurnView({
  messages,
  streamingMessageId,
  streamingContent,
  streamingReasoningContent,
  hasTts,
  selectedAgentTab,
  isActive = false,
}: AgentTurnProps) {
  const searchQuery = useSearchStore((s) => s.searchQuery);
  const currentMessageId = useSearchStore((s) => s.currentMessageId);
  const currentMatchStart = useSearchStore((s) => s.currentMatchStart);
  const caseSensitive = useSearchStore((s) => s.caseSensitive);
  const toolGroupExpansion = useExpansionState(isActive);
  const agents = useStore((state) => state.agents);
  const resolvedTheme = useResolvedTheme();

  // 已完成轮次默认折叠「过程」（思考/解释/工具/ReAct 文本等），仅保留总结回复可见，
  // 使历史会话整洁；用户点击摘要行可展开查看完整过程。活跃轮次保持展开。
  const [showProcess, setShowProcess] = useState(isActive);
  // 活跃态结束（isActive: true→false）时自动折叠过程，避免完成后仍展开堆叠。
  useEffect(() => {
    setShowProcess(isActive);
  }, [isActive]);

  const renderWithHighlight = (msgId: string, text: string) => {
    if (!searchQuery) return text;
    const occurrences = findTextOccurrences(text, searchQuery, caseSensitive);
    if (occurrences.length === 0) return text;
    const isCurrent = msgId === currentMessageId;
    return <HighlightText text={text} matches={occurrences} currentMatchStart={isCurrent ? currentMatchStart : null} />;
  };

  type Fragment =
    | { type: "explanation"; text: string; time?: string }
    | { type: "thinking"; content: string; time?: string }
    | { type: "tool_group"; key: string; tools: MessageItem[] }
    | { type: "user"; msg: MessageItem }
    | { type: "assistant"; msg: MessageItem; isStreaming: boolean }
    | { type: "error_system"; msg: MessageItem }
    | { type: "retry_system"; msg: MessageItem }
    | { type: "context_management"; msg: MessageItem }
    | { type: "agent_event"; category: string; content: string; agentRoles: string[] }
    | { type: "other_system"; msg: MessageItem };

  const fragments: Fragment[] = [];
  const shownReasonings = new Set<string>();
  let pendingTools: MessageItem[] = [];

  const flushTools = () => {
    if (pendingTools.length === 0) return;
    fragments.push({ type: "tool_group", key: pendingTools[0].id, tools: [...pendingTools] });
    pendingTools = [];
  };

  for (const msg of messages) {
    if (msg.role === "user") {
      flushTools();
      fragments.push({ type: "user", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[记忆检索] 策略:")) {
      pendingTools.push(msg);
    } else if (msg.role === "system" && textContent(msg).startsWith("[记忆检索]")) {
      pendingTools.push(msg);
    } else if (msg.role === "system" && textContent(msg).startsWith("LLM 输出")) {
      const reasoning = msgReasoning(msg);
      const explanation = extractLlmExplanation(textContent(msg));
      if (!reasoning && !explanation && llmOutputHasToolCalls(textContent(msg))) continue;
      flushTools();
      if (reasoning && !shownReasonings.has(reasoning)) {
        shownReasonings.add(reasoning);
        fragments.push({ type: "thinking", content: reasoning, time: msg.created_at });
      }
      if (explanation) fragments.push({ type: "explanation", text: explanation, time: msg.created_at });
    } else if (msg.role === "system" && (textContent(msg).includes("tool_name:") || textContent(msg).includes("exit_code") || textContent(msg).startsWith("工具执行 ["))) {
      pendingTools.push(msg);
    } else if (msg.role === "tool") {
      pendingTools.push(msg);
      continue;
    } else if (msg.role === "assistant") {
      const isStreaming = msg.id === streamingMessageId;
      const assistantReasoning = msgReasoning(msg);
      const hasVisibleAssistantContent = isStreaming || textContent(msg).trim().length > 0 || assistantReasoning.length > 0 || !!msg.media?.length || hasMediaBlocks(msg);
      if (!hasVisibleAssistantContent) continue;
      flushTools();
      const prevFrag = fragments[fragments.length - 1];
      if (prevFrag?.type === "explanation" && prevFrag.text === textContent(msg).trim() && !isStreaming) fragments.pop();
      if (!isStreaming && assistantReasoning && !shownReasonings.has(assistantReasoning)) {
        shownReasonings.add(assistantReasoning);
        fragments.push({ type: "thinking", content: assistantReasoning, time: msg.created_at });
      }
      // 总结阶段判定"任务未完成、需重入 Loop"的回复（[NEED_MORE_WORK] 标头）：
      // 前端作为思考过程展示，剥除标头，不作为最终回复正文。
      if (isNeedMoreWorkMessage(msg)) {
        const needMoreWorkBody = stripSummaryStatusMarker(textContent(msg)).trim();
        if (needMoreWorkBody || isStreaming) {
          fragments.push({ type: "thinking", content: needMoreWorkBody, time: msg.created_at });
        }
        continue;
      }
      fragments.push({ type: "assistant", msg, isStreaming });
    } else if (msg.role === "system" && textContent(msg).startsWith("[错误]")) {
      flushTools();
      fragments.push({ type: "error_system", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[重试]")) {
      flushTools();
      fragments.push({ type: "retry_system", msg });
    } else if (msg.role === "system" && textContent(msg).startsWith("[上下文管理]")) {
      if (textContent(msg).includes("正在压缩")) continue;
      flushTools();
      fragments.push({ type: "context_management", msg });
    } else if (msg.role === "system" && (textContent(msg).startsWith("[Agent]") || textContent(msg).startsWith("[文件锁]"))) {
      flushTools();
      const category = textContent(msg).startsWith("[文件锁]") ? "lock" : "info";
      fragments.push({ type: "agent_event", category, content: textContent(msg), agentRoles: extractAgentRoles(textContent(msg), agents) });
    } else if (msg.role === "system") {
      flushTools();
      fragments.push({ type: "other_system", msg });
    }
  }
  flushTools();

  const mergedFragments: Fragment[] = [];
  for (const frag of fragments) {
    const previous = mergedFragments[mergedFragments.length - 1];
    if (frag.type === "tool_group" && previous?.type === "tool_group") {
      previous.tools.push(...frag.tools);
      continue;
    }
    mergedFragments.push(frag);
  }

  // 分组：用户消息（锚点）/ 过程片段（思考、解释、工具、ReAct 文本等）/ 总结回复。
  // 已完成轮次默认折叠「过程」仅保留总结可见；活跃轮次全部展示。
  // summaryFrags 收集同一轮次内全部非 react 的助手回复（含总结阶段产出），
  // 全部渲染而非仅取最后一条，避免遗漏或互相覆盖。
  let userFrag: Fragment | null = null;
  const summaryFrags: Fragment[] = [];
  const processFrags: Fragment[] = [];
  for (const frag of mergedFragments) {
    if (frag.type === "user") {
      userFrag = frag;
    } else if (frag.type === "assistant" && frag.msg.phase !== "react") {
      summaryFrags.push(frag);
    } else {
      processFrags.push(frag);
    }
  }

  const renderFragment = (frag: Fragment, i: number) => {
    if (selectedAgentTab && frag.type !== "agent_event") return null;
    if (selectedAgentTab && frag.type === "agent_event" && frag.agentRoles.length > 0 && !frag.agentRoles.includes(selectedAgentTab)) return null;
    if (frag.type === "thinking") {
      // 历史/已完成思考块一律视为非活跃且默认折叠。
      return <div key={`think-${i}`} title={formatMessageTime(frag.time)}><ThinkingBlock content={frag.content} isActive={false} defaultExpanded={false} /></div>;
    }
        if (frag.type === "explanation") {
          return <p key={`expl-${i}`} className="text-sm text-muted-foreground leading-relaxed whitespace-pre-wrap break-words" title={formatMessageTime(frag.time)}>{frag.text}</p>;
        }
        if (frag.type === "tool_group") {
          return <ToolGroup key={`tools-${frag.key}`} tools={frag.tools} expansion={toolGroupExpansion} />;
        }
        if (frag.type === "user") {
          const statusMeta = frag.msg.turn_status ? TURN_STATUS_META[frag.msg.turn_status] : null;
          return (
            <div key={frag.msg.id} className="flex flex-col items-end gap-0.5" title={formatMessageTime(frag.msg.created_at)}>
              <div className="flex justify-end">
                <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-sm text-foreground whitespace-pre-wrap break-words">{textContent(frag.msg)}</div>
              </div>
              {(frag.msg.elapsed_ms != null || statusMeta) && (
                <div className="flex items-center gap-1.5 pr-1 text-[11px] text-muted-foreground/80 tabular-nums">
                  {statusMeta && (
                    <span className={`inline-flex items-center gap-1 ${statusMeta.className}`}>
                      <span className={`inline-block w-1.5 h-1.5 rounded-full ${statusMeta.dot}`} />
                      {statusMeta.label}
                    </span>
                  )}
                  {frag.msg.elapsed_ms != null && <span>⏱ {formatDuration(frag.msg.elapsed_ms)}</span>}
                </div>
              )}
            </div>
          );
        }
        if (frag.type === "assistant") {
          const { msg, isStreaming } = frag;
          const visibleText = displayTextContent(msg);
          const isReactPhase = msg.phase === "react";
          // 流式输出无条件剥离状态标记（[DONE]/[NEED_MORE_WORK]/[ASK_USER] 等），
          // 即使后端 phase 尚未传播到也兜底，避免标记泄漏到界面。
          const visibleStreamingContent = stripSummaryStatusMarker(streamingContent);
          const agentReply = !isStreaming ? parseAgentReply(visibleText) : null;
          if (agentReply) {
            // Sub Agent 汇报：默认折叠为一行摘要，点击展开完整正文。
            // 搜索命中时强制展开，便于定位。
            const hasSearchHit = !!searchQuery && findTextOccurrences(visibleText, searchQuery, caseSensitive).length > 0;
            return (
              <div key={msg.id} className="py-0.5">
                <AgentReplyCard
                  label={agentReply.label}
                  body={hasSearchHit ? visibleText : agentReply.body}
                  time={msg.created_at}
                  defaultExpanded={hasSearchHit}
                />
              </div>
            );
          }
          // ReAct 工具执行阶段的过程性文本：紧凑展示，不提供复制按钮。
          if (isReactPhase) {
            const body = isStreaming ? visibleStreamingContent : visibleText;
            if (!body && !streamingReasoningContent) return null;
            return (
              <div key={msg.id} className="text-xs text-muted-foreground leading-relaxed whitespace-pre-wrap break-words" title={formatMessageTime(msg.created_at)}>
                {isStreaming ? (
                  <StreamingMessage content={body} reasoningContent={streamingReasoningContent} />
                ) : (
                  renderWithHighlight(msg.id, body)
                )}
              </div>
            );
          }
          return (
            <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
              {isStreaming ? (
                <StreamingMessage content={visibleStreamingContent} reasoningContent={streamingReasoningContent} />
              ) : visibleText || (msg.media && msg.media.length > 0) || hasMediaBlocks(msg) ? (
                <div>
                  <ContentMedia message={msg} />
                  {searchQuery && findTextOccurrences(visibleText, searchQuery, caseSensitive).length > 0
                    ? <div className="text-sm whitespace-pre-wrap break-words">{renderWithHighlight(msg.id, visibleText)}</div>
                    : <MdPreview modelValue={resolveMarkdownImages(visibleText)} theme={resolvedTheme} previewTheme="github" />}
                </div>
              ) : null}
              {!isStreaming && msg.content && visibleText && (
                <div className="mt-1 border-t border-border/50 pt-1">
                  <MessageActions text={visibleText} showTts={hasTts} durationMs={!isActive ? userFrag?.msg.elapsed_ms : undefined} />
                </div>
              )}
            </div>
          );
        }
        if (frag.type === "error_system") {
          return <div key={frag.msg.id} className="text-sm text-destructive bg-destructive/10 rounded-md px-3 py-2 my-1">{textContent(frag.msg).replace("[错误] ", "")}</div>;
        }
        if (frag.type === "retry_system") {
          return <div key={frag.msg.id} className="text-xs text-yellow-600 dark:text-yellow-400 bg-yellow-500/10 rounded-md px-3 py-1.5 my-0.5">{textContent(frag.msg).replace("[重试] ", "")}</div>;
        }
        if (frag.type === "context_management") {
          const text = textContent(frag.msg).replace("[上下文管理] ", "");
          return (
            <div key={frag.msg.id} className="inline-flex max-w-full items-center gap-2 rounded-md border border-border/70 bg-muted/30 px-2.5 py-1 text-xs text-muted-foreground" title={formatMessageTime(frag.msg.created_at)}>
              <FileText className="h-3.5 w-3.5 shrink-0" />
              <span className="truncate">{text}</span>
            </div>
          );
        }
        if (frag.type === "agent_event") {
          const colorMap: Record<string, string> = { lock: "border-yellow-500/30 bg-yellow-500/5", info: "border-border bg-muted/30" };
          return <div key={`agent-${i}`} className={`text-xs text-muted-foreground border rounded px-2 py-1 my-0.5 ${colorMap[frag.category] || colorMap.info}`}>{frag.content}</div>;
        }
        if (frag.type === "other_system") {
          return <p key={frag.msg.id} className="text-xs text-muted-foreground">{textContent(frag.msg).split("\n")[0]}</p>;
        }
        return null;
  };

  // 过程片段计数：用于折叠态摘要文案（思考/工具条数）。
  const processStats = {
    thinking: processFrags.filter((f) => f.type === "thinking").length,
    tools: processFrags.filter((f) => f.type === "tool_group").reduce((acc, f) => acc + (f.type === "tool_group" ? f.tools.length : 0), 0),
  };
  const collapseProcess = !isActive && processFrags.length > 0;

  return (
    <div className="space-y-1.5">
      {userFrag && renderFragment(userFrag, 0)}
      {collapseProcess && !showProcess && (
        <button
          type="button"
          onClick={() => setShowProcess(true)}
          className="inline-flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors py-0.5"
        >
          <ChevronRight className="w-3 h-3" />
          <span>展开过程</span>
          <span className="opacity-60">
            （{processStats.thinking > 0 ? `思考 ${processStats.thinking}·` : ""}{processStats.tools > 0 ? `工具 ${processStats.tools}` : ""}{processStats.thinking === 0 && processStats.tools === 0 ? `${processFrags.length} 条` : ""}）
          </span>
        </button>
      )}
      {(!collapseProcess || showProcess) && processFrags.length > 0 && (
        <div className="space-y-1.5">
          {processFrags.map((frag, i) => renderFragment(frag, i))}
          {collapseProcess && showProcess && (
            <button
              type="button"
              onClick={() => setShowProcess(false)}
              className="inline-flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors py-0.5"
            >
              <ChevronDown className="w-3 h-3" />
              <span>收起过程</span>
            </button>
          )}
        </div>
      )}
      {summaryFrags.map((frag, i) => renderFragment(frag, mergedFragments.length + i))}
    </div>
  );
}

const AgentTurn = memo(AgentTurnView, (prev, next) => {
  if (prev.hasTts !== next.hasTts || !sameMessageRefs(prev.messages, next.messages) || prev.selectedAgentTab !== next.selectedAgentTab || prev.isActive !== next.isActive) return false;
  const touchesStreamingMessage = hasMessage(prev.messages, prev.streamingMessageId) || hasMessage(prev.messages, next.streamingMessageId);
  if (!touchesStreamingMessage) return true;
  return prev.streamingMessageId === next.streamingMessageId && prev.streamingContent === next.streamingContent && prev.streamingReasoningContent === next.streamingReasoningContent;
});

export { AgentTurn, AgentTurnView };
