# RFC 0012：嵌入式浏览器智能元素定位

> 状态：已实现
> 创建：2026-06-07
> 分支：`feature/browser-observer-rfc0012`
> 关联 Issue：#107 智能元素定位

## 1. 背景

当前浏览器工具主要依赖 CSS selector。面对动态 class、无 id/name 的按钮、链接、输入框或表格操作目标时，Agent 很难稳定定位元素，容易要求用户提供选择器，或点击失败。

#107 的目标是让 `web_click` / `web_form_fill` 可以接受更接近人类描述的定位输入，例如“登录按钮”“包含提交的按钮”“邮箱输入框”“表格第三行第二列的链接”，由 Bridge Script 自动选择合适的定位策略。

## 2. 范围

本阶段只实现智能元素定位，不实现 #106 的操作等待机制，也不实现浏览器实时观测层。

交付范围：

- `clickElement` / `fillField` 支持 CSS selector、文本、ARIA label、role、label、placeholder/name 和简单表格坐标定位。
- `web_click` / `web_form_fill` 的 `selector` 参数说明改为“定位描述”，保持参数名兼容旧调用。
- 多个候选匹配时返回候选列表，供 Agent 选择更精确目标。
- `web_form_extract` 返回更稳定的 selector，优先使用 id/name，其次使用文本/ARIA/label 辅助生成的路径。

非目标：

- 不新增 `wait_for` 参数。
- 不监听或注入网络响应。
- 不做持久 MutationObserver 事件流。

## 3. 定位策略

`locateElement(query, options)` 按以下顺序定位：

1. CSS selector：当 query 是有效 selector 且命中元素时直接使用。
2. 显式 DSL：支持 `text=提交`、`role=button[name=登录]`、`aria=关闭`、`label=邮箱`、`placeholder=请输入邮箱`。
3. 表单定位：通过 `label`、`placeholder`、`name`、`aria-label` 找到 input/textarea/select 或 UI 库控件容器。
4. 交互元素文本定位：在 button、a、role button/link/tab、summary、可点击元素中匹配可见文本或 accessible name。
5. 表格坐标定位：支持“表格第三行第二列的链接/按钮”这类描述。

候选评分考虑元素类型、可见性、文本精确度、ARIA/label 精确度、是否 disabled 等因素。若最高分与第二名差距不足，返回候选列表。

## 4. 工具行为

`web_click`：

- 输入仍为 `selector`，但可传 CSS selector 或自然语言定位描述。
- 找到唯一目标时执行点击，返回实际 selector、元素文本和坐标。
- 多候选或无匹配时返回候选列表，不执行点击。

`web_form_fill`：

- 输入仍为 `selector`、`value`、`strategy`。
- 定位时优先匹配可填写元素；支持 label/placeholder/name/ARIA。
- 找到唯一目标后沿用现有三层填写策略。
- 原生填写失败时继续尝试 UI 库组件填写。

## 5. 验收

- Agent 能通过“登录按钮”而非 `#login` 点击按钮。
- 无 id/class 的元素可通过文本内容定位。
- 表单字段可通过 label 或 placeholder 填写。
- 多个“提交”按钮时工具返回候选列表，而不是盲目点击。
