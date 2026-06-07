# 浏览器增强

天工嵌入式浏览器的架构设计与演进方案。

## 文档索引

| 文档 | 状态 | 说明 |
|------|------|------|
| [01-plugin-extraction.md](./01-plugin-extraction.md) | ✅ 已实现 | 浏览器能力从主应用拆分为独立 Tauri Plugin crate |
| [02-smart-element-location.md](./02-smart-element-location.md) | ✅ 已实现 | 智能元素定位：文本匹配、ARIA、批注矩形、序号选择 |
| [03-observer-design.md](./03-observer-design.md) | 📋 草案 | 持久观测层：MutationObserver + 用户行为追踪 + 事件推送 |
| [branch-comparison.md](./branch-comparison.md) | 📊 参考 | `feature/smart-element-location` 与 `feature/browser-observer-rfc0012` 分支对比 |

## 已完成的能力

- **Phase 21**：内嵌浏览器面板（用户浏览 + Agent 操控 + Cookie 持久化）
- **Phase 21-G**：浏览器能力插件化（`tiangong-plugin-browser` crate）
- **智能元素定位**：`locateElement` / `locateAll` 多策略定位
- **操作前后对比**：`getPageDigest` / `diffDigest` 语义化差异
- **条件等待**：`waitFor` 支持 navigation / element / stable
- **DOM 查询**：`web_query_dom` CSS 选择器查询
- **UI 库适配**：Ant Design / Element Plus 组件提取与填写
- **批注模式**：canvas 覆盖层绘制 + Agent 读取

## 下一步

按 `03-observer-design.md` 实施持久观测层，使 Agent 从"被动操作"升级为"主动感知"。
