import { textContent } from "@/api/tauri";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  Brain,
  Plug,
  Terminal,
  type LucideIcon,
} from "lucide-react";
import type { MessageGroup, MessageItem, SystemMessageMeta } from "./types";

/** 格式化消息时间戳 */
export function formatMessageTime(createdAt?: string): string {
  if (!createdAt) return "";
  try {
    const d = new Date(createdAt);
    if (isNaN(d.getTime())) return createdAt;
    const h = String(d.getHours()).padStart(2, "0");
    const m = String(d.getMinutes()).padStart(2, "0");
    return `${h}:${m}`;
  } catch {
    return createdAt;
  }
}

/** 将毫秒格式化为人类可读时长：< 1s 显示 ms；< 60s 显示 s（保留 1 位小数）；
 *  < 1h 显示「N分SS秒」；更长显示「N时MM分SS秒」。 */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = ms / 1000;
  if (totalSeconds < 60) return `${totalSeconds.toFixed(1)}s`;
  const total = Math.floor(totalSeconds);
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  if (hours > 0) {
    return `${hours}时${String(minutes).padStart(2, "0")}分${String(seconds).padStart(2, "0")}秒`;
  }
  return `${minutes}分${String(seconds).padStart(2, "0")}秒`;
}

export function msgReasoning(message: MessageItem): string {
  return (message.reasoning_content ?? "").trim();
}

/** 总结阶段的状态标记，需在前端展示时剥离。 */
const SUMMARY_STATUS_MARKERS = ["[DONE]", "[ASK_USER]", "[NEED_MORE_WORK]"];
const NEED_MORE_WORK_MARKER = "[NEED_MORE_WORK]";

/** 剥离总结阶段回复首行的状态标记（[DONE]/[ASK_USER]/[NEED_MORE_WORK]）。 */
export function stripSummaryStatusMarker(text: string): string {
  const trimmed = text.trimStart();
  for (const marker of SUMMARY_STATUS_MARKERS) {
    if (trimmed.slice(0, marker.length).toLowerCase() !== marker.toLowerCase()) continue;
    return trimmed.slice(marker.length).replace(/^[\s:：-]+/, "");
  }
  return text;
}

/** 判断消息是否为总结阶段判定"任务未完成、需重入 Loop"的回复（带 [NEED_MORE_WORK] 标头）。 */
export function isNeedMoreWorkMessage(message: MessageItem): boolean {
  return textContent(message).trimStart().slice(0, NEED_MORE_WORK_MARKER.length).toLowerCase() === NEED_MORE_WORK_MARKER.toLowerCase();
}

/**
 * 获取消息的可展示文本：无条件剥离总结阶段首行状态标记。
 *
 * `[DONE]`/`[ASK_USER]`/`[NEED_MORE_WORK]` 是 summary 阶段提示词要求 LLM 首行输出的
 * 完成度信号，属于控制标记而非正文。后端不应为显示而篡改正文（否则会污染后续喂回 LLM
 * 的上下文），剥离职责落在显示层。不依赖 `message.phase`，保证任何情况下都正确显示。
 *
 * 注意：`[NEED_MORE_WORK]` 在 AgentTurn 的分组逻辑中已被单独拦截为 thinking 片段，
 * 不会走到这里；此处主要处理 `[DONE]`/`[ASK_USER]`。
 */
export function displayTextContent(message: MessageItem): string {
  return stripSummaryStatusMarker(textContent(message));
}

export function resolveAssetUrl(url: string): string {
  if (!url) return "";
  if (url.startsWith("http://") || url.startsWith("https://") || url.startsWith("asset://")) {
    return url;
  }
  if (url.startsWith("/")) {
    return convertFileSrc(url);
  }
  return url;
}

export function resolveMarkdownImages(md: string): string {
  return md.replace(
    /(!\[[^\]]*\]\()(\/[^\s)]+)(\))/g,
    (_, prefix, path, suffix) => prefix + resolveAssetUrl(path) + suffix,
  );
}

/** 从 LLM 输出系统消息中提取解释文本 */
export function extractLlmExplanation(content: string): string {
  const lines = content.split("\n");
  const contentIdx = lines.findIndex((l) => l.startsWith("content:"));
  if (contentIdx >= 0 && contentIdx + 1 < lines.length) {
    return lines.slice(contentIdx + 1).join("\n").trim();
  }
  return "";
}

export function llmOutputHasToolCalls(content: string): boolean {
  return content
    .split("\n")
    .some((line) => line.trim().startsWith("tool_calls:"));
}

export function toolItemSucceeded(tool: MessageItem): boolean {
  // 只用后端透传的结构化字段判断；不要扫描正文文本。
  // read_file 的正文是被读取文件的原始内容，若文件里恰好含 "ok=false"
  // 字样（配置/日志/源码），基于文本的启发式会把它误判为失败。
  return !tool.tool_result_is_error;
}

export function summarizeToolGroup(tools: MessageItem[]): string {
  const total = tools.length;
  const failed = tools.filter((tool) => !toolItemSucceeded(tool)).length;
  const succeeded = total - failed;
  const names = Array.from(
    new Set(
      tools
        .map((tool) => tool.tool_name || getToolMessageMeta(tool).toolName || "")
        .filter(Boolean),
    ),
  );
  const nameSummary = names.length > 0
    ? ` · ${names.slice(0, 3).join(", ")}${names.length > 3 ? ` 等 ${names.length} 类` : ""}`
    : "";
  const statusSummary = failed > 0 ? `成功 ${succeeded} / 失败 ${failed}` : `成功 ${succeeded}`;
  return `工具调用 ${total} 次 · ${statusSummary}${nameSummary}`;
}

/** 从 Tool result 消息提取元数据（不依赖 System 摘要格式）。 */
export function getToolMessageMeta(msg: MessageItem): SystemMessageMeta {
  const content = textContent(msg);
  const toolName = msg.tool_name || "";
  const isError = msg.tool_result_is_error;

  // 注入消息格式（数据来源：xxx）
  if (content.startsWith("数据来源：")) {
    const sourceMatch = content.match(/^数据来源：(\S+)/);
    const source = sourceMatch ? sourceMatch[1] : "plugin";
    const cmdMatch = content.match(/command:\s*(.+)/);
    const urlMatch = content.match(/url:\s*(.+)/);
    const titleMatch = content.match(/title:\s*(.+)/);
    const detail = cmdMatch?.[1] || urlMatch?.[1] || titleMatch?.[1] || "";
    return {
      icon: Plug as LucideIcon,
      label: "插件注入",
      summary: detail
        ? `${source} · ${detail.length > 50 ? detail.slice(0, 47) + "..." : detail}`
        : source,
      toolName: source,
    };
  }

  // recall_memory
  if (toolName === "recall_memory" || content.startsWith("[记忆检索]")) {
    const countMatch = content.match(/命中 (\d+) 条/);
    const count = countMatch ? countMatch[1] : "";
    const noHit = content.includes("无相关记忆");
    return {
      icon: Brain as LucideIcon,
      label: "记忆检索",
      summary: noHit ? "无命中" : count ? `${count} 条命中` : "记忆检索",
      toolName: "recall_memory",
    };
  }

  // 正常工具结果
  const parts: string[] = [];
  if (toolName) parts.push(toolName);
  parts.push(isError ? "FAIL" : "OK");
  const cmdMatch = content.match(/命令:\s*(.+)/);
  if (cmdMatch) {
    const cmd = cmdMatch[1];
    parts.push(cmd.length > 50 ? cmd.slice(0, 47) + "..." : cmd);
  }
  return {
    icon: Terminal as LucideIcon,
    label: "工具执行",
    summary: parts.join(" · ") || content.split("\n")[0].slice(0, 60),
    toolName: toolName || undefined,
  };
}

export function groupMessages(messages: MessageItem[]): MessageGroup[] {
  const groups: MessageGroup[] = [];
  let currentAgentTurn: MessageGroup | null = null;

  for (const msg of messages) {
    if (msg.phase === "compressedresume") continue;
    if (msg.worker_id) {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      const previous = groups[groups.length - 1];
      if (previous?.type === "worker" && previous.worker_id === msg.worker_id) {
        previous.messages.push(msg);
      } else {
        groups.push({ key: `worker-${msg.worker_id}-${msg.id}`, type: "worker", worker_id: msg.worker_id, messages: [msg] });
      }
    } else if (msg.role === "user") {
      if (currentAgentTurn) { groups.push(currentAgentTurn); currentAgentTurn = null; }
      groups.push({ key: msg.id, type: "user", messages: [msg] });
    } else {
      if (!currentAgentTurn) {
        currentAgentTurn = { key: `turn-${msg.id}`, type: "agent_turn", messages: [] };
      }
      currentAgentTurn.messages.push(msg);
    }
  }
  if (currentAgentTurn) groups.push(currentAgentTurn);
  return groups;
}

export function workerContentMessages(messages: MessageItem[]): MessageItem[] {
  return messages.filter((m) => m.worker_id);
}

export function workerBelongsToAgent(
  workerId: string | undefined,
  role: string,
  agentId: string | undefined,
): boolean {
  if (!workerId) return false;
  if (agentId) return workerId === `agent:${role}:${agentId}`;
  // 兼容没有持久 Agent ID 的旧会话。
  return workerId.startsWith(`agent:${role}:`);
}

export function sameMessageRefs(left: MessageItem[], right: MessageItem[]): boolean {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i++) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}

export function hasMessage(messages: MessageItem[], id: string | null): boolean {
  return !!id && messages.some((message) => message.id === id);
}

export function extractAgentRoles(content: string, agents: { role: string; label: string }[]): string[] {
  const roles = new Set<string>();
  const addByLabel = (label?: string) => {
    if (!label || label === "User") return;
    const agent = agents.find((item) => item.label === label);
    if (agent) roles.add(agent.role);
  };
  const createMatch = content.match(/^\[Agent\] .+? \((.+?)\)/);
  if (createMatch) roles.add(createMatch[1]);
  const statusMatch = content.match(/^\[Agent\] (.+?) 状态变更:/);
  if (statusMatch) addByLabel(statusMatch[1]);
  const lockMatch = content.match(/^\[文件锁\] .+ by (.+)$/);
  if (lockMatch) addByLabel(lockMatch[1]);
  return Array.from(roles);
}

export function parseAgentReply(content: string): { label: string; body: string } | null {
  const match = content.match(/^<!-- tiangong-agent-reply -->\n<!-- label:([^\n]*) -->\n\n?([\s\S]*)$/);
  if (!match) return null;
  return {
    label: match[1].trim() || "Agent",
    body: match[2].trim(),
  };
}
