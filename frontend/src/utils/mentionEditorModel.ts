import { parseBlocks, type Block } from './mentionBlocks';

export interface MentionBoundary {
  start: number;
  end: number;
  leadingSeparatorStart: number | null;
  trailingSeparatorEnd: number | null;
}

/** mention 输入态的活跃区：`@` 位置到光标的文本区间（过滤词可含空格）。 */
export interface ActiveRange {
  start: number;
  end: number;
}

export type MentionKey = 'Backspace' | 'Delete' | 'ArrowLeft' | 'ArrowRight';

export type MentionKeyAction =
  | { type: 'move'; offset: number }
  | { type: 'delete'; start: number; end: number; offset: number };

export interface TextReplacement {
  value: string;
  offset: number;
}

export function getMentionBoundaries(text: string): MentionBoundary[] {
  const boundaries: MentionBoundary[] = [];
  let offset = 0;

  for (const block of parseBlocks(text)) {
    if (block.type === 'text') {
      offset += block.value.length;
      continue;
    }

    const start = offset;
    const end = start + block.token.length;
    boundaries.push({
      start,
      end,
      leadingSeparatorStart: start > 0 && text[start - 1] === ' ' ? start - 1 : null,
      trailingSeparatorEnd: text[end] === ' ' ? end + 1 : null,
    });
    offset = end;
  }

  return boundaries;
}

export function resolveMentionKeyAction(
  text: string,
  caret: number,
  key: MentionKey,
): MentionKeyAction | null {
  if (caret < 0 || caret > text.length) return null;

  const forward = key === 'Delete' || key === 'ArrowRight';
  const boundary = getMentionBoundaries(text).find((item) => (
    forward
      ? caret === item.start || caret === item.leadingSeparatorStart
      : caret === item.end || caret === item.trailingSeparatorEnd
  ));
  if (!boundary) return null;

  if (key === 'ArrowLeft') {
    return {
      type: 'move',
      offset: boundary.leadingSeparatorStart ?? boundary.start,
    };
  }
  if (key === 'ArrowRight') {
    return {
      type: 'move',
      offset: boundary.trailingSeparatorEnd ?? boundary.end,
    };
  }

  let start = boundary.start;
  let end = boundary.end;
  if (boundary.trailingSeparatorEnd != null) {
    end = boundary.trailingSeparatorEnd;
    if (end === text.length && boundary.leadingSeparatorStart != null) {
      start = boundary.leadingSeparatorStart;
    }
  } else if (boundary.leadingSeparatorStart != null) {
    start = boundary.leadingSeparatorStart;
  }

  return { type: 'delete', start, end, offset: start };
}

export function deleteMentionSelection(
  text: string,
  selectionStart: number,
  selectionEnd: number,
): TextReplacement {
  let start = Math.max(0, Math.min(selectionStart, selectionEnd, text.length));
  let end = Math.max(0, Math.min(Math.max(selectionStart, selectionEnd), text.length));

  if (start === end) return { value: text, offset: start };

  for (const boundary of getMentionBoundaries(text)) {
    if (start < boundary.end && end > boundary.start) {
      start = Math.min(start, boundary.start);
      end = Math.max(end, boundary.end);
    }
  }

  if (start > 0 && end < text.length && text[start - 1] === ' ' && text[end] === ' ') {
    end += 1;
  }

  return {
    value: text.slice(0, start) + text.slice(end),
    offset: start,
  };
}

export function replaceMentionCompletion(
  text: string,
  mentionStart: number,
  cursor: number,
  token: string,
): TextReplacement | null {
  if (
    mentionStart < 0
    || mentionStart > text.length
    || cursor < mentionStart
    || cursor > text.length
  ) return null;

  return {
    value: `${text.slice(0, mentionStart)}${token} ${text.slice(cursor)}`,
    offset: mentionStart + token.length + 1,
  };
}

/// mention 选中时的替换区间末端。
///
/// 过滤词由面板内的独立搜索框承载，不写入消息文本，因此消息文本里只有
/// `@` 到触发时那段已输入内容；替换末端取触发时记录的 `mentionEnd`，不能用
/// 当前光标（用户可能已把焦点移回编辑器别处）。
/// mention 选中时的替换区间末端。
///
/// 过滤词由面板内的独立搜索框承载，不写入消息文本，因此消息文本里只有
/// `@` 到触发时光标那段已输入内容；替换末端取触发时记录的 `mentionEnd`，
/// 不能用当前光标（用户可能已把焦点移回编辑器别处）。
/// `mentionEnd` 非法（未记录或早于 `@`）时退化为只替换 `@` 一个字符。
export function mentionReplaceEnd(mentionStart: number, mentionEnd: number): number {
  return mentionEnd > mentionStart ? mentionEnd : mentionStart + 1;
}

export function insertTextAtMentionBoundary(
  text: string,
  caret: number,
  insertedText: string,
): TextReplacement | null {
  if (caret < 0 || caret > text.length || insertedText.length === 0) return null;

  const boundaries = getMentionBoundaries(text);
  const atMentionBoundary = boundaries.some((boundary) => (
    caret === boundary.start
    || caret === boundary.end
    || caret === boundary.leadingSeparatorStart
    || caret === boundary.trailingSeparatorEnd
  ));
  if (!atMentionBoundary) return null;

  const before = text.slice(0, caret);
  const after = text.slice(caret);
  const touchesMentionOnLeft = boundaries.some((boundary) => boundary.end === caret);
  const touchesMentionOnRight = boundaries.some((boundary) => boundary.start === caret);
  const leadingSeparator = touchesMentionOnLeft
    && !/\s$/.test(before)
    && !/^\s/.test(insertedText)
    ? ' '
    : '';
  const trailingSeparator = touchesMentionOnRight
    && !/\s$/.test(insertedText)
    && !/^\s/.test(after)
    ? ' '
    : '';

  return {
    value: `${before}${leadingSeparator}${insertedText}${trailingSeparator}${after}`,
    offset: caret + leadingSeparator.length + insertedText.length,
  };
}

export function normalizePastedText(text: string): string {
  return text.replace(/\r\n?/g, '\n');
}

/**
 * 把落在活跃区内的 mention 块降级为 text 块。
 *
 * 输入态下 `@` 到光标之间的文本正在被编辑（过滤词可含空格），其中的
 * `@xxx` 形片段会被 {@link parseBlocks} 误识别为 mention 并渲染成 chip——
 * 用户尚未确认候选，chip 化会打断继续输入。活跃区永不 chip 化。
 *
 * 区间判定用块的起始偏移，取 `[range.start, range.end)`；`range` 为空表示
 * 不在输入态，原样返回。
 */
export function deactivateBlocksInRange(blocks: Block[], range: ActiveRange | null | undefined): Block[] {
  if (!range) return blocks;
  let offset = 0;
  return blocks.map((block) => {
    const start = offset;
    offset += block.type === 'text' ? block.value.length : block.token.length;
    if (block.type === 'mention' && start >= range.start && start < range.end) {
      return { type: 'text', value: block.token };
    }
    return block;
  });
}
