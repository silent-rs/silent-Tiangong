import { memo } from "react";
import { MdPreview } from "md-editor-rt";
import { FileText } from "lucide-react";
import { useSearchStore } from "@/store/useSearchStore";
import { useStore } from "@/store/useStore";
import { findTextOccurrences } from "@/utils/search";
import { HighlightText } from "../HighlightText";
import { ThinkingBlock } from "../ThinkingBlock";
import { useResolvedTheme } from "@/hooks/useTheme";
import { textContent } from "@/api/tauri";
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
      const hasVisibleAssistantContent = isStreaming || textContent(msg).trim().length > 0 || assistantReasoning.length > 0 || !!msg.media?.length || msg.content.some((b) => b.type === "media");
      if (!hasVisibleAssistantContent) continue;
      flushTools();
      const prevFrag = fragments[fragments.length - 1];
      if (prevFrag?.type === "explanation" && prevFrag.text === textContent(msg).trim() && !isStreaming) fragments.pop();
      if (!isStreaming && assistantReasoning && !shownReasonings.has(assistantReasoning)) {
        shownReasonings.add(assistantReasoning);
        fragments.push({ type: "thinking", content: assistantReasoning, time: msg.created_at });
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

  return (
    <div className="space-y-1.5">
      {mergedFragments.map((frag, i) => {
        if (selectedAgentTab && frag.type !== "agent_event") return null;
        if (selectedAgentTab && frag.type === "agent_event" && frag.agentRoles.length > 0 && !frag.agentRoles.includes(selectedAgentTab)) return null;
        if (frag.type === "thinking") {
          return <div key={`think-${i}`} title={formatMessageTime(frag.time)}><ThinkingBlock content={frag.content} defaultExpanded={isActive} /></div>;
        }
        if (frag.type === "explanation") {
          return <p key={`expl-${i}`} className="text-sm text-muted-foreground leading-relaxed whitespace-pre-wrap break-words" title={formatMessageTime(frag.time)}>{frag.text}</p>;
        }
        if (frag.type === "tool_group") {
          return <ToolGroup key={`tools-${frag.key}`} tools={frag.tools} expansion={toolGroupExpansion} />;
        }
        if (frag.type === "user") {
          return (
            <div key={frag.msg.id} className="flex justify-end" title={formatMessageTime(frag.msg.created_at)}>
              <div className="max-w-[85%] rounded-2xl bg-primary/10 px-4 py-2.5 text-sm text-foreground whitespace-pre-wrap break-words">{textContent(frag.msg)}</div>
            </div>
          );
        }
        if (frag.type === "assistant") {
          const { msg, isStreaming } = frag;
          const agentReply = !isStreaming ? parseAgentReply(textContent(msg)) : null;
          if (agentReply) {
            return (
              <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
                <div className="inline-flex items-center gap-1.5 rounded-full border border-green-500/30 bg-green-500/10 px-2 py-0.5 text-xs font-medium text-green-700 dark:text-green-300 mb-1">
                  <span className="h-1.5 w-1.5 rounded-full bg-green-500" />
                  {agentReply.label}
                </div>
                <div className="border-l-2 border-green-500/50 pl-3">
                  {agentReply.body ? (
                    searchQuery && findTextOccurrences(textContent(msg), searchQuery, caseSensitive).length > 0
                      ? <div className="text-sm whitespace-pre-wrap break-words">{renderWithHighlight(msg.id, agentReply.body)}</div>
                      : <MdPreview modelValue={resolveMarkdownImages(agentReply.body)} theme={resolvedTheme} previewTheme="github" />
                  ) : null}
                </div>
                {agentReply.body && <MessageActions text={agentReply.body} showTts={hasTts} />}
              </div>
            );
          }
          return (
            <div key={msg.id} className="text-foreground" title={formatMessageTime(msg.created_at)}>
              {isStreaming ? (
                <StreamingMessage content={streamingContent} reasoningContent={streamingReasoningContent} />
              ) : textContent(msg) || (msg.media && msg.media.length > 0) || msg.content.some((b) => b.type === "media") ? (
                <div>
                  <ContentMedia message={msg} />
                  {searchQuery && findTextOccurrences(textContent(msg), searchQuery, caseSensitive).length > 0
                    ? <div className="text-sm whitespace-pre-wrap break-words">{renderWithHighlight(msg.id, textContent(msg))}</div>
                    : <MdPreview modelValue={resolveMarkdownImages(textContent(msg))} theme={resolvedTheme} previewTheme="github" />}
                </div>
              ) : null}
              {!isStreaming && msg.content && <MessageActions text={textContent(msg)} showTts={hasTts} />}
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
      })}
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
