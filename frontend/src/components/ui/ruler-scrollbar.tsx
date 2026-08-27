import * as React from "react"
import { cn } from "@/lib/utils"

/**
 * 刻度线式竖直滚动条：整条高度由固定间距的短横线组成（flex 布局逐条排列，
 * 每格定高保证刚性等距），当前视口对应的区段横线转亮，形似编辑器 overview ruler。
 * 点击/拖动任意位置直接映射滚动；悬停时滚轮可直接翻动视口内容；
 * 无可滚动内容时整体隐藏。
 *
 * 节点指示：markerTops 对应的横线常显为加重色，hover 吸附的节点进一步
 * 显示为发光亮杠；onHover 把指针 Y 上报给父层展示问答预览卡片，
 * onLayout 在轨道尺寸变化时上报高度。
 */
interface RulerScrollbarProps {
  viewportRef: React.RefObject<HTMLDivElement | null>
  /** 刻度垂直间距（px），也是每根横线所在格子的固定高度 */
  tickSpacing?: number
  /** 各预览节点在轨道内的 y 坐标（px），由父层按文档位置换算 */
  markerTops?: number[]
  /** 当前 hover 吸附的节点下标；hover 时在该节点横线上渲染发光亮杠 */
  activeMarker?: number | null
  /** hover 变化回调：y 为相对轨道顶部的像素、trackH 为轨道总高，null 表示移出/拖动中 */
  onHover?: (info: { y: number; trackH: number } | null) => void
  /** 轨道高度变化回调（挂载/窗口尺寸变化时），用于父层把文档比例换算为像素 */
  onLayout?: (trackH: number) => void
  /** 悬停刻度尺滚动滚轮时的速度倍率（相对正常滚动幅度） */
  wheelBoost?: number
  className?: string
}

// 横线的宽度：普通刻度稍窄，落在刻度列右侧、朝左伸向内容方向，右缘留一根淡竖分隔线
const TICK_RIGHT = 10
const TICK_WIDTH = 13

// 正态（高斯）悬停效果参数：σ 越小中心的横线越突出；
// 中心横线在基础宽度上最多再加 GAUSS_EXTRA_WIDTH px
const GAUSS_SIGMA = 26
const GAUSS_EXTRA_WIDTH = 13

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
  const dragRef = React.useRef<{ grabOffset: number; thumbSpan: number } | null>(null)
  // 指针在轨道内的 Y（驱动正态高度变化）；仅在组件内部使用，不影响父层渲染
  const [pointerY, setPointerY] = React.useState<number | null>(null)
  const pointerYRef = React.useRef<number | null>(null)
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
      el.scrollTop += e.deltaY * wheelBoost
    }
    root.addEventListener("wheel", onWheel, { passive: false })
    return () => root.removeEventListener("wheel", onWheel)
  }, [viewportRef, wheelBoost])

  const maxScroll = Math.max(1, scrollHeight - clientHeight)
  // 视口段（thumb）高度按可见占比取值，最小一根刻度——内容极长时只亮一条线
  const thumbH = Math.min(trackH, Math.max(tickSpacing, Math.round(trackH * (clientHeight / Math.max(1, scrollHeight)))))
  const thumbTop = Math.round(
    Math.min(1, Math.max(0, scrollTop / maxScroll)) * Math.max(0, trackH - thumbH),
  )

  const applyDragRatio = (clientY: number) => {
    const el = viewportRef.current
    const drag = dragRef.current
    if (!el || !drag || trackH <= 0 || !scrollable) return
    const rect = rootRef.current?.getBoundingClientRect()
    if (!rect) return
    // 按住已有位置（thumb）时相对位移；点击空白时 thumb 中心吸附到点击处后继续拖动
    const raw = (clientY - rect.top - drag.grabOffset) / Math.max(1, trackH - drag.thumbSpan)
    const clamped = Math.min(Math.max(raw, 0), 1)
    el.scrollTop = clamped * maxScroll
  }

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!scrollable || !viewportRef.current) return
    e.preventDefault()
    // 开始滚动定位：预览卡片立即退场，避免视觉残留
    onHover?.(null)
    const rect = rootRef.current?.getBoundingClientRect()
    if (!rect) return
    const yInTrack = e.clientY - rect.top
    // 点中视口段保持相对抓取；点空白则以点击处为中心跳转过去并继续拖动
    const inThumb = yInTrack >= thumbTop && yInTrack <= thumbTop + thumbH
    const grabOffset = inThumb
      ? yInTrack - thumbTop
      : Math.round(thumbH / 2)
    dragRef.current = { grabOffset, thumbSpan: thumbH }
    e.currentTarget.setPointerCapture(e.pointerId)
    applyDragRatio(e.clientY)
  }

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRef.current) {
      applyDragRatio(e.clientY)
      return
    }
    // 非拖动的悬停：记录指针 Y（驱动正态高度变化），并把吸附信息报给父层
    if (trackH > 0) {
      const rect = rootRef.current?.getBoundingClientRect()
      if (!rect) return
      const y = Math.min(Math.max(e.clientY - rect.top, 0), trackH)
      if (y !== pointerYRef.current) {
        pointerYRef.current = y
        setPointerY(y)
      }
      onHover?.({ y, trackH })
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

  // ---- 刻度线数据：全部由 DOM 横线元素构成（flex 逐条排列，严格等距） ----
  const lineCount = Math.max(1, Math.floor(trackH / tickSpacing))

  // 当前视口占据的亮段区间（以横线索引表示）
  const viewportRatio = Math.min(1, clientHeight / Math.max(1, scrollHeight))
  const brightCount = Math.max(1, Math.round(viewportRatio * lineCount))
  const brightStart = Math.round(
    Math.min(1, Math.max(0, scrollTop / maxScroll)) * Math.max(0, lineCount - brightCount),
  )
  const brightEnd = brightStart + brightCount - 1

  // 节点横线：文档比例位置取整到最近的横线索引；active 单独记录用于发光
  const nodeLineSet = React.useMemo(() => {
    if (!(trackH > 0) || !markerTops?.length) return new Set<number>()
    const s = new Set<number>()
    for (const raw of markerTops) {
      s.add(Math.min(lineCount - 1, Math.max(0, Math.round(raw / tickSpacing))))
    }
    return s
  }, [markerTops, trackH, lineCount, tickSpacing])
  const activeNodeLine =
    hovered && activeMarker != null && markerTops?.[activeMarker] != null && trackH > 0
      ? Math.min(lineCount - 1, Math.max(0, Math.round(markerTops[activeMarker] / tickSpacing)))
      : -1

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
        pointerYRef.current = null
        setPointerY(null)
        onHover?.(null)
      }}
    >
      {/* 悬停底板：给刻度一个可感知的落区 */}
      {hovered && (
        <div className="absolute inset-y-1 right-0 w-[21px] rounded-lg bg-muted/50 shadow-[inset_0_0_0_1px_hsl(var(--border))]" />
      )}
      {/* 淡竖分隔线贴刻度右缘 */}
      <div className="absolute bottom-0 top-0 w-px bg-muted-foreground/15" style={{ right: TICK_RIGHT - 2 }} />
      {/* 刻度横线列：flex 布局逐条收纳，每格定高（tickSpacing）保证刚性等距。
          条贴窗口右缘，横线统一右对齐锚定——悬停正态加长时向左（内容一侧）伸出。
          最长最亮的线始终是指针所在的那根，向两侧平滑收敛；
          吸附到的提问节点以主题色标注 */}
      <div className="absolute bottom-0 right-0 top-0 flex flex-col items-end" style={{ paddingRight: TICK_RIGHT }}>
        {Array.from({ length: lineCount }, (_, i) => {
          const isBright = i >= brightStart && i <= brightEnd
          const isNode = nodeLineSet.has(i)
          const isActiveNode = i === activeNodeLine
          // 正态权重：中心（指针所在处）=1，向两侧平滑衰减
          let depth = 0
          if (pointerY != null) {
            const d = i * tickSpacing + tickSpacing / 2 - pointerY
            depth = Math.exp(-(d * d) / (2 * GAUSS_SIGMA * GAUSS_SIGMA))
          }
          return (
            <div key={i} className="flex shrink-0 grow-0 items-start justify-start" style={{ height: tickSpacing }}>
              <div
                className="rounded-full transition-[background-color,width] duration-150"
                style={{
                  width: (isActiveNode || isBright ? TICK_WIDTH + 2 : TICK_WIDTH) + depth * GAUSS_EXTRA_WIDTH,
                  height: 1.5,
                  backgroundColor: isActiveNode
                    ? `hsl(var(--primary) / ${Math.max(0.85, depth)})`
                    : isBright || isNode
                      ? `hsl(var(--foreground) / ${Math.max(isBright ? (hovered ? 0.9 : 0.65) : 0.5, depth * 0.92)})`
                      : `hsl(var(--muted-foreground) / ${Math.max(hovered ? 0.45 : 0.26, depth * 0.8)})`,
                }}
              />
            </div>
          )
        })}
      </div>
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
