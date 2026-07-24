import { MdPreview } from "md-editor-rt";
import { useResolvedTheme } from "@/hooks/useTheme";
import { ThinkingBlock } from "../ThinkingBlock";
import { resolveMarkdownImages } from "./utils";

export function StreamingMessage({ content, reasoningContent }: { content: string; reasoningContent: string }) {
  const resolvedTheme = useResolvedTheme();
  return (
    // ReAct 过程消息的父容器使用 pre-wrap 展示完成态纯文本。流式 Markdown 必须覆盖该
    // 可继承样式，否则预览生成的标签间格式换行也会显示，形成额外空行。
    <div className="whitespace-normal">
      {reasoningContent && <ThinkingBlock content={reasoningContent} isActive defaultExpanded />}
      <MdPreview modelValue={resolveMarkdownImages(content)} theme={resolvedTheme} previewTheme="github" />
      {content.length > 0 && <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />}
    </div>
  );
}
