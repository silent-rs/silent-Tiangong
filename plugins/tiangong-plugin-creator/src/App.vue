<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref } from 'vue';
import {
  createTiangongBridge,
  createToolProvider,
  pluginStorage,
  type HostBridge,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';
import { handleAgentTool, pluginDev, type ProjectEntry } from './tools';

type OutputKind = 'info' | 'success' | 'error' | 'log';
interface OutputPanel {
  kind: OutputKind;
  title: string;
  body: string;
}

const TEMPLATES = [
  {
    id: 'ui-app',
    title: '纯 UI 插件',
    description: '零构建依赖的单页面插件（看板、仪表盘类），改完即构建。',
  },
  {
    id: 'ts-tool',
    title: 'TS 工具插件',
    description: '页面 + 向 Agent 提供工具（interaction 同款结构），需要 Node 与 yarn。',
  },
] as const;

const bridge = ref<HostBridge | null>(null);
const connected = ref('正在连接宿主桥接…');
const projects = ref<ProjectEntry[]>([]);
const template = ref<string>('ui-app');
const newId = ref('');
const newName = ref('');
const busy = ref(false);
const output = ref<OutputPanel | null>(null);

let providerStop: (() => void) | null = null;
let closedStop: (() => void) | null = null;

function show(kind: OutputKind, title: string, body: string) {
  output.value = { kind, title, body };
}

async function refreshProjects() {
  if (!bridge.value) return;
  try {
    projects.value = await pluginDev.list(bridge.value);
  } catch (error) {
    show('error', '项目列表读取失败', error instanceof Error ? error.message : String(error));
  }
}

async function createProject() {
  if (!bridge.value || busy.value) return;
  const id = newId.value.trim();
  if (!id) {
    show('error', '缺少插件 ID', '请填写插件 ID（字母数字与 - _ .）');
    return;
  }
  busy.value = true;
  try {
    const result = await pluginDev.init(bridge.value, {
      template: template.value,
      id,
      name: newName.value.trim() || undefined,
    });
    show(
      'success',
      `项目 ${result.plugin_id} 已创建`,
      `目录：${result.directory}\n模板：${result.template}，共 ${result.files} 个文件。\n可以让 Agent 在该目录中继续开发，或自行编辑后构建。`,
    );
    newId.value = '';
    newName.value = '';
    await refreshProjects();
  } catch (error) {
    show('error', '创建失败', error instanceof Error ? error.message : String(error));
  } finally {
    busy.value = false;
  }
}

async function runAction(action: 'validate' | 'build' | 'install', entry: ProjectEntry) {
  if (!bridge.value || busy.value) return;
  busy.value = true;
  const labels = { validate: '校验', build: '构建', install: '安装' } as const;
  try {
    if (action === 'validate') {
      const result = await pluginDev.validate(bridge.value, entry.id);
      const body = [
        ...result.errors.map((item) => `错误：${item}`),
        ...result.warnings.map((item) => `提示：${item}`),
      ].join('\n');
      show(
        result.ok ? 'success' : 'error',
        `${entry.id} 校验${result.ok ? '通过' : '未通过'}`,
        body || `清单 ${result.id} v${result.version}，权限 [${result.permissions.join('、') || '无'}]`,
      );
    } else if (action === 'build') {
      const result = await pluginDev.build(bridge.value, entry.id);
      show(
        'success',
        `${entry.id} 构建完成（${(result.duration_ms / 1000).toFixed(1)}s）`,
        `产物：${result.release_dir}\n\n日志尾部：\n${result.log_tail}`,
      );
    } else {
      const result = await pluginDev.install(bridge.value, entry.id);
      show(
        'success',
        `${result.plugin_id} v${result.version} 已安装`,
        `状态：${result.state}${result.enabled ? '（已启用）' : ''}。含 extension.tab 贡献时可在拓展区打开。`,
      );
      await recordHistory(`安装 ${result.plugin_id} v${result.version}`);
    }
    await refreshProjects();
  } catch (error) {
    show('error', `${entry.id} ${labels[action]}失败`, error instanceof Error ? error.message : String(error));
  } finally {
    busy.value = false;
  }
}

async function showBuildLog(entry: ProjectEntry) {
  if (!bridge.value) return;
  try {
    const result = await pluginDev.logs(bridge.value, `dev:${entry.id}`);
    show('log', `${entry.id} 构建日志尾部`, result.lines.join('\n'));
  } catch (error) {
    show('error', '日志读取失败', error instanceof Error ? error.message : String(error));
  }
}

function versionLabel(entry: ProjectEntry): string {
  const parts = [
    entry.source_version ? `源码 ${entry.source_version}` : null,
    entry.release_version ? `构建 ${entry.release_version}` : '未构建',
    entry.installed_version ? `已安装 ${entry.installed_version}` : '未安装',
  ].filter(Boolean);
  return parts.join(' · ');
}

onMounted(async () => {
  try {
    bridge.value = await createTiangongBridge();
    connected.value = '已连接';
    const provider = createToolProvider(bridge.value);
    providerStop = provider.onRequested(async (invocation: ToolInvocation) => {
      const result = await handleAgentTool(
        bridge.value!,
        invocation.name,
        invocation.arguments as Record<string, unknown>,
      );
      try {
        await provider.resolve({
          invocation_id: invocation.invocation_id,
          status: 'answered',
          result,
        });
      } catch (error) {
        // 迟到/重复闭合由宿主拒绝，属兜底路径，不打断页面。
        console.warn('tool.resolve 失败', error);
      }
    });
    closedStop = provider.onClosed(() => undefined);
    await refreshProjects();
  } catch (error) {
    connected.value = `桥接连接失败：${error instanceof Error ? error.message : String(error)}`;
  }
});

onBeforeUnmount(() => {
  providerStop?.();
  closedStop?.();
});

// 安装历史落插件私有存储（storage.private 权限的实际消费），供追溯。
async function recordHistory(text: string) {
  if (!bridge.value) return;
  try {
    const raw = await pluginStorage.get(bridge.value, 'install_history');
    const history = raw ? (JSON.parse(raw) as string[]) : [];
    history.unshift(`${new Date().toISOString().slice(0, 19).replace('T', ' ')} ${text}`);
    await pluginStorage.set(bridge.value, 'install_history', JSON.stringify(history.slice(0, 50)));
  } catch {
    // 历史记录是尽力而为的辅助信息，失败不影响主流程。
  }
}
</script>

<template>
  <div class="app">
    <header class="head">
      <h2>插件创作</h2>
      <span class="status" :data-ok="connected === '已连接'">{{ connected }}</span>
    </header>

    <section class="create card">
      <h3>新建插件项目</h3>
      <div class="templates">
        <button
          v-for="item in TEMPLATES"
          :key="item.id"
          class="template"
          :class="{ active: template === item.id }"
          type="button"
          @click="template = item.id"
        >
          <strong>{{ item.title }}</strong>
          <span>{{ item.description }}</span>
          <code>{{ item.id }}</code>
        </button>
      </div>
      <div class="row">
        <input v-model="newId" placeholder="插件 ID（如 my-dashboard）" />
        <input v-model="newName" placeholder="显示名（可选）" />
        <button class="primary" type="button" :disabled="busy" @click="createProject">创建</button>
      </div>
      <p class="hint">项目生成在 ~/.tiangong/plugins-dev/&lt;id&gt;/，可让 Agent 在对话中继续开发，再回到本页构建安装。</p>
    </section>

    <section class="card">
      <h3>项目列表</h3>
      <p v-if="projects.length === 0" class="hint">暂无项目。创建一个，或让 Agent 调用 plugin_init 工具。</p>
      <ul v-else class="projects">
        <li v-for="entry in projects" :key="entry.id">
          <div class="meta">
            <strong>{{ entry.name }}</strong>
            <code>{{ entry.id }}</code>
            <span class="badge">{{ entry.template }}</span>
            <span class="versions">{{ versionLabel(entry) }}</span>
          </div>
          <div class="actions">
            <button type="button" :disabled="busy" @click="runAction('validate', entry)">校验</button>
            <button type="button" :disabled="busy" @click="runAction('build', entry)">构建</button>
            <button type="button" :disabled="busy" @click="runAction('install', entry)">安装</button>
            <button type="button" class="ghost" @click="showBuildLog(entry)">日志</button>
          </div>
        </li>
      </ul>
    </section>

    <section v-if="output" class="card" :class="`out-${output.kind}`">
      <h3>{{ output.title }}</h3>
      <pre>{{ output.body }}</pre>
    </section>
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
  overflow: auto;
}
.head { display: flex; align-items: baseline; gap: 10px; margin-bottom: 12px; }
.head h2 { margin: 0; font-size: 15px; }
.status { font-size: 11px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.status[data-ok='true'] { color: hsl(var(--green, 142.1 76.2% 36.3%)); }
.card {
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  border-radius: var(--radius, 0.5rem);
  padding: 12px;
  margin-bottom: 12px;
}
.card h3 { margin: 0 0 10px; font-size: 13px; }
.templates { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-bottom: 10px; }
.template {
  display: flex; flex-direction: column; gap: 4px; align-items: flex-start;
  padding: 10px; text-align: left; cursor: pointer;
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  border-radius: var(--radius, 0.5rem);
  background: transparent; color: inherit;
}
.template.active {
  border-color: hsl(var(--primary, 222.2 47.4% 11.2%));
  box-shadow: 0 0 0 1px hsl(var(--primary, 222.2 47.4% 11.2%));
}
.template span { color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); font-size: 11px; }
.template code { font-size: 10px; opacity: 0.7; }
.row { display: flex; gap: 8px; }
.row input {
  flex: 1; padding: 6px 8px;
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  border-radius: var(--radius, 0.5rem);
  background: transparent; color: inherit;
}
button {
  padding: 6px 12px; border: none; border-radius: var(--radius, 0.5rem);
  background: hsl(var(--secondary, 210 40% 96.1%));
  color: hsl(var(--secondary-foreground, 222.2 47.4% 11.2%));
  cursor: pointer; font-size: 12px;
}
button:disabled { opacity: 0.5; cursor: not-allowed; }
button.primary {
  background: hsl(var(--primary, 222.2 47.4% 11.2%));
  color: hsl(var(--primary-foreground, 210 40% 98%));
}
button.ghost { background: transparent; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.hint { margin: 8px 0 0; font-size: 11px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.projects { list-style: none; padding: 0; margin: 0; }
.projects li {
  display: flex; justify-content: space-between; gap: 10px; align-items: center;
  padding: 8px; margin-bottom: 6px;
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  border-radius: var(--radius, 0.5rem);
}
.meta { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.meta .badge {
  padding: 1px 6px; font-size: 10px; border-radius: 999px;
  border: 1px solid hsl(var(--border, 214.3 31.8% 91.4%));
  color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
}
.meta .versions { font-size: 11px; color: hsl(var(--muted-foreground, 215.4 16.3% 46.9%)); }
.actions { display: flex; gap: 6px; flex-shrink: 0; }
pre {
  margin: 0; padding: 10px; max-height: 300px; overflow: auto;
  border-radius: var(--radius, 0.5rem);
  background: hsl(var(--muted, 210 40% 96.1%));
  font-size: 11px; white-space: pre-wrap; word-break: break-all;
}
.out-error h3 { color: hsl(var(--red, 0 84.2% 60.2%)); }
.out-success h3 { color: hsl(var(--green, 142.1 76.2% 36.3%)); }
</style>
