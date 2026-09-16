import * as React from "react"

// 浮层断点：对齐 MainApp 的 SIDEBAR_RESTORE_THRESHOLD（内容最小 400 + 侧边栏 256），
// 窄于此窗口时侧边栏以 Sheet 浮层展示（不挤压、不扩窗），宽于此恢复挤压布局
const MOBILE_BREAKPOINT = 656

export function useIsMobile() {
  const [isMobile, setIsMobile] = React.useState<boolean | undefined>(undefined)

  React.useEffect(() => {
    const mql = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`)
    const onChange = () => {
      setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    }
    mql.addEventListener("change", onChange)
    setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    return () => mql.removeEventListener("change", onChange)
  }, [])

  return !!isMobile
}
