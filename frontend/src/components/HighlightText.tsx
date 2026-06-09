import { type TextMatch } from '@/utils/search';

interface HighlightTextProps {
  text: string;
  matches: TextMatch[];
  currentMatchStart: number | null;
}

/** 按匹配位置分片渲染文本，匹配部分用 <mark> 包裹 */
export function HighlightText({ text, matches, currentMatchStart }: HighlightTextProps) {
  if (matches.length === 0) return <>{text}</>;

  const parts: Array<{ type: 'text' | 'highlight'; content: string; isCurrent: boolean }> = [];
  let lastEnd = 0;

  for (const m of matches) {
    if (m.start > lastEnd) {
      parts.push({ type: 'text', content: text.slice(lastEnd, m.start), isCurrent: false });
    }
    const isCurrent = currentMatchStart !== null && m.start === currentMatchStart;
    parts.push({ type: 'highlight', content: text.slice(m.start, m.end), isCurrent });
    lastEnd = m.end;
  }
  if (lastEnd < text.length) {
    parts.push({ type: 'text', content: text.slice(lastEnd), isCurrent: false });
  }

  return (
    <>
      {parts.map((part, i) =>
        part.type === 'highlight' ? (
          <mark key={i} className={part.isCurrent ? 'search-highlight-current' : 'search-highlight'}>
            {part.content}
          </mark>
        ) : (
          <span key={i}>{part.content}</span>
        ),
      )}
    </>
  );
}
