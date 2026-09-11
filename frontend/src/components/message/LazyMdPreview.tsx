import { useEffect, useRef, useState } from "react";
import { MdPreview } from "md-editor-rt";

/**
 * 懒挂载的 Markdown 预览。
 *
 * MdPreview（md-editor-rt）单次挂载是数十毫秒级的重组件（组件初始化
 * 远贵于正文解析本身），切换会话一次性挂载可见窗口内的多轮回复会把
 * 这份成本叠加成可感知的卡顿。此处先用按文本长度估高的占位元素撑住
 * 布局，等元素滚动到视口附近（提前 600px）才真正挂载 MdPreview；
 * 已挂载的实例保持不变。
 *
 * 环境不支持 IntersectionObserver（测试/旧内核）时直接渲染，行为退
 * 化为立即挂载。
 */
export function LazyMdPreview({ modelValue, theme }: {
  modelValue: string;
  theme: "light" | "dark";
}) {
  const placeholderRef = useRef<HTMLDivElement>(null);
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    if (mounted) return;
    const el = placeholderRef.current;
    if (!el || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setMounted(true);
          observer.disconnect();
        }
      },
      // 提前一段距离开始挂载，滚动经过时正文已就绪，肉眼无感
      { rootMargin: "600px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [mounted]);

  if (mounted || typeof IntersectionObserver === "undefined") {
    return <MdPreview modelValue={modelValue} theme={theme} previewTheme="github" />;
  }

  // 占位高度与虚拟列表 estimateSize 的量级保持一致，避免挂载后大幅跳动
  const estimatedHeight = Math.min(Math.max(Math.ceil(modelValue.length / 48) * 22, 60), 460);
  return (
    <div
      ref={placeholderRef}
      style={{ minHeight: estimatedHeight }}
      className="text-sm text-muted-foreground/40"
      aria-busy="true"
    />
  );
}
