# 交互处理器插件（Interaction Handler）

这个纯 UI 插件演示如何接管天工的审批、确认、单选、多选、文本输入和表单请求。

## 使用

1. 复制整个目录并修改 `plugin.json` 中的 `id`、标题和版本。
2. 在天工「设置 → 插件管理 → 导入本地插件」选择这个目录。
3. 保留以下声明：
   - `capabilities.interaction: true`
   - `capabilities.events: ["interaction.*"]`
   - `permissions: ["interaction.handle"]`
   - `slot: "session.interaction"`
4. 重新打开天工。Agent 调用 `request_user` 时，宿主会把请求交给该插件。

## 协议

- 订阅 `interaction.requested`：负载包含请求 ID、会话、六种 kind、内容、创建时间和后端权威 deadline。
- 订阅 `interaction.closed`：负载包含 `answered`、`expired` 或 `cancelled` 最终状态。
- 调用 `interaction.resolve`：

```json
{
  "request_id": "请求 ID",
  "result_json": "序列化后的 JSON 响应"
}
```

插件只负责界面和提交。会话归属、截止时间、唯一响应、审批挑战及授权均由宿主验证。即使插件篡改文案或在过期后提交，也不能直接授权受保护操作。

作为默认交互处理器真实使用：处理 request_user 发起的审批、确认、选择与输入请求。零构建可直接导入；工程化扩展可使用 `@tiangong/plugin-sdk` 的 `createInteractionHandler()`。
