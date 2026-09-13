import { useEffect, useMemo, useRef, useState } from "react";
import { MdPreview } from "md-editor-rt";
import { useResolvedTheme } from "@/hooks/useTheme";
import { ThinkingBlock } from "../ThinkingBlock";
import { resolveMarkdownImages } from "./utils";

/** 流式正文渲染节流间隔。增量事件约 16ms 一批，而 MdPreview 每次收到新
 * modelValue 都会整篇重新解析渲染——长回答（数千 token、含表格）时单次
 * 解析达几十毫秒，逐批重渲会把主线程打满、界面掉帧。以固定间隔渲染中间
 * 态：肉眼流畅度不变，解析开销恒定；流式结束后组件卸载，由持久化消息的
 * 渲染路径展示全文，节流不丢内容。 */
const STREAM_RENDER_INTERVAL_MS = 150;

export function StreamingMessage({ content, reasoningContent }: { content: string; reasoningContent: string }) {
  const resolvedTheme = useResolvedTheme();
  // Markdown 只按节流间隔更新；内容回退（长度变短，如编辑重发）立即同步。
  const [renderedContent, setRenderedContent] = useState(content);
  const latestContentRef = useRef(content);
  const renderTimerRef = useRef<number | null>(null);

  useEffect(() => {
    latestContentRef.current = content;
    if (content === renderedContent) return;
    if (content.length < renderedContent.length) {
      if (renderTimerRef.current !== null) {
        clearTimeout(renderTimerRef.current);
        renderTimerRef.current = null;
      }
      setRenderedContent(content);
      return;
    }
    if (renderTimerRef.current !== null) return;
    renderTimerRef.current = window.setTimeout(() => {
      renderTimerRef.current = null;
      setRenderedContent(latestContentRef.current);
    }, STREAM_RENDER_INTERVAL_MS);
  }, [content, renderedContent]);

  useEffect(() => () => {
    if (renderTimerRef.current !== null) clearTimeout(renderTimerRef.current);
  }, []);

  // 图片路径解析随节流后的渲染值缓存：父组件每批重渲时只做字符串比较，
  // 正则扫描仅在渲染值实际变化（约 150ms 一次）时执行。
  const previewModelValue = useMemo(
    () => resolveMarkdownImages(renderedContent),
    [renderedContent],
  );

  return (
    // ReAct 过程消息的父容器使用 pre-wrap 展示完成态纯文本。流式 Markdown 必须覆盖该
    // 可继承样式，否则预览生成的标签间格式换行也会显示，形成额外空行。
    <div className="whitespace-normal">
      {/* 正文开始输出即视为思考结束：停表并收起（完成后由持久化值精确展示）。 */}
      {reasoningContent && <ThinkingBlock content={reasoningContent} isActive={!content} defaultExpanded />}
      <MdPreview modelValue={previewModelValue} theme={resolvedTheme} previewTheme="github" />
      {content.length > 0 && <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />}
    </div>
  );
}
