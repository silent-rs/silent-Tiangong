<script setup lang="ts">
// ts-tool 模板页面：展示收到的工具调用记录，并在此分发工具处理逻辑。
// Agent 调用你在 plugin.json tools 里声明的工具时，宿主会把调用推送到
// 这里（无订阅者时会自动后台拉起本页面），处理完经 provider.resolve 回传。
import { onMounted, onBeforeUnmount, ref } from 'vue';
import {
  createTiangongBridge,
  createToolProvider,
  type HostBridge,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';

interface ToolOutcome {
  ok: boolean;
  summary: string;
  stdout: string;
  stderr: string;
  exit_code: number;
}

interface CallRecord {
  time: string;
  name: string;
  summary: string;
}

const connected = ref('正在连接宿主桥接…');
const calls = ref<CallRecord[]>([]);

let bridge: HostBridge | null = null;
let providerStop: (() => void) | null = null;
let closedStop: (() => void) | null = null;

function ok(summary: string): ToolOutcome {
  return { ok: true, summary, stdout: '', stderr: '', exit_code: 0 };
}

function fail(summary: string): ToolOutcome {
  return { ok: false, summary, stdout: '', stderr: '', exit_code: 1 };
}

/** 工具分发：按 plugin.json 声明的工具实现处理逻辑（示例 hello）。 */
async function handleToolCall(name: string, args: Record<string, unknown>): Promise<ToolOutcome> {
  switch (name) {
    case 'hello': {
      const who = typeof args.name === 'string' && args.name ? args.name : '天工';
      return ok(`你好，${who}！这是 {{PLUGIN_ID}} 的第一个工具。`);
    }
    default:
      return fail(`未知工具 ${name}`);
  }
}

onMounted(async () => {
  try {
    bridge = await createTiangongBridge();
    connected.value = '已连接';
    const provider = createToolProvider(bridge);
    providerStop = provider.onRequested(async (invocation: ToolInvocation) => {
      const result = await handleToolCall(
        invocation.name,
        invocation.arguments as Record<string, unknown>,
      );
      calls.value.unshift({
        time: new Date().toLocaleTimeString(),
        name: invocation.name,
        summary: result.summary,
      });
      calls.value = calls.value.slice(0, 20);
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
      这是 ts-tool 模板页面：Agent 调用本插件工具（如 hello）时，调用会推送到这里处理并展示在下方的调用记录中。
      改造入口：plugin.json（工具声明）与 src/App.vue（handleToolCall）。
    </p>
    <ul class="calls">
      <li v-for="(call, index) in calls" :key="index">
        <code>{{ call.time }}</code>
        <strong>{{ call.name }}</strong>
        <span>{{ call.summary }}</span>
      </li>
    </ul>
    <p v-if="calls.length === 0" class="hint">暂无工具调用记录。</p>
  </div>
</template>

<style scoped>
* { box-sizing: border-box; }
.app {
  min-height: 100%;
  padding: 14px;
  font-family: inherit;
  font-size: 13px;
  background: hsl(var(--background, 0 0% 100%));
  color: hsl(var(--foreground, 222.2 47.4% 11.2%));
}
.head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 8px; }
.head h2 { margin: 0; font-size: 15px; }
.status { font-size: 11px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.status[data-ok='true'] { color: hsl(var(--green, 142.1 76.2% 36.3%)); }
.hint { font-size: 11px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.calls { list-style: none; padding: 0; margin: 10px 0 0; }
.calls li {
  display: flex; gap: 8px; align-items: baseline; padding: 6px 8px; margin-bottom: 6px;
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  border-radius: var(--radius, 0.5rem);
}
.calls code { font-size: 10px; opacity: 0.7; }
</style>
