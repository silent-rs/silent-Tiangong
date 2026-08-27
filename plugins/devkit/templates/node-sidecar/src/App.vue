<script setup lang="ts">
// {{PLUGIN_NAME}} 工具页：Agent 调用 query 工具时，本页经宿主桥接把请求
// 转发给常驻 sidecar（sidecar/main.mjs 的 demo.echo）处理并应答。
// 改造入口：handleToolCall 的 bridge.call 操作名（与 sidecar dispatch 对应）、
// plugin.json 的工具声明。
import { onBeforeUnmount, onMounted, ref } from 'vue';
import {
  createTiangongBridge,
  createToolProvider,
  type HostBridge,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';

/** 工具应答（对齐宿主 tool.resolve 的结果结构）。 */
interface ToolOutcome {
  ok: boolean;
  summary: string;
  stdout: string;
  stderr: string;
  exit_code: number;
}

const connected = ref('连接宿主桥接中…');
const calls = ref<{ time: string; name: string; summary: string; ok: boolean }[]>([]);

let bridge: HostBridge | null = null;
let providerStop: (() => void) | null = null;
let closedStop: (() => void) | null = null;

function outcome(ok: boolean, summary: string): ToolOutcome {
  return { ok, summary, stdout: '', stderr: '', exit_code: ok ? 0 : 1 };
}

function record(name: string, summary: string, ok: boolean) {
  calls.value.unshift({ time: new Date().toLocaleTimeString(), name, summary, ok });
  calls.value = calls.value.slice(0, 20);
}

/** 工具分发：转发给常驻 sidecar（bridge 的 sidecar.* 命名空间）。 */
async function handleToolCall(
  name: string,
  args: Record<string, unknown>,
): Promise<ToolOutcome> {
  if (name !== 'query') {
    return outcome(false, `未知工具 ${name}`);
  }
  try {
    const raw = await bridge!.call('sidecar.demo.echo', JSON.stringify({ text: args.text ?? '' }));
    const result = JSON.parse(raw) as { text?: string; received_at?: string };
    return outcome(true, `sidecar 回显：${result.text ?? ''}（${result.received_at ?? ''}）`);
  } catch (error) {
    return outcome(false, `sidecar 调用失败：${error instanceof Error ? error.message : String(error)}`);
  }
}

onMounted(async () => {
  try {
    bridge = await createTiangongBridge();
    connected.value = '已连接';
    const provider = createToolProvider(bridge);
    providerStop = provider.onRequested(async (invocation: ToolInvocation) => {
      const result = await handleToolCall(invocation.name, invocation.arguments as Record<string, unknown>);
      record(invocation.name, result.summary, result.ok);
      try {
        await provider.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result,
        });
      } catch (error) {
        console.warn('tool.resolve 失败', error);
      }
    });
    closedStop = provider.onClosed(() => undefined);
  } catch (error) {
    connected.value = `桥接连接失败：${error instanceof Error ? error.message : String(error)}`;
  }
});

onBeforeUnmount(() => {
  providerStop?.();
  closedStop?.();
});
</script>

<template>
  <div class="app">
    <header class="head">
      <h2>{{PLUGIN_NAME}}</h2>
      <span class="status" :data-ok="connected === '已连接'">{{ connected }}</span>
    </header>
    <p class="hint">
      本插件带一个常驻 node sidecar（sidecar/main.mjs）：Agent 调用 query 工具时，
      本页面把请求经宿主桥接转发给 sidecar 处理并应答。
    </p>
    <ul class="calls">
      <li v-for="(call, index) in calls" :key="index" :data-ok="call.ok">
        <code>{{ call.time }}</code>
        <strong>{{ call.name }}</strong>
        <span>{{ call.summary }}</span>
      </li>
    </ul>
    <p v-if="calls.length === 0" class="hint">暂无工具调用记录。</p>
  </div>
</template>

<style scoped>
.app {
  min-height: 100%;
  padding: 14px;
  box-sizing: border-box;
  font-size: 13px;
  background: hsl(var(--background, 0 0% 100%));
  color: hsl(var(--foreground, 222.2 47.4% 11.2%));
}
.head {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin-bottom: 8px;
}
h2 {
  margin: 0;
  font-size: 15px;
}
.status {
  font-size: 11px;
  color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
}
.status[data-ok='true'] {
  color: hsl(142 72% 33%);
}
.hint {
  color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
  font-size: 11px;
  margin: 8px 0;
}
.calls {
  list-style: none;
  margin: 8px 0 0;
  padding: 0;
  display: grid;
  gap: 6px;
}
.calls li {
  display: grid;
  gap: 2px;
  padding: 6px 8px;
  border-radius: var(--radius, 0.5rem);
  background: hsl(var(--muted, 210 40% 96.1%));
}
.calls li[data-ok='false'] span:last-child {
  color: hsl(0 72% 45%);
}
code {
  font-size: 10px;
  opacity: 0.8;
}
</style>
