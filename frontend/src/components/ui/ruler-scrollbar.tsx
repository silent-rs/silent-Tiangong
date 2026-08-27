import * as React from "react"
import { cn } from "@/lib/utils"

/**
 * 刻度尺式竖直滚动条：整条高度均匀铺满小横线刻度，当前视口对应区间的
 * 刻度整体转亮（内容极长时表现为一条亮横杠），形似编辑器 overview ruler。
 * 点击/拖动任意位置直接映射滚动；无可滚动内容时整体隐藏。
 */
interface RulerScrollbarProps {
  viewportRef: React.RefObject<HTMLDivElement | null>
  /** 刻度垂直间距（px） */
  tickSpacing?: number
  className?: string
}

// 刻度列在轨道内的位置：距左边 3px、宽 13px，右侧留白并带一根淡竖线
const TICK_LEFT = 3
const TICK_WIDTH = 13

export function RulerScrollbar({ viewportRef, tickSpacing = 8, className }: RulerScrollbarProps) {
  const rootRef = React.useRef<HTMLDivElement>(null)
  const dragRef = React.useRef<{ grabOffset: number } | null>(null)
  const [hovered, setHovered] = React.useState(false)
  const [dims, setDims] = React.useState({ trackH: 0, scrollTop: 0, scrollHeight: 0, clientHeight: 0 })

  React.useEffect(() => {
    const el = viewportRef.current
    if (!el) return
    const update = () => {
      setDims(prev => {
        const next = {
          trackH: rootRef.current?.clientHeight ?? prev.trackH,
          scrollTop: el.scrollTop,
          scrollHeight: el.scrollHeight,
          clientHeight: el.clientHeight,
        }
        return (prev.trackH === next.trackH &&
          prev.scrollTop === next.scrollTop &&
          prev.scrollHeight === next.scrollHeight &&
          prev.clientHeight === next.clientHeight)
          ? prev
          : next
      })
    }
    update()
    el.addEventListener("scroll", update, { passive: true })
    // 视口尺寸与内容高度都要盯住：流式追加消息时只有子容器会长高
    const ro = new ResizeObserver(update)
    ro.observe(el)
    if (el.firstElementChild) ro.observe(el.firstElementChild)
    return () => {
      el.removeEventListener("scroll", update)
      ro.disconnect()
    }
  }, [viewportRef])

  const { trackH, scrollTop, scrollHeight, clientHeight } = dims
  const scrollable = trackH > 0 && scrollHeight - clientHeight > 4

  const maxScroll = Math.max(1, scrollHeight - clientHeight)
  // thumb 高度按视口占比取值，最小一格刻度——内容极长时恰好只亮一条线
  const thumbH = Math.min(trackH, Math.max(tickSpacing, Math.round(trackH * (clientHeight / Math.max(1, scrollHeight)))))
  const thumbTop = Math.round(Math.min(1, Math.max(0, scrollTop / maxScroll)) * (trackH - thumbH))

  const applyClientY = (clientY: number) => {
    const el = viewportRef.current
    const drag = dragRef.current
    if (!el || !drag || trackH <= 0) return
    const rect = rootRef.current?.getBoundingClientRect()
    if (!rect) return
    const top = Math.min(
      Math.max(clientY - rect.top - drag.grabOffset, 0),
      trackH - thumbH,
    )
    el.scrollTop = (top / Math.max(1, trackH - thumbH)) * maxScroll
  }

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!scrollable || !viewportRef.current) return
    e.preventDefault()
    const rect = rootRef.current?.getBoundingClientRect()
    if (!rect) return
    const yInTrack = e.clientY - rect.top
    // 点中 thumb 保持抓取偏移；点空白则跳转过去并以 thumb 中心吸附后继续拖动
    const grabOffset = yInTrack >= thumbTop && yInTrack <= thumbTop + thumbH
      ? yInTrack - thumbTop
      : Math.round(thumbH / 2)
    dragRef.current = { grabOffset }
    e.currentTarget.setPointerCapture(e.pointerId)
    applyClientY(e.clientY)
  }

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRef.current) applyClientY(e.clientY)
  }

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRef.current) {
      dragRef.current = null
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId)
      }
    }
  }

  // 两层用同一纹样、相位对齐（亮层按自身 top 反向平移背景），
  // 亮层盒子裁出当前视口区间 → 区间内的刻度变亮
  const tickPattern = (lineColor: string) =>
    `repeating-linear-gradient(to bottom, ${lineColor} 0px, ${lineColor} 1px, transparent 1px, transparent ${tickSpacing}px)`
  const idleLine = `hsl(var(--muted-foreground) / ${hovered ? 0.38 : 0.26})`
  const activeLine = `hsl(var(--foreground) / ${hovered ? 0.85 : 0.6})`

  return (
    <div
      ref={rootRef}
      role="scrollbar"
      aria-orientation="vertical"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round((scrollTop / maxScroll) * 100)}
      aria-label="会话滚动条"
      className={cn(
        "absolute inset-y-0 right-0 z-20 w-[26px] touch-none select-none transition-opacity duration-150",
        scrollable ? "opacity-100" : "pointer-events-none opacity-0",
        hovered ? "cursor-grabbing" : "cursor-pointer",
        className,
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      {/* 暗刻度轨道 */}
      <div
        className="absolute bottom-0 top-0 border-r border-muted-foreground/15"
        style={{
          left: TICK_LEFT,
          width: TICK_WIDTH,
          backgroundImage: tickPattern(idleLine),
        }}
      />
      {/* 亮 thumb 层：overflow 裁出视口区间，内部纹样与全局相位对齐 */}
      <div
        className="absolute overflow-hidden"
        style={{
          left: TICK_LEFT,
          width: TICK_WIDTH,
          top: thumbTop,
          height: thumbH,
          backgroundImage: tickPattern(activeLine),
          backgroundPositionY: `${-thumbTop}px`,
        }}
      />
    </div>
  )
}
