import { useEffect, useRef, KeyboardEvent } from 'react';
import { ChevronUp, ChevronDown, X } from 'lucide-react';
import { useSearchStore } from '@/store/useSearchStore';

interface SearchBarProps {
  matchCount: number;
}

export function SearchBar({ matchCount }: SearchBarProps) {
  const searchQuery = useSearchStore((s) => s.searchQuery);
  const currentMatchIndex = useSearchStore((s) => s.currentMatchIndex);
  const setSearchQuery = useSearchStore((s) => s.setSearchQuery);
  const closeSearch = useSearchStore((s) => s.closeSearch);
  const nextMatch = useSearchStore((s) => s.nextMatch);
  const prevMatch = useSearchStore((s) => s.prevMatch);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      closeSearch();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) {
        prevMatch(matchCount);
      } else {
        nextMatch(matchCount);
      }
    }
  };

  const position =
    matchCount > 0 ? `${currentMatchIndex + 1}/${matchCount}` : '无匹配';

  return (
    <div className="sticky top-0 z-20 flex items-center gap-2 rounded-md border bg-background/95 px-3 py-1.5 backdrop-blur shadow-sm">
      <input
        ref={inputRef}
        data-search-input
        type="text"
        value={searchQuery}
        onChange={(e) => setSearchQuery(e.target.value)}
        onKeyDown={handleKeyDown}
        placeholder="搜索消息..."
        className="flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
      />
      <span className="shrink-0 text-xs text-muted-foreground min-w-[3rem] text-right">
        {position}
      </span>
      <button
        onClick={() => prevMatch(matchCount)}
        disabled={matchCount === 0}
        className="text-muted-foreground hover:text-foreground disabled:opacity-30 transition-colors"
        title="上一个匹配 (Shift+Enter)"
      >
        <ChevronUp className="w-4 h-4" />
      </button>
      <button
        onClick={() => nextMatch(matchCount)}
        disabled={matchCount === 0}
        className="text-muted-foreground hover:text-foreground disabled:opacity-30 transition-colors"
        title="下一个匹配 (Enter)"
      >
        <ChevronDown className="w-4 h-4" />
      </button>
      <button
        onClick={closeSearch}
        className="text-muted-foreground hover:text-foreground transition-colors"
        title="关闭搜索 (Esc)"
      >
        <X className="w-4 h-4" />
      </button>
    </div>
  );
}
