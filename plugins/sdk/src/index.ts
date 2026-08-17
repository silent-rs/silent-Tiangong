/**
 * 天工插件 SDK：UI 沙箱内访问宿主能力的统一客户端。
 *
 * 两种容器自动适配：
 * - Shadow 容器：宿主以 `bridge` 参数注入（脚本打包后经受限执行）。
 * - iframe 容器：经 `postMessage` 协议与宿主通信（`tiangong_host_context` 提供频道）。
 *
 * 桥接方法命名空间（白名单控制）：
 * - `plugin.*`   转发到本插件 WASM 逻辑层（带逻辑层的插件）
 * - `storage.*`  插件私有数据读写：get/set/delete/list
 * - 其余命名空间（session/tool/approval/interaction）按宿主版本渐进开放。
 *
 * 用法（打包工具如 esbuild 把本 SDK 与插件代码一起构建）：
 * ```ts
 * import { createTiangongBridge } from '@tiangong/plugin-sdk';
 * const bridge = await createTiangongBridge();
 * await bridge.call('storage.set', JSON.stringify({ key: 'title', value: '看板' }));
 * ```
 */

// ── 类型定义（与宿主 tauri.ts 契约对齐）──

/** 宿主桥接：call 调用宿主能力，on 订阅宿主事件（返回取消函数）。 */
export interface HostBridge {
  call(method: string, payload: string): Promise<string>;
  on(channel: string, handler: (payload: string) => void): () => void;
}

/** App 打开模式：singleton 全局至多一个实例，multi 每次打开新建。 */
export type OpenMode = 'singleton' | 'multi';

/** UI 贡献沙箱级别。 */
export type SandboxKind = 'shadow' | 'iframe' | 'native';

/** manifest v2 `ui.contributions[]` 声明。 */
export interface UiContribution {
  slot: string;
  id: string;
  title: string;
  description?: string;
  icon?: string;
  entry: string;
  open_mode?: OpenMode;
  context?: string[];
  sandbox?: SandboxKind;
}

/** manifest v2 `capabilities` 声明。 */
export interface PluginCapabilities {
  tools: boolean;
  prompt: boolean;
  lifecycle: boolean;
  approval: boolean;
  interaction: boolean;
  events: string[];
}

/** 宿主主题上下文（iframe 容器经 postMessage 收到）。 */
export interface HostContext {
  type: 'tiangong_host_context';
  channel: string;
  theme: 'light' | 'dark';
  tokens: Record<string, string>;
  fontFamily?: string;
}

// ── 运行时 ──

/** Shadow 容器注入的桥接参数（受限执行作用域内可见）。 */
declare const bridge: HostBridge | undefined;

/** 容器类型。 */
export type TiangongContainer = 'shadow' | 'iframe' | 'unknown';

/** 探测当前运行的容器类型。 */
export function detectContainer(): TiangongContainer {
  // Shadow 容器：宿主以 bridge 参数注入脚本执行环境
  try {
    if (typeof bridge !== 'undefined' && bridge) return 'shadow';
  } catch {
    // typeof 检查不会抛出，防御性兜底
  }
  // iframe 容器：srcdoc 沙箱，window.parent 可达（沙箱 allow-scripts）
  if (typeof window !== 'undefined' && window.parent !== window) return 'iframe';
  return 'unknown';
}

/** iframe 容器的 postMessage 桥接实现。 */
class IframeBridge implements HostBridge {
  private channel: string | null = null;
  private channelWaiters: Array<() => void> = [];
  private nextCallId = 0;
  private readonly pending = new Map<string, { resolve: (v: string) => void; reject: (e: Error) => void }>();

  constructor() {
    window.addEventListener('message', (event) => {
      const data = event.data as Record<string, unknown> | null;
      if (!data) return;
      if (data.type === 'tiangong_host_context' && typeof data.channel === 'string') {
        this.channel = data.channel;
        this.channelWaiters.forEach((notify) => notify());
        this.channelWaiters = [];
        return;
      }
      if (typeof data.id === 'string' && (data.channel === this.channel)) {
        const waiter = this.pending.get(data.id);
        if (!waiter) return;
        this.pending.delete(data.id);
        if (typeof data.error === 'string') waiter.reject(new Error(data.error));
        else if (typeof data.result === 'string') waiter.resolve(data.result);
      }
    });
  }

  private async ensureChannel(): Promise<string> {
    if (this.channel) return this.channel;
    return new Promise((resolve) => {
      this.channelWaiters.push(() => resolve(this.channel!));
    });
  }

  async call(method: string, payload: string): Promise<string> {
    const channel = await this.ensureChannel();
    const id = `sdk-${++this.nextCallId}`;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      window.parent.postMessage({ type: 'plugin_call', channel, id, method, payload }, '*');
      setTimeout(() => {
        if (this.pending.delete(id)) {
          reject(new Error(`bridge.call(${method}) 超时`));
        }
      }, 30_000);
    });
  }

  on(_channel: string, _handler: (payload: string) => void): () => void {
    // iframe 容器的事件推送通道随宿主版本开放，当前返回空取消函数
    console.warn('[tiangong-plugin-sdk] 事件订阅在 iframe 容器暂不可用');
    return () => {};
  }
}

/**
 * 创建宿主桥接客户端（自动适配容器）。
 * iframe 容器下首次调用会等待宿主推送频道（挂载即送达，通常立即就绪）。
 */
export async function createTiangongBridge(): Promise<HostBridge> {
  const container = detectContainer();
  if (container === 'shadow') {
    return bridge as HostBridge;
  }
  if (container === 'iframe') {
    return new IframeBridge();
  }
  throw new Error('未检测到天工容器（不在插件沙箱内运行）');
}

/** 便捷的存储读写（storage.* 封装，值为字符串）。 */
export const pluginStorage = {
  async get(bridge: HostBridge, key: string): Promise<string | null> {
    const result = await bridge.call('storage.get', JSON.stringify({ key }));
    return result === 'null' ? null : JSON.parse(result) as string;
  },
  async set(bridge: HostBridge, key: string, value: string): Promise<void> {
    await bridge.call('storage.set', JSON.stringify({ key, value }));
  },
  async remove(bridge: HostBridge, key: string): Promise<void> {
    await bridge.call('storage.delete', JSON.stringify({ key }));
  },
  async keys(bridge: HostBridge): Promise<string[]> {
    const result = await bridge.call('storage.list', '{}');
    return JSON.parse(result) as string[];
  },
};
