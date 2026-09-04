<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import type { HostContext } from '@tiangong/plugin-sdk';
import {
  BACKEND_IMPLEMENTED,
  BACKEND_LABELS,
  RUN_STATUS_LABELS,
  TASK_STATUS_LABELS,
  WORKSPACE_POLICY_LABELS,
  type AgentSummary,
  type StateSnapshot,
} from './types';
import { sidecarCall, subscribeSubagentEvents } from './api';
import AgentForm, { type AgentFormValue } from './components/AgentForm.vue';

interface Props {
  initialHostContext?: HostContext;
  subscribeHostContext?: (handler: (context: HostContext) => void) => () => void;
  shadowContainer?: boolean;
}

const props = withDefaults(defineProps<Props>(), {
  initialHostContext: undefined,
  subscribeHostContext: undefined,
  shadowContainer: false,
});

type FilterKind = 'all' | 'session' | 'running' | 'waiting' | 'stopped';

const FILTERS: Array<{ key: FilterKind; label: string }> = [
  { key: 'all', label: '全部' },
  { key: 'session', label: '当前会话' },
  { key: 'running', label: '运行中' },
  { key: 'waiting', label: '等待处理' },
  { key: 'stopped', label: '已停用' },
];

const snapshot = ref<StateSnapshot | null>(null);
const filter = ref<FilterKind>('all');
const sessionContext = ref<{ id: string; workspace: string } | null>(null);
const connectionError = ref('');
const busyAgentId = ref<string | null>(null);
const messageDraft = ref<Record<string, string>>({});
const expandedAgentId = ref<string | null>(null);
const formVisible = ref(false);
const formInitial = ref<AgentFormValue | null>(null);
const formError = ref('');

let stopEvents: (() => void) | null = null;
let stopHostContext: (() => void) | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | null = null;
let disposed = false;

const sessionId = computed(() => sessionContext.value?.id ?? '');
const workspace = computed(() => sessionContext.value?.workspace ?? '');

function applyContext(context?: HostContext) {
  const session = context?.session;
  sessionContext.value = session?.id
    ? { id: session.id, workspace: session.workspace ?? '' }
    : null;
}

const filteredAgents = computed<AgentSummary[]>(() => {
  const agents = snapshot.value?.agents ?? [];
  switch (filter.value) {
    case 'session':
      return agents.filter((agent) => agent.activated_in_session);
    case 'running':
      return agents.filter((agent) => agent.runtime_status === 'working');
    case 'waiting':
      return agents.filter((agent) =>
        agent.runtime_status === 'blocked' || agent.runtime_status === 'approval_required');
    case 'stopped':
      return agents.filter((agent) => !agent.config.enabled);
    default:
      return agents;
  }
});

const waitingCount = computed(() =>
  (snapshot.value?.agents ?? []).filter((agent) =>
    agent.runtime_status === 'blocked' || agent.runtime_status === 'approval_required').length);

const runningCount = computed(() =>
  (snapshot.value?.agents ?? []).filter((agent) => agent.runtime_status === 'working').length);

function sessionActivation(agent: AgentSummary) {
  return agent.activations.find((item) => item.session_id === sessionId.value);
}

function runtimeStatusLabel(agent: AgentSummary): string {
  return agent.runtime_status ? RUN_STATUS_LABELS[agent.runtime_status] : '空闲';
}

function runtimeStatusTone(agent: AgentSummary): string {
  switch (agent.runtime_status) {
    case 'working':
      return 'tone-working';
    case 'blocked':
    case 'approval_required':
      return 'tone-waiting';
    case 'failed':
      return 'tone-failed';
    case 'completed':
      return 'tone-completed';
    default:
      return 'tone-idle';
  }
}

function tasksOf(agent: AgentSummary) {
  const active = snapshot.value?.active_tasks ?? [];
  return active.filter((task) => task.agent_id === agent.config.id);
}

function eventsOf(agent: AgentSummary) {
  const events = snapshot.value?.recent_events ?? [];
  return events.filter((event) => event.agent_id === agent.config.id).slice(0, 12);
}

function eventText(payload: Record<string, unknown>): string {
  const text = payload?.text;
  if (typeof text === 'string') return text;
  const status = payload?.status;
  return typeof status === 'string' ? status : '';
}

async function refresh() {
  try {
    const body = await sidecarCall<StateSnapshot>('ui_state_snapshot', {
      session_id: sessionId.value,
    });
    if (!disposed) {
      snapshot.value = body;
      connectionError.value = '';
    }
  } catch (error) {
    if (!disposed) connectionError.value = String((error as Error).message ?? error);
  }
}

function scheduleRefresh() {
  if (refreshTimer || disposed) return;
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    void refresh();
  }, 300);
}

async function withBusy(agentId: string, action: () => Promise<unknown>) {
  busyAgentId.value = agentId;
  try {
    await action();
    await refresh();
  } catch (error) {
    connectionError.value = String((error as Error).message ?? error);
  } finally {
    busyAgentId.value = null;
  }
}

function activate(agent: AgentSummary) {
  if (!sessionContext.value) {
    connectionError.value = '缺少当前会话上下文，无法激活';
    return;
  }
  void withBusy(agent.config.id, () => sidecarCall('ui_activate', {
    agent_id: agent.config.id,
    session_id: sessionId.value,
    workspace: workspace.value,
  }));
}

function deactivate(agent: AgentSummary) {
  void withBusy(agent.config.id, () => sidecarCall('ui_deactivate', {
    agent_id: agent.config.id,
    session_id: sessionId.value,
  }));
}

function sendMessage(agent: AgentSummary) {
  const content = (messageDraft.value[agent.config.id] ?? '').trim();
  if (!content || !sessionContext.value) return;
  messageDraft.value[agent.config.id] = '';
  void withBusy(agent.config.id, () => sidecarCall('ui_send_message', {
    agent_id: agent.config.id,
    session_id: sessionId.value,
    workspace: workspace.value,
    content,
  }));
}

function submitTask(agent: AgentSummary) {
  if (!sessionContext.value) {
    connectionError.value = '缺少当前会话上下文，无法提交任务';
    return;
  }
  void withBusy(agent.config.id, () => sidecarCall('ui_submit_task', {
    agent_id: agent.config.id,
    session_id: sessionId.value,
    workspace: workspace.value,
    goal: `管理页触发：请执行 ${agent.config.name} 的职责`,
  }));
}

function interruptRun(agent: AgentSummary) {
  if (!agent.active_run_id) return;
  void withBusy(agent.config.id, () => sidecarCall('ui_interrupt_run', {
    run_id: agent.active_run_id,
  }));
}

function cancelRun(agent: AgentSummary) {
  if (!agent.active_run_id) return;
  void withBusy(agent.config.id, () => sidecarCall('ui_cancel_run', {
    run_id: agent.active_run_id,
  }));
}

function openCreate() {
  formInitial.value = null;
  formError.value = '';
  formVisible.value = true;
}

function openEdit(agent: AgentSummary) {
  formInitial.value = {
    agentId: agent.config.id,
    name: agent.config.name,
    description: agent.config.description,
    backend: agent.config.backend,
    command: agent.config.command ?? '',
    workspacePolicy: agent.config.workspace_policy,
    enabled: agent.config.enabled,
  };
  formError.value = '';
  formVisible.value = true;
}

async function submitForm(value: AgentFormValue) {
  try {
    if (value.agentId) {
      await sidecarCall('ui_agent_update', {
        agent_id: value.agentId,
        name: value.name,
        description: value.description,
        backend: value.backend,
        command: value.command,
        workspace_policy: value.workspacePolicy,
        enabled: value.enabled,
      });
    } else {
      await sidecarCall('ui_agent_create', {
        name: value.name,
        description: value.description,
        backend: value.backend,
        command: value.command,
        workspace_policy: value.workspacePolicy,
      });
    }
    formVisible.value = false;
    await refresh();
  } catch (error) {
    formError.value = String((error as Error).message ?? error);
  }
}

async function removeAgent(agent: AgentSummary) {
  const label = agent.config.name;
  if (!window.confirm(`确定删除 Subagent「${label}」？身份、指令与产物目录将一并删除（任务与运行历史保留）。`)) {
    return;
  }
  void withBusy(agent.config.id, () => sidecarCall('ui_agent_delete', {
    agent_id: agent.config.id,
  }));
}

function toggleExpand(agentId: string) {
  expandedAgentId.value = expandedAgentId.value === agentId ? null : agentId;
}

onMounted(async () => {
  applyContext(props.initialHostContext);
  if (props.subscribeHostContext) {
    stopHostContext = props.subscribeHostContext((context) => {
      applyContext(context);
      void refresh();
    });
  }
  await refresh();
  try {
    stopEvents = await subscribeSubagentEvents(() => scheduleRefresh());
  } catch {
    // 通知不可用时页面仍可手动刷新
  }
});

onUnmounted(() => {
  disposed = true;
  if (refreshTimer) clearTimeout(refreshTimer);
  stopEvents?.();
  stopHostContext?.();
});
</script>

<template>
  <div class="subagent-app">
    <header class="header">
      <div class="header-title">
        <h1>Subagent 管理</h1>
        <p class="session-line">
          <template v-if="sessionContext">
            当前会话 {{ sessionId.slice(0, 8) }}… · {{ workspace || '未指定 Workspace' }}
          </template>
          <template v-else>等待会话上下文…</template>
        </p>
      </div>
      <button class="btn btn-primary" type="button" @click="openCreate">新建 Subagent</button>
    </header>

    <p v-if="connectionError" class="error-line">{{ connectionError }}</p>

    <nav class="filters">
      <button
        v-for="item in FILTERS"
        :key="item.key"
        type="button"
        class="filter-chip"
        :class="{ active: filter === item.key }"
        @click="filter = item.key"
      >
        {{ item.label }}
        <span v-if="item.key === 'running' && runningCount" class="count">{{ runningCount }}</span>
        <span v-if="item.key === 'waiting' && waitingCount" class="count">{{ waitingCount }}</span>
      </button>
    </nav>

    <main class="content">
      <p v-if="!filteredAgents.length" class="empty">
        暂无 Subagent。点击右上角「新建 Subagent」创建第一个持久 Agent。
      </p>

      <section
        v-for="agent in filteredAgents"
        :key="agent.config.id"
        class="agent-card"
        :class="{ disabled: !agent.config.enabled }"
      >
        <div class="card-head" @click="toggleExpand(agent.config.id)">
          <div class="identity">
            <strong class="name">{{ agent.config.name }}</strong>
            <span class="tags">
              <span class="tag">{{ BACKEND_LABELS[agent.config.backend] }}</span>
              <span class="tag">{{ WORKSPACE_POLICY_LABELS[agent.config.workspace_policy] }}</span>
              <span v-if="!agent.config.enabled" class="tag tag-muted">已禁用</span>
              <span v-if="!BACKEND_IMPLEMENTED[agent.config.backend]" class="tag tag-muted">待接入</span>
            </span>
          </div>
          <span class="runtime" :class="runtimeStatusTone(agent)">● {{ runtimeStatusLabel(agent) }}</span>
        </div>

        <p class="description">{{ agent.config.description || '（无描述）' }}</p>

        <div class="status-row">
          <span class="status-item" :class="{ on: agent.config.enabled }">
            身份 · {{ agent.config.enabled ? '启用' : '禁用' }}
          </span>
          <span class="status-item" :class="{ on: agent.activated_in_session }">
            本会话 · {{ agent.activated_in_session ? '已激活' : '未激活' }}
          </span>
          <span class="status-item" :class="{ on: !!agent.activations.length }">
            活跃激活 {{ agent.activations.length }}
          </span>
        </div>

        <p v-if="agent.activated_in_session && sessionActivation(agent)" class="workspace-line">
          Workspace：{{ sessionActivation(agent)?.workspace }}
        </p>

        <div class="actions">
          <button
            v-if="!agent.activated_in_session"
            class="btn"
            type="button"
            :disabled="busyAgentId === agent.config.id || !agent.config.enabled"
            @click="activate(agent)"
          >
            激活
          </button>
          <button
            v-else
            class="btn"
            type="button"
            :disabled="busyAgentId === agent.config.id"
            @click="deactivate(agent)"
          >
            停用
          </button>
          <button
            class="btn btn-primary"
            type="button"
            :disabled="busyAgentId === agent.config.id || !agent.activated_in_session"
            @click="submitTask(agent)"
          >
            提交任务
          </button>
          <button
            v-if="agent.active_run_id && agent.runtime_status === 'working'"
            class="btn"
            type="button"
            :disabled="busyAgentId === agent.config.id"
            @click="interruptRun(agent)"
          >
            中断
          </button>
          <button
            v-if="agent.active_run_id && agent.runtime_status && ['working', 'blocked', 'approval_required', 'ready'].includes(agent.runtime_status)"
            class="btn btn-danger"
            type="button"
            :disabled="busyAgentId === agent.config.id"
            @click="cancelRun(agent)"
          >
            取消
          </button>
          <button class="btn btn-ghost" type="button" @click="openEdit(agent)">编辑</button>
          <button class="btn btn-ghost btn-danger-ghost" type="button" @click="removeAgent(agent)">删除</button>
        </div>

        <div
          v-if="agent.activated_in_session"
          class="message-row"
        >
          <input
            v-model="messageDraft[agent.config.id]"
            class="message-input"
            type="text"
            placeholder="发送消息（追问 / 补充背景 / 纠正方向）…"
            :disabled="busyAgentId === agent.config.id"
            @keydown.enter="sendMessage(agent)"
          />
          <button
            class="btn"
            type="button"
            :disabled="busyAgentId === agent.config.id || !(messageDraft[agent.config.id] ?? '').trim()"
            @click="sendMessage(agent)"
          >
            发送
          </button>
        </div>

        <div v-if="expandedAgentId === agent.config.id" class="detail">
          <div class="detail-block">
            <h3>进行中任务</h3>
            <p v-if="!tasksOf(agent).length" class="empty small">暂无</p>
            <ul v-else class="task-list">
              <li v-for="task in tasksOf(agent)" :key="task.task_id">
                <span class="task-status">{{ TASK_STATUS_LABELS[task.status] }}</span>
                <span class="task-goal">{{ task.goal }}</span>
              </li>
            </ul>
          </div>
          <div class="detail-block">
            <h3>最近事件</h3>
            <p v-if="!eventsOf(agent).length" class="empty small">暂无</p>
            <ul v-else class="event-list">
              <li v-for="event in eventsOf(agent)" :key="event.event_id">
                <span class="event-time">{{ event.created_at }}</span>
                <span class="event-type">{{ event.event_type }}</span>
                <span class="event-text">{{ eventText(event.payload) }}</span>
              </li>
            </ul>
          </div>
          <div class="detail-block">
            <h3>能力声明</h3>
            <p class="capability-line">
              消息 {{ agent.capabilities.messaging ? '✓' : '✗' }} ·
              任务 {{ agent.capabilities.tasks ? '✓' : '✗' }} ·
              流式 {{ agent.capabilities.streaming ? '✓' : '✗' }} ·
              中断 {{ agent.capabilities.interrupt ? '✓' : '✗' }} ·
              恢复 {{ agent.capabilities.resume ? '✓' : '✗' }} ·
              审批 {{ agent.capabilities.approval ? '✓' : '✗' }} ·
              写工作区 {{ agent.capabilities.workspace_write ? '✓' : '✗' }} ·
              产物 {{ agent.capabilities.artifacts ? '✓' : '✗' }}
            </p>
          </div>
        </div>
      </section>
    </main>

    <AgentForm
      v-if="formVisible"
      :initial="formInitial"
      :error="formError"
      @submit="submitForm"
      @cancel="formVisible = false"
    />
  </div>
</template>

<style scoped>
.subagent-app {
  --ui-background: hsl(var(--background, 0 0% 100%));
  --ui-foreground: hsl(var(--foreground, 222.2 47.4% 11.2%));
  --ui-card: hsl(var(--card, 0 0% 100%));
  --ui-muted-foreground: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
  --ui-border: hsl(var(--border, 214.3 31.8% 91.4%));
  --ui-primary: hsl(var(--primary, 222.2 47.4% 11.2%));
  --ui-primary-foreground: hsl(var(--primary-foreground, 210 40% 98%));
  --ui-destructive: hsl(var(--destructive, 0 84.2% 60.2%));
  --ui-input: hsl(var(--input, 214.3 31.8% 91.4%));

  box-sizing: border-box;
  display: flex;
  flex-direction: column;
  gap: 14px;
  width: 100%;
  height: 100%;
  padding: 18px 20px;
  overflow-y: auto;
  color: var(--ui-foreground);
  background: var(--ui-background);
  font-size: 13px;
}

.header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
}

.header-title h1 {
  margin: 0;
  font-size: 17px;
  font-weight: 600;
}

.session-line {
  margin: 4px 0 0;
  color: var(--ui-muted-foreground);
  font-size: 12px;
}

.error-line {
  margin: 0;
  padding: 8px 10px;
  border: 1px solid var(--ui-destructive);
  border-radius: 8px;
  color: var(--ui-destructive);
  font-size: 12px;
}

.filters {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.filter-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 4px 10px;
  border: 1px solid var(--ui-border);
  border-radius: 999px;
  background: transparent;
  color: var(--ui-muted-foreground);
  font-size: 12px;
  cursor: pointer;
}

.filter-chip.active {
  border-color: var(--ui-primary);
  color: var(--ui-foreground);
  font-weight: 600;
}

.filter-chip .count {
  padding: 0 6px;
  border-radius: 999px;
  background: var(--ui-primary);
  color: var(--ui-primary-foreground);
  font-size: 11px;
}

.content {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.empty {
  margin: 24px 0;
  color: var(--ui-muted-foreground);
  text-align: center;
}

.empty.small {
  margin: 4px 0;
  text-align: left;
}

.agent-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 14px;
  border: 1px solid var(--ui-border);
  border-radius: 12px;
  background: var(--ui-card);
}

.agent-card.disabled {
  opacity: 0.6;
}

.card-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 10px;
  cursor: pointer;
}

.identity {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.name {
  font-size: 14px;
}

.tags {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.tag {
  padding: 1px 8px;
  border: 1px solid var(--ui-border);
  border-radius: 999px;
  color: var(--ui-muted-foreground);
  font-size: 11px;
}

.tag-muted {
  font-style: italic;
}

.runtime {
  flex-shrink: 0;
  font-size: 12px;
  white-space: nowrap;
}

.tone-idle { color: var(--ui-muted-foreground); }
.tone-working { color: hsl(var(--primary, 222.2 47.4% 11.2%)); }
.tone-waiting { color: hsl(38 92% 50%); }
.tone-failed { color: var(--ui-destructive); }
.tone-completed { color: hsl(142 71% 45%); }

.description {
  margin: 0;
  color: var(--ui-muted-foreground);
}

.status-row {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  font-size: 12px;
}

.status-item {
  color: var(--ui-muted-foreground);
}

.status-item.on {
  color: var(--ui-foreground);
  font-weight: 600;
}

.workspace-line {
  margin: 0;
  color: var(--ui-muted-foreground);
  font-size: 12px;
  word-break: break-all;
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.btn {
  padding: 5px 12px;
  border: 1px solid var(--ui-border);
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 12px;
  cursor: pointer;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-primary {
  border-color: var(--ui-primary);
  background: var(--ui-primary);
  color: var(--ui-primary-foreground);
}

.btn-danger {
  border-color: var(--ui-destructive);
  color: var(--ui-destructive);
}

.btn-ghost {
  border-color: transparent;
  color: var(--ui-muted-foreground);
}

.btn-danger-ghost {
  color: var(--ui-destructive);
}

.message-row {
  display: flex;
  gap: 6px;
}

.message-input {
  flex: 1;
  padding: 6px 10px;
  border: 1px solid var(--ui-input);
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 12px;
}

.detail {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px;
  border-top: 1px dashed var(--ui-border);
}

.detail-block h3 {
  margin: 0 0 6px;
  font-size: 12px;
  color: var(--ui-muted-foreground);
}

.task-list,
.event-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.task-list li,
.event-list li {
  display: flex;
  gap: 8px;
  font-size: 12px;
}

.task-status {
  flex-shrink: 0;
  color: var(--ui-muted-foreground);
}

.task-goal {
  word-break: break-all;
}

.event-time {
  flex-shrink: 0;
  color: var(--ui-muted-foreground);
}

.event-type {
  flex-shrink: 0;
  min-width: 84px;
  color: var(--ui-muted-foreground);
}

.event-text {
  word-break: break-all;
}

.capability-line {
  margin: 0;
  color: var(--ui-muted-foreground);
  font-size: 12px;
}
</style>
