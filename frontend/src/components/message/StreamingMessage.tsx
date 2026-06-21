import { MdPreview } from "md-editor-rt";
import { useResolvedTheme } from "@/hooks/useTheme";
import { ThinkingBlock } from "../ThinkingBlock";
import { resolveMarkdownImages } from "./utils";

export function StreamingMessage({ content, reasoningContent }: { content: string; reasoningContent: string }) {
  const resolvedTheme = useResolvedTheme();
  return (
    <div>
      {reasoningContent && <ThinkingBlock content={reasoningContent} defaultExpanded={true} />}
      <MdPreview modelValue={resolveMarkdownImages(content)} theme={resolvedTheme} previewTheme="github" />
      {content.length > 0 && <span className="inline-block w-1.5 h-4 bg-primary ml-0.5 animate-pulse" />}
    </div>
  );
}
