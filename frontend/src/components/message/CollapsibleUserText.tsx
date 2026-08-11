import { useCallback, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";

const COLLAPSED_LINES = 4;

interface CollapsibleUserTextProps {
  children: ReactNode;
  messageId: string;
}

export function CollapsibleUserText({ children, messageId }: CollapsibleUserTextProps) {
  const contentRef = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [isOverflowing, setIsOverflowing] = useState(false);

  const measureOverflow = useCallback(() => {
    const element = contentRef.current;
    if (!element) return;

    const lineHeight = Number.parseFloat(window.getComputedStyle(element).lineHeight);
    if (!Number.isFinite(lineHeight)) return;

    setIsOverflowing(element.scrollHeight > lineHeight * COLLAPSED_LINES + 1);
  }, []);

  useLayoutEffect(() => {
    setExpanded(false);
  }, [messageId]);

  useLayoutEffect(() => {
    measureOverflow();

    const element = contentRef.current;
    if (!element || typeof ResizeObserver === "undefined") return;

    const observer = new ResizeObserver(measureOverflow);
    observer.observe(element);
    return () => observer.disconnect();
  }, [children, measureOverflow]);

  return (
    <div>
      <p
        ref={contentRef}
        className="whitespace-pre-wrap break-words text-sm leading-5"
        style={!expanded ? {
          display: "-webkit-box",
          WebkitBoxOrient: "vertical",
          WebkitLineClamp: COLLAPSED_LINES,
          overflow: "hidden",
        } : undefined}
      >
        {children}
      </p>
      {isOverflowing && (
        <button
          type="button"
          className="mt-1 inline-flex items-center gap-0.5 text-xs text-muted-foreground transition-colors hover:text-foreground"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded ? <ChevronUp className="h-3 w-3" /> : <ChevronDown className="h-3 w-3" />}
          <span>{expanded ? "收起" : "展开"}</span>
        </button>
      )}
    </div>
  );
}
