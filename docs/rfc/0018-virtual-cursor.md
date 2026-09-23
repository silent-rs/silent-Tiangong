# RFC 0018：天工虚拟指针（Agent 桌面操作可视化）

- 状态：已实施（Phase 1，macOS）
- 关联：RFC 0017（主动截图与图片注入）、`docs/computer-use-exception-analysis.md`
- 归属：`plugins/tiangong-plugin-computer-use`（实现完全位于插件内）

## 1. 动机

`desktop_action` 走 AX 语义动作（`AXPress` / `AXSetValue` 等），**系统鼠标指针纹丝不动**。Agent 连续操作桌面应用时，用户看不到任何指针活动，既无法感知「AI 正在操作哪里」，也无法在演示/审查场景跟随 Agent 的动作。需要一个虚拟指针把操作落点可视化。

## 2. 方案

### 2.1 指针形态

白底箭头 + 靛紫渐变描边（#6366f1 → #a855f7）+ 柔光阴影；右下角跟随「天工」胶囊徽标（深底白字、紫描边），明确标识这是天工 Agent 的指针而非系统鼠标。

- 真源：`resources/virtual-cursor.svg`（128×96 画布；hotspot 即箭头尖端位于 (16,12)）
- sidecar 嵌入图：`resources/virtual-cursor.png`（512×384 = 4x，RGBA 透明底），由 `resources/gen_cursor_png.swift` 按 SVG 同规格一次性生成后入库
- **复用约定**：后续嵌入式浏览器等需要同一指针时，直接引用 `resources/virtual-cursor.svg`（webview 场景矢量最佳）；两份产物改动时需保持规格一致（坐标/颜色/徽标）

### 2.2 实现位置：插件 sidecar 内自建 overlay 窗口

不新增宿主窗口、不扩展 core/协议——虚拟鼠标整体是 computer-use 插件的自身能力：

```
desktop_action(sidecar, AX 执行)
  → 执行前读取目标控件 bounds（AXPosition + AXSize）
  → 成功后 overlay::show_at(控件中心)
  → overlay 线程：NSWindow 小窗（64×48pt）显示指针，箭头尖端对准中心
  → 保持 2.4s 后 30ms/步淡出
```

overlay 窗口属性（`sidecar/src/backend/overlay.rs`，仅 macOS）：

- `NSStatusWindowLevel`（25）置顶；borderless、透明背景、无阴影
- `ignoresMouseEvents = true`：点击完全穿透，不干扰用户与目标应用
- `ActivationPolicy::Prohibited` + `orderFrontRegardless`：无 Dock 图标、永不激活、不抢焦点
- 首个 `show_at` 懒启动专用 AppKit 线程（手动泵事件循环驱动绘制与淡出）；命令经 channel 非阻塞投递，失败静默——指针是纯增益可视化，不得拖垮桌面动作

坐标换算：AX 全局坐标（主屏左上原点）→ AppKit 全局坐标（主屏左下原点），`window_origin(x, y, screen_top)` 纯函数（含单测）；bounds 零/负尺寸（读取失败默认值）不显示指针。

### 2.3 与系统输入的关系

Phase 1 不合成任何真实输入——指针纯展示，配合既有 AX 语义动作。若后续引入坐标级鼠标合成（CGEvent 合成移动/点击），指针将作为合成事件的可视化伴生（提前移动到目标、点击涟漪），届时再扩展。

## 3. 复用路线（嵌入式浏览器）

浏览器插件的自动化（web_click 等）发生在天工窗口内的 webview，无系统鼠标参与，同样有落点可视化需求。预留：

- 指针图片直接引用本插件的 `resources/virtual-cursor.svg`（webview 内 `<img>`/内联矢量渲染）
- 浏览器侧实现归浏览器插件自身（DOM 指针元素 + 页面坐标换算），不在本 RFC 范围

## 4. 平台与限制

- macOS 实装；Windows（UIA BoundingRectangle）/ Linux（AT-SPI extents）后续按同模式扩展
- Phase 1 限制：仅主屏坐标正确（多屏待后续）；指针瞬移无移动动画；无点击涟漪；Windows/Linux 不显示
