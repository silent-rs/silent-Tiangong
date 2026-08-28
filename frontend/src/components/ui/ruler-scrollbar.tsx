import * as React from "react"
import { cn } from "@/lib/utils"

interface RulerScrollbarProps {
  /** 用户消息数量；边栏严格按一条用户消息生成一根横条 */
  markerCount: number
  /** 当前消息列表所在位置对应的用户消息 */
  currentMarker?: number | null
  /** 鼠标命中的用户消息；用于父层展示预览 */
  onHover?: (info: { markerIndex: number; y: number; trackH: number } | null) => void
  /** 点击横条时跳转到对应用户消息 */
  onSelect?: (markerIndex: number) => void
  /** 底部需要避让的高度，例如右下角导航按钮组 */
  bottomInset?: number
  className?: string
}

/** 命令式接口：后台悬停时以宿主下发的坐标驱动与真实指针事件等同的效果 */
export interface RulerScrollbarHandle {
  /** clientY 为视口 y 坐标（等同 onPointerMove 的 event.clientY）；null 等同 onPointerLeave */
  externalPointer: (clientY: number | null) => void
  /** 后台首击补发：以视口 y 坐标命中的横条执行 onSelect（系统首击被窗口激活消费） */
  externalClick: (clientY: number) => void
}

const ROW_HEIGHT = 12
const TRACK_PADDING = 10
const BASE_WIDTH = 11
const GAUSS_SIGMA = 34
const GAUSS_EXTRA_WIDTH = 34

/**
 * 用户消息导航边栏。
 *
 * 它不是消息正文的第二根滚动条：边栏中每根横条只对应一条用户消息，横条较多时
 * 边栏拥有独立滚动位置。鼠标滚轮、方向键和翻页键只移动边栏；点击横条才跳转正文。
 */
export const RulerScrollbar = React.forwardRef<RulerScrollbarHandle, RulerScrollbarProps>(
  function RulerScrollbar({
    markerCount,
    currentMarker = null,
    onHover,
    onSelect,
    bottomInset = 0,
    className,
  }, ref) {
  const rootRef = React.useRef<HTMLDivElement>(null)
  const railRef = React.useRef<HTMLDivElement>(null)
  const frameRef = React.useRef<number | null>(null)
  const pendingPointerRef = React.useRef<number | null>(null)
  const [pointerY, setPointerY] = React.useState<number | null>(null)
  const [hoveredMarker, setHoveredMarker] = React.useState<number | null>(null)
  const [railScrollTop, setRailScrollTop] = React.useState(0)
  const [railHeight, setRailHeight] = React.useState(0)

  const markersHeight = markerCount * ROW_HEIGHT
  const contentHeight = markersHeight + TRACK_PADDING * 2
  const centeredOffset = Math.max(TRACK_PADDING, (railHeight - markersHeight) / 2)

  const updatePointer = React.useCallback((clientY: number) => {
    const root = rootRef.current
    const rail = railRef.current
    if (!root || !rail || markerCount === 0) return

    const rect = root.getBoundingClientRect()
    const y = Math.min(Math.max(clientY - rect.top, 0), rect.height)
    setPointerY(y)

    const contentY = y + rail.scrollTop - centeredOffset
    const markerIndex = Math.min(markerCount - 1, Math.max(0, Math.round((contentY - ROW_HEIGHT / 2) / ROW_HEIGHT)))
    setHoveredMarker(markerIndex)
    onHover?.({ markerIndex, y, trackH: rect.height })
  }, [centeredOffset, markerCount, onHover])

  const schedulePointerUpdate = React.useCallback((clientY: number) => {
    pendingPointerRef.current = clientY
    if (frameRef.current != null) return
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = null
      if (pendingPointerRef.current != null) updatePointer(pendingPointerRef.current)
    })
  }, [updatePointer])

  const pointerLeave = React.useCallback(() => {
    pendingPointerRef.current = null
    setPointerY(null)
    setHoveredMarker(null)
    onHover?.(null)
  }, [onHover])

  const scrollRailBy = React.useCallback((delta: number) => {
    const rail = railRef.current
    if (!rail) return
    rail.scrollTop += delta
    setRailScrollTop(rail.scrollTop)
  }, [])

  // 后台悬停：宿主轮询下发的窗口内坐标经父层转发至此，驱动与真实
  // 指针事件等同的横条变宽、高亮、预览卡与首击跳转（后台窗口收不到 DOM 指针事件）。
  React.useImperativeHandle(ref, () => ({
    externalPointer: (clientY: number | null) => {
      if (clientY == null) {
        pointerLeave()
        return
      }
      const root = rootRef.current
      if (!root) return
      const rect = root.getBoundingClientRect()
      // 越过刻度尺上下沿（含底部按钮避让区）不产生悬停，与真实命中一致
      if (clientY < rect.top || clientY > rect.bottom) {
        pointerLeave()
        return
      }
      schedulePointerUpdate(clientY)
    },
    externalClick: (clientY: number) => {
      const root = rootRef.current
      const rail = railRef.current
      if (!root || !rail || markerCount === 0) return
      const rect = root.getBoundingClientRect()
      if (clientY < rect.top || clientY > rect.bottom) return
      const y = Math.min(Math.max(clientY - rect.top, 0), rect.height)
      const contentY = y + rail.scrollTop - centeredOffset
      const markerIndex = Math.min(markerCount - 1, Math.max(0, Math.round((contentY - ROW_HEIGHT / 2) / ROW_HEIGHT)))
      onSelect?.(markerIndex)
    },
  }), [centeredOffset, markerCount, onSelect, pointerLeave, schedulePointerUpdate])

  React.useEffect(() => {
    const rail = railRef.current
    if (!rail) return
    const updateHeight = () => setRailHeight(rail.clientHeight)
    updateHeight()
    const observer = new ResizeObserver(updateHeight)
    observer.observe(rail)
    return () => observer.disconnect()
  }, [])

  React.useEffect(() => () => {
    if (frameRef.current != null) cancelAnimationFrame(frameRef.current)
  }, [])

  const onWheel = (event: React.WheelEvent<HTMLDivElement>) => {
    // 阻断滚轮继续冒泡到消息视口；滚轮只翻动这列用户消息横条。
    event.preventDefault()
    event.stopPropagation()
    scrollRailBy(event.deltaY)
    if (pendingPointerRef.current != null) schedulePointerUpdate(pendingPointerRef.current)
  }

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const rail = railRef.current
    if (!rail) return
    let delta: number | null = null
    if (event.key === "ArrowUp") delta = -ROW_HEIGHT
    if (event.key === "ArrowDown") delta = ROW_HEIGHT
    if (event.key === "PageUp") delta = -rail.clientHeight * 0.8
    if (event.key === "PageDown") delta = rail.clientHeight * 0.8
    if (event.key === "Home") {
      event.preventDefault()
      rail.scrollTop = 0
      setRailScrollTop(0)
      return
    }
    if (event.key === "End") {
      event.preventDefault()
      rail.scrollTop = rail.scrollHeight
      setRailScrollTop(rail.scrollTop)
      return
    }
    if (delta != null) {
      event.preventDefault()
      scrollRailBy(delta)
    }
  }

  if (markerCount === 0) return null

  return (
    <div
      ref={rootRef}
      role="navigation"
      tabIndex={0}
      aria-label="用户消息导航"
      className={cn(
        "absolute right-0 top-0 z-20 w-[54px] select-none overflow-hidden bg-background/80 outline-none backdrop-blur-sm",
        "focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-border",
        className,
      )}
      style={{ bottom: bottomInset }}
      onPointerMove={(event) => schedulePointerUpdate(event.clientY)}
      onPointerLeave={pointerLeave}
      onWheel={onWheel}
      onKeyDown={onKeyDown}
    >
      <div
        ref={railRef}
        className="absolute inset-0 overflow-y-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
        onScroll={(event) => setRailScrollTop(event.currentTarget.scrollTop)}
      >
        <div className="relative min-h-full" style={{ height: Math.max(contentHeight, railHeight, 1) }}>
          {Array.from({ length: markerCount }, (_, index) => {
            const centerY = centeredOffset + index * ROW_HEIGHT + ROW_HEIGHT / 2 - railScrollTop
            const distance = pointerY == null ? Number.POSITIVE_INFINITY : centerY - pointerY
            const depth = pointerY == null ? 0 : Math.exp(-(distance * distance) / (2 * GAUSS_SIGMA * GAUSS_SIGMA))
            const isHovered = index === hoveredMarker
            const isCurrent = index === currentMarker
            const width = BASE_WIDTH + depth * GAUSS_EXTRA_WIDTH + (isHovered ? 5 : isCurrent ? 3 : 0)

            return (
              <button
                key={index}
                type="button"
                aria-label={`跳转到第 ${index + 1} 条用户消息`}
                aria-current={isCurrent ? "location" : undefined}
                className="absolute right-2 flex w-[46px] items-center justify-end outline-none"
                style={{ top: centeredOffset + index * ROW_HEIGHT, height: ROW_HEIGHT }}
                onClick={() => onSelect?.(index)}
                onFocus={() => {
                  setHoveredMarker(index)
                  onHover?.({ markerIndex: index, y: centerY, trackH: rootRef.current?.clientHeight ?? 0 })
                }}
              >
                <span
                  className="block rounded-full"
                  style={{
                    width,
                    height: isHovered ? 3 : 2,
                    backgroundColor: isHovered
                      ? "hsl(var(--foreground) / 0.96)"
                      : isCurrent
                        ? "hsl(var(--foreground) / 0.72)"
                        : `hsl(var(--muted-foreground) / ${0.32 + depth * 0.5})`,
                    transition: "width 45ms linear, background-color 45ms linear, height 45ms linear",
                  }}
                />
              </button>
            )
          })}
        </div>
      </div>
    </div>
  )
  }
)

export function TurnPreviewCard({
  question,
  answer,
  onClick,
  className,
  style,
}: {
  question: string
  answer: string
  onClick?: () => void
  className?: string
  style?: React.CSSProperties
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "block w-[320px] max-w-[calc(100vw-5rem)] cursor-pointer overflow-hidden rounded-2xl border border-white/10 bg-neutral-800/95 px-4 py-3.5 text-left shadow-2xl backdrop-blur-xl",
        "transition-[border-color,background-color] duration-150 hover:border-white/20 hover:bg-neutral-800",
        className,
      )}
      style={style}
    >
      <div className="line-clamp-2 whitespace-pre-wrap break-words text-sm font-medium leading-5 text-neutral-100">
        {question || "(空消息)"}
      </div>
      <div className="mt-2 line-clamp-3 whitespace-pre-wrap break-words text-sm leading-5 text-neutral-400">
        {answer.trim() || "(本轮暂无文字回复)"}
      </div>
    </button>
  )
}
