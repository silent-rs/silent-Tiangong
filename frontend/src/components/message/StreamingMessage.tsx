import { useEffect, useRef, useState } from "react";
import { MdPreview } from "md-editor-rt";
import { useResolvedTheme } from "@/hooks/useTheme";
import { ThinkingBlock } from "../ThinkingBlock";
import { resolveMarkdownImages } from "./utils";

/**
 * 流式期间 Markdown 预览的刷新间隔（毫秒）。
 *
 * 流事件仍以约 16ms 写入状态，但直接把不断增长的半成品 Markdown 交给 MdPreview，
 * 会因反复解析未闭合的代码块/列表/表格等结构，产生临时块级排版异常（表现为换行间隔过大）。
 * 降低预览刷新频率可显著减少半成品解析次数，同时保留流式观感。
 */
const MD_PREVIEW_THROTTLE_MS = 80;

export function StreamingMessage({ content, reasoningContent }: { content: string; reasoningContent: string }) {
  const resolvedTheme = useResolvedTheme();
  // 传给 MdPreview 的内容降频更新：latestRef 始终持有最新文本，renderedContent 每
  // MD_PREVIEW_THROTTLE_MS 至多更新一次。这是节流（非防抖）——content 持续高频变化时，
  // 定时器不会被反复重置，到点即 flush；组件卸载时清理定时器。
  const latestRef = useRef(content);
  const [renderedContent, setRenderedContent] = useState(content);
  const timerRef = useRef<number | null>(null);

  useEffect(() => {
    latestRef.current = content;
    // 已有定时器在等待：由它统一 flush，避免每次都重置导致末尾内容迟迟不刷新。
    if (timerRef.current !== null) return;
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      setRenderedContent(latestRef.current);
    }, MD_PREVIEW_THROTTLE_MS);
  }, [content]);

  // 仅在组件卸载时清理定时器；content 变化时不清理（否则退化为防抖）。
  useEffect(() => {
    return () => {
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, []);

  return (
    <div>
      {reasoningContent && <ThinkingBlock content={reasoningContent} isActive defaultExpanded />}
      <MdPreview modelValue={resolveMarkdownImages(renderedContent)} theme={resolvedTheme} previewTheme="github" />
      {content.length > 0 && <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />}
    </div>
  );
}
