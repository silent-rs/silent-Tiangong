/**
 * 解析内联 <think>...</think> 标签，将思考内容与正文分离。
 * 支持流式场景（未闭合的 <think> 标签）。
 */
export function parseInlineThinking(text: string): { thinking: string; content: string } {
  if (!text.includes('<think>')) {
    return { thinking: '', content: text };
  }

  let thinking = '';
  let content = '';
  let remaining = text;

  while (remaining.length > 0) {
    const openIdx = remaining.indexOf('<think>');
    if (openIdx === -1) {
      content += remaining;
      break;
    }

    // <think> 之前的内容属于正文
    content += remaining.slice(0, openIdx);

    const afterOpen = remaining.slice(openIdx + 7); // 7 = '<think>'.length
    const closeIdx = afterOpen.indexOf('</think>');

    if (closeIdx === -1) {
      // 未闭合 — 流式场景，剩余全部视为思考内容
      thinking += afterOpen;
      break;
    }

    thinking += afterOpen.slice(0, closeIdx);
    remaining = afterOpen.slice(closeIdx + 8); // 8 = '</think>'.length
  }

  return { thinking: thinking.trim(), content: content.trim() };
}
