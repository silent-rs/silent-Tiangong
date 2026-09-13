import { textContent } from "@/api/tauri";
import { resolveAttachmentUrl } from "@/utils/attachments";
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

/** 将毫秒格式化为人类可读时长：< 1s 显示 ms；秒以上取整秒、单位用中文「秒」，
 *  满一分钟进位为「N分SS秒」，满一小时进位为「N时MM分SS秒」。 */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = Math.floor(ms / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);
  if (hours > 0) {
    return `${hours}时${String(minutes).padStart(2, "0")}分${String(seconds).padStart(2, "0")}秒`;
  }
  if (minutes > 0) {
    return `${minutes}分${String(seconds).padStart(2, "0")}秒`;
  }
  return `${seconds}秒`;
}

/** 轮次状态对应的中文标签与颜色（失败/取消直观醒目，成功保持低调）。 */
export const TURN_STATUS_META: Record<string, { label: string; className: string; dot: string }> = {
  failed: { label: "失败", className: "text-destructive", dot: "bg-destructive" },
  cancelled: { label: "已取消", className: "text-muted-foreground", dot: "bg-muted-foreground" },
};

/** 工具行耗时格式：与总时间的中文格式区分，单位用英文 h/m/s/ms，
 *  进位规则相同（< 1s 显示 ms，满 60 秒进分，满 60 分进时）。 */
export function formatToolDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const totalSeconds = Math.floor(ms / 1000);
  const seconds = totalSeconds % 60;
  const minutes = Math.floor(totalSeconds / 60) % 60;
  const hours = Math.floor(totalSeconds / 3600);
  if (hours > 0) {
    return `${hours}h${String(minutes).padStart(2, "0")}m${String(seconds).padStart(2, "0")}s`;
  }
  if (minutes > 0) {
    return `${minutes}m${String(seconds).padStart(2, "0")}s`;
  }
  return `${seconds}s`;
}

export function msgReasoning(message: MessageItem): string {
  return (message.reasoning_content ?? "").trim();
}

/** 总结阶段的状态标记，需在前端展示时剥离。 */
const SUMMARY_STATUS_MARKERS = ["[DONE]", "[NEED_MORE_WORK]"];
const NEED_MORE_WORK_MARKER = "[NEED_MORE_WORK]";

/** 剥离总结阶段回复首行的状态标记（[DONE]/[NEED_MORE_WORK]）。 */
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
 * `[DONE]`/`[NEED_MORE_WORK]` 是 summary 阶段提示词要求 LLM 首行输出的
 * 完成度信号，属于控制标记而非正文。后端不应为显示而篡改正文（否则会污染后续喂回 LLM
 * 的上下文），剥离职责落在显示层。不依赖 `message.phase`，保证任何情况下都正确显示。
 *
 * 注意：`[NEED_MORE_WORK]` 在 AgentTurn 的分组逻辑中已被单独拦截为 thinking 片段，
 * 不会走到这里；此处主要处理 `[DONE]`。
 */
export function displayTextContent(message: MessageItem): string {
  return stripSummaryStatusMarker(textContent(message));
}

export function resolveAssetUrl(url: string): string {
  return resolveAttachmentUrl(url);
}

/** 判断链接目标是否本地路径形态（file://、POSIX 绝对路径、Windows 盘符或 UNC）。 */
function isLocalFileUrl(url: string): boolean {
  return url.startsWith('file:')
    || url.startsWith('/')
    || /^[A-Za-z]:[\\/]/.test(url)
    || url.startsWith('\\\\');
}

/** 把指向本地 SVG 文件的链接改写为图片语法。模型常用 [名称](file:///…/x.svg)
 *  引用生成的矢量图，按链接渲染只是一行文本；改写后经下方图片路径解析内联显示
 *  （img 上下文中的 SVG 不执行脚本）。远程地址不改写，保留链接点击行为。 */
function inlineLocalSvgLinks(md: string): string {
  return md.replace(
    /(?<!!)\[([^\]]*)\]\(([^\s)]+)\)/g,
    (match, alt: string, url: string) => {
      const path = url.split(/[?#]/)[0];
      return path.toLowerCase().endsWith('.svg') && isLocalFileUrl(url)
        ? `![${alt}](${url})`
        : match;
    },
  );
}

export function resolveMarkdownImages(md: string): string {
  return inlineLocalSvgLinks(md).replace(
    /(!\[[^\]]*\]\()([^\s)]+)(\))/g,
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

/** 分组结果引用缓存：流式期间 messages 数组每批（约 16ms）都是新引用，但
 * 历史组的消息元素引用不变。按组 key 缓存上一次结果，消息引用完全一致时
 * 复用旧组对象——下游（AgentTurn memo、各 useMemo）依赖组内数组引用即可
 * 保持命中，不必每批全量重建分组造成级联重算与分配压力。
 *
 * 不可变契约：复用判定按「消息对象引用逐条相等」进行，调用方必须以替换
 * 引用的方式更新消息（store 现有写入路径均为不可变更新）。若将来出现就
 * 地修改消息对象的写入路径，本缓存与 AgentTurn 的 sameMessageRefs memo
 * 都会静默跳过重渲染而不报错。 */
let groupReuseCache: Map<string, { refs: MessageItem[]; group: MessageGroup }> | null = null;

/** 上一次返回的分组数组：本次所有组均命中复用时直接沿用，让依赖分组数组
 * 本身的 useMemo（turnResultByGroupKey、userGroupIndices 等）也保持命中。 */
let lastGroupsResult: MessageGroup[] | null = null;

export function groupMessages(messages: MessageItem[]): MessageGroup[] {
  // 第一遍：按原规则聚合出每组的消息列表（临时结构）。
  type PendingGroup = {
    key: string;
    type: MessageGroup["type"];
    worker_id?: string;
    msgs: MessageItem[];
  };
  const pending: PendingGroup[] = [];
  let currentAgentTurn: PendingGroup | null = null;

  for (const msg of messages) {
    if (msg.phase === "compressedresume") continue;
    if (msg.worker_id) {
      if (currentAgentTurn) { pending.push(currentAgentTurn); currentAgentTurn = null; }
      const previous = pending[pending.length - 1];
      if (previous?.type === "worker" && previous.worker_id === msg.worker_id) {
        previous.msgs.push(msg);
      } else {
        pending.push({ key: `worker-${msg.worker_id}-${msg.id}`, type: "worker", worker_id: msg.worker_id, msgs: [msg] });
      }
    } else if (msg.role === "user") {
      if (currentAgentTurn) { pending.push(currentAgentTurn); currentAgentTurn = null; }
      pending.push({ key: msg.id, type: "user", msgs: [msg] });
    } else {
      if (!currentAgentTurn) {
        currentAgentTurn = { key: `turn-${msg.id}`, type: "agent_turn", msgs: [] };
      }
      currentAgentTurn.msgs.push(msg);
    }
  }
  if (currentAgentTurn) pending.push(currentAgentTurn);

  // 第二遍：引用复用——消息引用逐条一致的组沿用上次的组对象。
  const nextCache = new Map<string, { refs: MessageItem[]; group: MessageGroup }>();
  let reusedCount = 0;
  const groups: MessageGroup[] = pending.map((p) => {
    const cached = groupReuseCache?.get(p.key);
    if (
      cached
      && cached.group.type === p.type
      && cached.refs.length === p.msgs.length
      && cached.refs.every((m, i) => m === p.msgs[i])
    ) {
      reusedCount += 1;
      nextCache.set(p.key, cached);
      return cached.group;
    }
    const group: MessageGroup = {
      key: p.key,
      type: p.type,
      ...(p.type === "worker" ? { worker_id: p.worker_id } : {}),
      messages: p.msgs,
    };
    nextCache.set(p.key, { refs: p.msgs, group });
    return group;
  });
  groupReuseCache = nextCache;
  // 所有组均命中复用且无 key 覆盖（重复 key 时 nextCache 更小）时，分组结果
  // 与上一次完全一致，直接沿用上次数组引用，派生 useMemo 也保持命中。
  if (
    reusedCount === pending.length
    && nextCache.size === pending.length
    && lastGroupsResult !== null
    && lastGroupsResult.length === groups.length
  ) {
    return lastGroupsResult;
  }
  lastGroupsResult = groups;
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
