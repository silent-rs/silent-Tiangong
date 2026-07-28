import { parseBlocks } from './mentionBlocks';

export interface MentionBoundary {
  start: number;
  end: number;
  leadingSeparatorStart: number | null;
  trailingSeparatorEnd: number | null;
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
