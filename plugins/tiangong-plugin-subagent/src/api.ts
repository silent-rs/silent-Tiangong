// sidecar 通信封装：bridge.call("sidecar.<op>") + subagent.event 通知订阅。
import { createTiangongBridge, type HostBridge } from '@tiangong/plugin-sdk';

let bridgePromise: Promise<HostBridge> | null = null;

export function getBridge(): Promise<HostBridge> {
  bridgePromise ??= createTiangongBridge();
  return bridgePromise;
}

export async function sidecarCall<T = Record<string, unknown>>(
  operation: string,
  payload: Record<string, unknown>,
): Promise<T> {
  const bridge = await getBridge();
  const raw = await bridge.call(`sidecar.${operation}`, JSON.stringify(payload ?? {}));
  const parsed = JSON.parse(raw) as Record<string, unknown>;
  if (parsed.ok === false) {
    throw new Error(String(parsed.summary ?? '操作失败'));
  }
  return parsed as T;
}

export interface SubagentNotification {
  kind: string;
  agent_id?: string;
  run_id?: string;
  session_id?: string;
  status?: string;
}

/** 订阅 sidecar 状态通知（channel=subagent.event 经宿主 sidecar.event 到达）。 */
export async function subscribeSubagentEvents(
  handler: (notification: SubagentNotification) => void,
): Promise<() => void> {
  const bridge = await getBridge();
  return bridge.on('sidecar.event', (payload) => {
    try {
      const outer = JSON.parse(payload) as { channel?: string; payload?: string };
      if (outer.channel && outer.channel !== 'subagent.event') return;
      const body = outer.payload ? JSON.parse(outer.payload) : outer;
      handler(body as SubagentNotification);
    } catch {
      // 非法通知忽略，不影响页面
    }
  });
}
