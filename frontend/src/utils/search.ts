import { textContent, type Message } from '@/api/tauri';

export interface SearchMatch {
  messageId: string;
  groupIndex: number;
  field: 'content' | 'reasoning_content' | 'tool_name';
  start: number;
  end: number;
}

export interface TextMatch {
  start: number;
  end: number;
}

/** 大小写不敏感纯文本匹配，返回所有 (start, end) 位置 */
export function findTextOccurrences(text: string, query: string): TextMatch[] {
  if (!query) return [];
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  const matches: TextMatch[] = [];
  let offset = 0;
  while (offset < lower.length) {
    const idx = lower.indexOf(q, offset);
    if (idx < 0) break;
    matches.push({ start: idx, end: idx + q.length });
    offset = idx + 1;
  }
  return matches;
}

/** 为单条消息拼接所有可搜索字段 */
function buildSearchableTexts(
  msg: Message,
): Array<{ field: SearchMatch['field']; text: string }> {
  const parts: Array<{ field: SearchMatch['field']; text: string }> = [];
  const content = textContent(msg);
  if (content) parts.push({ field: 'content', text: content });
  if (msg.reasoning_content) {
    parts.push({ field: 'reasoning_content', text: msg.reasoning_content });
  }
  if (msg.tool_name) {
    parts.push({ field: 'tool_name', text: msg.tool_name });
  }
  return parts;
}

export interface MessageGroupLike {
  messages: Array<{ id: string }>;
}

/** 遍历所有消息和分组，收集搜索匹配 */
export function findSearchMatches(
  messages: Message[],
  query: string,
  groups: MessageGroupLike[],
): SearchMatch[] {
  if (!query) return [];

  // 建立 messageId → groupIndex 映射
  const msgGroupMap = new Map<string, number>();
  for (let gi = 0; gi < groups.length; gi++) {
    for (const msg of groups[gi].messages) {
      msgGroupMap.set(msg.id, gi);
    }
  }

  const matches: SearchMatch[] = [];
  for (const msg of messages) {
    const groupIndex = msgGroupMap.get(msg.id) ?? -1;
    const fields = buildSearchableTexts(msg);
    for (const { field, text } of fields) {
      const occurrences = findTextOccurrences(text, query);
      for (const occ of occurrences) {
        matches.push({
          messageId: msg.id,
          groupIndex,
          field,
          start: occ.start,
          end: occ.end,
        });
      }
    }
  }

  return matches;
}
