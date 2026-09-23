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

不新增宿主窗口、不扩展 core/协议——虚拟鼠标整体是 computer-use 插件的自身能力。

**持久开关（Agent 控制）**：指针默认关闭；Agent 经 `virtual_cursor` 工具持久开关——
开启后指针出现在系统鼠标当前位置（`NSEvent.mouseLocation`）并常驻，`desktop_action`
执行时平滑移动（≈60fps 逐帧指数趋近，自带减速缓动）到目标控件中心，不自动淡出；
关闭后淡出，后续动作不再驱动指针。普通自动化不开启也不受影响。

```
Agent: virtual_cursor {enabled: true}   → sidecar → overlay 常驻显示（接管系统鼠标位置）
desktop_action(sidecar, AX 执行)
  → 执行前读取目标控件 bounds（AXPosition + AXSize）
  → 成功后 overlay::move_to(控件中心)（关闭态静默忽略）
  → 指针平滑滑动、常驻
Agent: virtual_cursor {enabled: false}  → 淡出
```

**线程模型**：AppKit 断言 NSApplication/NSWindow 只能在进程主线程使用（子线程触发
Objective-C 异常直接 abort）。sidecar `main` 把服务循环交给工作线程，主线程专跑
overlay 事件循环（手动泵 + 16ms 帧）；`move_to`/`set_enabled` 经全局 channel 非阻塞
投递，可在任意线程调用；服务退出经 `request_shutdown` 归还主线程。

overlay 窗口属性（`sidecar/src/backend/overlay.rs`，仅 macOS）：

- `NSStatusWindowLevel`（25）置顶；borderless、透明背景、无阴影
- `ignoresMouseEvents = true`：点击完全穿透，不干扰用户与目标应用
- `ActivationPolicy::Prohibited` + `orderFrontRegardless`：无 Dock 图标、永不激活、不抢焦点
- 首个 `show_at` 懒启动专用 AppKit 线程（手动泵事件循环驱动绘制与淡出）；命令经 channel 非阻塞投递，失败静默——指针是纯增益可视化，不得拖垮桌面动作

坐标换算：AX 全局坐标（主屏左上原点）→ AppKit 全局坐标（主屏左下原点），`window_origin(x, y, screen_top)` 纯函数（含单测）；bounds 零/负尺寸（读取失败默认值）不显示指针。

### 2.3 desktop_mouse：坐标级鼠标手势（CGEvent 合成）

`desktop_action`（AX 语义动作）唤不出右键菜单、没有滚轮、拖不了拽，Canvas 等自绘
UI 还没有 AX 树——坐标级合成输入是其补充。工具暴露的是**手势而非裸事件**：
`down`/`up` 永不单独出现，`drag` 由实现侧生成插值轨迹（ease-out 步进）保证拖拽
会话时序，杜绝跨工具调用的按住状态泄漏（cliclick/Playwright 同款设计）。

```
desktop_mouse { gesture: move | click | right_click | double_click | drag | scroll,
                x, y, to_x?, to_y?, delta_y?, delta_x? }
```

**系统鼠标「借用并归还」**（用户系统鼠标不被劫持）：

- `move`：纯虚拟——只动天工指针，不投递任何系统事件
- `click/right_click/double_click`：真实 HID down/up 序列（窗口激活、上下文菜单、
  菜单跟踪与真人完全一致——定向 `CGEventPostToPid` 实测无法唤出依赖 WindowServer
  状态的菜单），借用后立即把系统鼠标移回原位（≈100ms 闪回）
- `drag`：真实指针轨迹（拖拽会话必须跟随系统指针），结束归还原位
- `scroll`：无激活/菜单依赖，AX 定位坐标处进程后定向投递，系统鼠标不动

虚拟指针全程独立平滑跟随手势目标，点击瞬间播放按压脉冲动画（0→1.25→1 正弦
回弹，hotspot 始终对准目标点）。合成事件需要辅助功能授权（与 AX 同一 TCC）。

## 3. 复用路线（嵌入式浏览器）

浏览器插件的自动化（web_click 等）发生在天工窗口内的 webview，无系统鼠标参与，同样有落点可视化需求。预留：

- 指针图片直接引用本插件的 `resources/virtual-cursor.svg`（webview 内 `<img>`/内联矢量渲染）
- 浏览器侧实现归浏览器插件自身（DOM 指针元素 + 页面坐标换算），不在本 RFC 范围

## 4. 平台与限制

- macOS 实装（虚拟指针 + desktop_mouse 手势）；Windows/Linux 后续按同模式扩展
- 限制：仅主屏坐标正确（多屏待后续）；无 AX 树区域的点击兜底为 warp+归还；定向投递的滚轮在个别自绘应用可能不生效；Windows/Linux 两工具均返回不支持
