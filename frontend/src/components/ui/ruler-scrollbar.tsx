import * as React from "react"
import { cn } from "@/lib/utils"

/**
 * 刻度尺式竖直滚动条：整条高度均匀铺满小横线刻度，当前视口对应区间的
 * 刻度整体转亮（内容极长时表现为一条亮横杠），形似编辑器 overview ruler。
 * 点击/拖动任意位置直接映射滚动；悬停时滚轮可直接翻动视口内容；
 * 无可滚动内容时整体隐藏。
 *
 * 节点横杠：markerTops 以淡色短杠常显（相邻过近时合并），
 * hover 吸附的节点显示为发光亮杠；通过 onHover 把指针 Y 上报给父层
 * （父层负责展示问答预览小卡片）。onLayout 在轨道尺寸变化时上报高度。
 */
interface RulerScrollbarProps {
  viewportRef: React.RefObject<HTMLDivElement | null>
  /** 刻度垂直间距（px） */
  tickSpacing?: number
  /** 各预览节点在轨道内的 y 坐标（px），由父层按文档位置换算 */
  markerTops?: number[]
  /** 当前 hover 吸附的节点下标；hover 时在该 y 处渲染亮杠标记 */
  activeMarker?: number | null
  /** hover 变化回调：y 为相对轨道顶部的像素、trackH 为轨道总高，null 表示移出/拖动中 */
  onHover?: (info: { y: number; trackH: number } | null) => void
  /** 轨道高度变化回调（挂载/窗口尺寸变化时），用于父层把文档比例换算为像素 */
  onLayout?: (trackH: number) => void
  /** 悬停刻度尺滚动滚轮时的速度倍率（相对正常滚动幅度） */
  wheelBoost?: number
  className?: string
}

// 刻度列在轨道内的位置：距左边 3px、宽 13px，右侧留白并带一根淡竖线
const TICK_LEFT = 3
const TICK_WIDTH = 13

export function RulerScrollbar({
  viewportRef,
  tickSpacing = 8,
  markerTops,
  activeMarker = null,
  onHover,
  onLayout,
  wheelBoost = 3,
  className,
}: RulerScrollbarProps) {
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

  // 轨道高度变化时上报给父层（首次挂载也触发一次）
  React.useEffect(() => {
    onLayout?.(trackH)
  }, [trackH, onLayout])

  // 悬停时刻度尺上滚动滚轮 → 直接翻动视口内容（倍速，便于超长对话跳转）。
  // React 的 onWheel 是被动监听无法 preventDefault，这里用原生监听接管。
  React.useEffect(() => {
    const root = rootRef.current
    const el = viewportRef.current
    if (!root || !el) return
    const onWheel = (e: WheelEvent) => {
      if (!(el.scrollHeight - el.clientHeight > 4)) return
      e.preventDefault()
      el.scrollBy({ top: e.deltaY * wheelBoost })
    }
    root.addEventListener("wheel", onWheel, { passive: false })
    return () => root.removeEventListener("wheel", onWheel)
  }, [viewportRef, wheelBoost, scrollable])

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
    // 开始滚动定位：预览卡片立即退场，避免视觉残留
    onHover?.(null)
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
    if (dragRef.current) {
      applyClientY(e.clientY)
      return
    }
    // 非拖动的悬停：把指针 Y 报给父层用于吸附节点与预览卡片
    if (onHover && trackH > 0) {
      const rect = rootRef.current?.getBoundingClientRect()
      if (rect) onHover({ y: Math.min(Math.max(e.clientY - rect.top, 0), trackH), trackH })
    }
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
  const idleLine = `hsl(var(--muted-foreground) / ${hovered ? 0.45 : 0.26})`
  const activeLine = `hsl(var(--foreground) / ${hovered ? 0.9 : 0.6})`
  const markerTop = hovered && activeMarker != null ? markerTops?.[activeMarker] ?? null : null

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
      onMouseLeave={() => {
        setHovered(false)
        onHover?.(null)
      }}
    >
      {/* 悬停底板：给刻度一个可感知的落区 */}
      {hovered && (
        <div className="absolute inset-y-1 right-0 w-[21px] rounded-lg bg-muted/50 shadow-[inset_0_0_0_1px_hsl(var(--border))]" />
      )}
      {/* 暗刻度轨道 */}
      <div
        className="absolute bottom-0 top-0"
        style={{
          left: TICK_LEFT,
          width: TICK_WIDTH + 3,
          backgroundImage: tickPattern(idleLine),
          backgroundSize: `${TICK_WIDTH}px 100%`,
          backgroundRepeat: 'no-repeat',
        }}
      />
      {/* 淡竖分隔线贴刻度右缘 */}
      <div className="absolute bottom-0 top-0 w-px bg-muted-foreground/15" style={{ left: TICK_LEFT + TICK_WIDTH + 2 }} />
      {/* 常显节点淡杠：每条提问在全文中的位置；相邻过近时合并避免糊成一片 */}
      {markerTops && markerTops.length > 0 && (() => {
        const bars: number[] = []
        for (const raw of markerTops) {
          const top = Math.min(Math.max(Math.round(raw), 0), Math.max(0, trackH - 1))
          if (bars.length === 0 || top - bars[bars.length - 1] >= 4) bars.push(top)
        }
        return bars.map((top, i) => (
          <div
            key={i}
            className="absolute rounded-full"
            style={{
              left: TICK_LEFT,
              width: TICK_WIDTH,
              top,
              height: 1.5,
              background: 'hsl(var(--muted-foreground) / 0.35)',
            }}
          />
        ))
      })()}
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
      {/* 吸附节点亮杠：指示 hover 命中的提问在全文中的位置 */}
      {markerTop != null && (
        <div
          className="pointer-events-none absolute rounded-full bg-primary shadow-[0_0_6px_hsl(var(--primary)/0.8)]"
          style={{ left: TICK_LEFT - 2, top: Math.round(markerTop) - 1.5, width: TICK_WIDTH + 4, height: 3 }}
        />
      )}
    </div>
  )
}

/**
 * 会话预览小卡片：上方为用户提问、下方为回答摘要，整卡可点击跳转该轮会话。
 * 独立导出以便复用与单独验证。
 */
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
        "block w-[290px] cursor-pointer rounded-xl border border-border/80 bg-background/95 p-3 text-left shadow-xl backdrop-blur-md",
        "transition-transform duration-150 hover:border-muted-foreground/40",
        className,
      )}
      style={style}
    >
      {/* 提问（前景色加粗）与回答摘要（弱化灰）以颜色区分，不加文字标签 */}
      <div className="line-clamp-2 whitespace-pre-wrap break-words text-xs font-medium leading-5 text-foreground">
        {question || '(空消息)'}
      </div>
      <div className="my-2 border-t border-border/60" />
      <div className="line-clamp-3 whitespace-pre-wrap break-words text-xs leading-5 text-muted-foreground">
        {answer.trim() || '(本轮暂无文字回复)'}
      </div>
    </button>
  )
}
