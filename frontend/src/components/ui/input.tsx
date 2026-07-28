import * as React from "react"

import { cn } from "@/lib/utils"
import { useArrowKeyBoundaryGuard } from "@/hooks/useArrowKeyBoundaryGuard"

const Input = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, type, onKeyDown, ...props }, ref) => {
    // WKWebView 在光标处于边界时会把方向键转义序列误当文本插入（渲染成方格），
    // 这里在边界处兜底 preventDefault。详见 useArrowKeyBoundaryGuard。
    const handleKeyDown = useArrowKeyBoundaryGuard<HTMLInputElement>(onKeyDown);
    return (
      <input
        type={type}
        className={cn(
          "flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-base ring-offset-background file:border-0 file:bg-transparent file:text-sm file:font-medium file:text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 md:text-sm",
          className
        )}
        ref={ref}
        onKeyDown={handleKeyDown}
        {...props}
      />
    )
  }
)
Input.displayName = "Input"

export { Input }
