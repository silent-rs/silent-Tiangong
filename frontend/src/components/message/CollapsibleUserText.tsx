import { useCallback, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { ChevronDown, ChevronUp } from "lucide-react";
import { useResolvedTheme } from "@/hooks/useTheme";
import { LazyMdPreview } from "./LazyMdPreview";
import { resolveMarkdownImages } from "./utils";

const COLLAPSED_LINES = 4;

/** Markdown 正文折叠高度：4 行 text-sm 正文（约 22px/行）。
 * 与纯文本路径的 line-clamp 4 行语义对齐。 */
const COLLAPSED_MAX_HEIGHT = 88;

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

/// 用户消息 Markdown 正文：与纯文本气泡保持同样的 4 行折叠体验。
/// Markdown 渲染出的是块级结构，line-clamp 只对行盒生效、对块级子元素
/// 失效，因此折叠改用 maxHeight 截断；溢出测量观察内层自然高度容器，
/// 懒挂载完成或内容变化引起的尺寸变化都能触发重测。
export function CollapsibleMarkdownText({ text, messageId }: {
  text: string;
  messageId: string;
}) {
  const theme = useResolvedTheme();
  const contentRef = useRef<HTMLDivElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [isOverflowing, setIsOverflowing] = useState(false);

  const measureOverflow = useCallback(() => {
    const element = contentRef.current;
    if (!element) return;
    setIsOverflowing(element.offsetHeight > COLLAPSED_MAX_HEIGHT + 1);
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
  }, [text, measureOverflow]);

  return (
    <div className="user-bubble-md">
      <div style={!expanded ? {
        maxHeight: COLLAPSED_MAX_HEIGHT,
        overflow: "hidden",
      } : undefined}>
        {/* 覆盖可能继承的 pre-wrap：预览生成的标签间格式换行会显示成额外空行 */}
        <div ref={contentRef} className="whitespace-normal">
          <LazyMdPreview modelValue={resolveMarkdownImages(text)} theme={theme} />
        </div>
      </div>
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
