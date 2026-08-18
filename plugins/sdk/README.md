# @tiangong/plugin-sdk

天工插件 SDK：插件 UI 沙箱内访问宿主能力的统一客户端与类型定义。

## 安装

```sh
# 本地开发（仓库内引用）
yarn add @tiangong/plugin-sdk --registry file:../plugins/sdk
# 或直接把 src/index.ts 复制进工程
```

## 快速开始

插件 UI 入口（建议用 esbuild 等打包为单文件，`entry` 指向构建产物）：

```ts
import { createTiangongBridge, pluginStorage } from '@tiangong/plugin-sdk';

const bridge = await createTiangongBridge();

// 私有数据持久化（宿主落盘在插件 data 目录，无需逻辑层）
await pluginStorage.set(bridge, 'title', '我的看板');
const title = await pluginStorage.get(bridge, 'title');

// 带逻辑层的插件可调用自身 WASM 方法（plugin.* 转发）
const config = await bridge.call('plugin.getConfig', '{}');

// 事件订阅（Shadow 容器；需在 manifest capabilities.events 声明命名空间）
const off = bridge.on('session.updated', (payload) => console.log(payload));
```

## 容器适配

| 容器 | manifest 声明 | 通道 |
| --- | --- | --- |
| shadow（默认） | `"sandbox": "shadow"` 或缺省 | 宿主注入 `bridge` 参数（受限执行） |
| iframe | `"sandbox": "iframe"` | `postMessage` 协议 |
| native | 仅官方签名插件 | 官方 React 组件 |

`createTiangongBridge()` 自动探测容器并返回统一的 `HostBridge`：

```ts
interface HostBridge {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}
```

## 桥接方法命名空间

| 命名空间 | 能力 | 所需权限 |
| --- | --- | --- |
| `plugin.*` | 转发到本插件 WASM 逻辑层 | `bridge.call` |
| `storage.get/set/delete/list` | 插件私有数据读写 | `storage.private` |
| `session.*` | 按宿主版本渐进开放 | 见设计文档 6.3 |
| `tool.resolve` | Desktop TS 插件闭合自己声明的工具调用 | `tool.provide` |

负载均为字符串（JSON 序列化），宿主不做业务解析。

## Desktop TypeScript 工具提供器

纯 TypeScript 插件在 manifest 声明 `entrypoints: ["desktop"]`、`tools`、
`capabilities.tools=true`、`tool.provide` 权限和 `tool.*` 事件后，可以接收并闭合
自己声明的工具调用：

```ts
import { createTiangongBridge, createToolProvider } from '@tiangong/plugin-sdk';

const tools = createToolProvider(await createTiangongBridge());
tools.onRequested(async (invocation) => {
  await tools.resolve({
    invocation_id: invocation.invocation_id,
    status: 'answered',
    result: {
      ok: true,
      summary: JSON.stringify({ status: 'answered', result: true }),
      exit_code: 0,
    },
  });
});
tools.onClosed((closed) => console.log(closed.invocation_id, closed.status));
```

`status` 可为 `answered`、`expired` 或 `cancelled`。宿主保证调用只能闭合一次，
拒绝错插件、迟到和重复提交，并在插件未响应时按 manifest 的 `timeout_ms` 兜底。
默认交互处理器见 `plugins/interaction-handler`，它只是在这套通用协议上实现的一个
Desktop TS 工具插件。

## 主题

宿主在挂载与主题切换时推送设计 token：iframe 容器经 `tiangong_host_context`
postMessage；Shadow 容器将同名 token 写入 `:host` CSS 变量。插件 UI 直接用
`var(--background)` 等 CSS 变量即可跟随主题。
