# @tiangong/sidecar-sdk

天工插件 sidecar 协议库（Node/stdio 通道）。让 Node 脚本以天工 sidecar 身份
常驻运行：与宿主 `tiangong-plugin-runtime` 的 stdio 传输逐字段对齐（JSON Lines
帧协议、Auth 首帧认证、握手、请求/响应/进度/通知）。零依赖，Node ≥ 20。

上游为仓库本目录；plugin-creator 的 node-sidecar 模板将本目录 vendor 进插件
项目（与 ts-tool 模板的 plugin-sdk vendor 机制一致），运行时不需要网络。

## 用法

```js
import { runSidecar, SidecarError } from './vendor/tiangong-sidecar-sdk/index.mjs';

await runSidecar({
  pluginId: 'my-plugin',
  pluginVersion: '0.1.0',
  dispatch(operation, payload, ctx) {
    if (operation === 'my-plugin.greet') {
      ctx.progress('处理中');
      return { payload: { message: `hello ${payload?.name ?? 'world'}` } };
    }
    throw new SidecarError(`未知操作: ${operation}`, 'bad_request');
  },
});
```

- `dispatch` 可为异步；返回值即响应 `payload`（`{ payload }` 包装或裸值均可）。
- `ctx.progress(message)` 向宿主发送进度；`ctx.notify(channel, payload)` 发送通知。
- 业务错误抛 `SidecarError(message, code, retryable)`；其他异常按 `service_error` 返回。

## 取消与并发（0.2.0）

- 普通请求并发处理，默认上限 16，可由宿主通过 `TIANGONG_SIDECAR_MAX_CONCURRENCY` 收窄。
- 宿主使用 `cancel` 帧按 `request_id` 取消；取消不占普通请求并发名额。
- `dispatch` 的 `ctx.signal` 会在取消时触发；启动子进程的插件必须监听该信号并清理目标进程树。
- 可选 `cancel(operation, payload, ctx)` 清理钩子用于释放 PTY、子进程与外部句柄。
- 0.1.x 制品不支持请求级取消，宿主会在握手阶段明确要求升级，不会退回无沙箱模式。

## 协议要点（与宿主实现对齐）

- 帧为单行 JSON + 换行；`kind` 取 `auth` / `request` / `progress` /
  `notification` / `response` / `error`。
- 首帧必须 `auth`，token 与环境变量 `TIANGONG_PLUGIN_STDIO_TOKEN` 比对。
- 握手操作 `runtime.handshake` 由 SDK 自动应答；业务协议版本经
  `businessProtocol` 选项声明。
- stdin EOF（宿主退出/停止）即退出进程。
