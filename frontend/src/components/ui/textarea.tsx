import * as React from "react"

import { cn } from "@/lib/utils"
import { useArrowKeyBoundaryGuard } from "@/hooks/useArrowKeyBoundaryGuard"

export interface TextareaProps
  extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {}

const Textarea = React.forwardRef<HTMLTextAreaElement, TextareaProps>(
  ({ className, onKeyDown, ...props }, ref) => {
    // WKWebView 在光标处于边界时会把方向键转义序列误当文本插入（渲染成方格），
    // 这里在边界处兜底 preventDefault。详见 useArrowKeyBoundaryGuard。
    const handleKeyDown = useArrowKeyBoundaryGuard<HTMLTextAreaElement>(onKeyDown);
    return (
      <textarea
        className={cn(
          "flex min-h-[60px] w-full rounded-md border border-input bg-transparent px-3 py-2 text-sm shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50",
          className
        )}
        ref={ref}
        onKeyDown={handleKeyDown}
        {...props}
      />
    )
  }
)
Textarea.displayName = "Textarea"

export { Textarea }
