<script setup lang="ts">
import { reactive, ref, watch } from 'vue';
import {
  BACKEND_IMPLEMENTED,
  BACKEND_LABELS,
  WORKSPACE_POLICY_LABELS,
  type BackendKind,
  type SessionBrief,
  type WorkspacePolicy,
} from '../types';
import { sidecarCall } from '../api';

export interface AgentFormValue {
  agentId?: string;
  name: string;
  description: string;
  backend: BackendKind;
  command: string;
  sessionId?: string | null;
  workspacePolicy: WorkspacePolicy;
  enabled: boolean;
  instructions: string;
}

interface Props {
  initial?: AgentFormValue | null;
  error?: string;
}

const props = withDefaults(defineProps<Props>(), {
  initial: null,
  error: '',
});

const emit = defineEmits<{
  submit: [value: AgentFormValue];
  cancel: [];
}>();

const editing = Boolean(props.initial?.agentId);

const form = reactive<AgentFormValue>({
  agentId: props.initial?.agentId,
  name: props.initial?.name ?? '',
  description: props.initial?.description ?? '',
  backend: props.initial?.backend ?? 'agent_team',
  command: props.initial?.command ?? '',
  sessionId: props.initial?.sessionId ?? null,
  workspacePolicy: props.initial?.workspacePolicy ?? 'read-only',
  enabled: props.initial?.enabled ?? true,
  instructions: props.initial?.instructions ?? '',
});

const sessions = ref<SessionBrief[]>([]);
const sessionSearch = ref('');
const sessionLoading = ref(false);
const sessionError = ref('');

async function loadSessions() {
  sessionLoading.value = true;
  sessionError.value = '';
  try {
    sessions.value = await sidecarCall<SessionBrief[]>('ui_list_sessions', {});
  } catch (error) {
    sessionError.value = String((error as Error).message ?? error);
  } finally {
    sessionLoading.value = false;
  }
}

// 后端切到天工会话时才拉取会话列表。
watch(
  () => form.backend,
  (backend) => {
    if (backend === 'tiangong_session' && sessions.value.length === 0) {
      void loadSessions();
    }
  },
);

const filteredSessions = () => {
  const keyword = sessionSearch.value.trim().toLowerCase();
  const list = sessions.value;
  if (!keyword) return list.slice(0, 50);
  return list
    .filter((session) =>
      session.title.toLowerCase().includes(keyword)
      || session.id.toLowerCase().includes(keyword))
    .slice(0, 50);
};

const selectedSessionTitle = () =>
  sessions.value.find((session) => session.id === form.sessionId)?.title ?? '';

function submit() {
  emit('submit', { ...form });
}
</script>

<template>
  <div class="form-overlay" @click.self="emit('cancel')">
    <form class="form-panel" @submit.prevent="submit">
      <h2>{{ editing ? '编辑 Subagent' : '新建 Subagent' }}</h2>

      <label class="field">
        <span>名称 *</span>
        <input v-model="form.name" type="text" required placeholder="例如：代码审查员" />
      </label>

      <label class="field">
        <span>描述</span>
        <input v-model="form.description" type="text" placeholder="职责一句话说明" />
      </label>

      <label class="field">
        <span>运行后端</span>
        <select v-model="form.backend" :disabled="editing">
          <option v-for="(label, key) in BACKEND_LABELS" :key="key" :value="key">
            {{ label }}{{ BACKEND_IMPLEMENTED[key as BackendKind] ? '' : '（后续阶段）' }}
          </option>
        </select>
      </label>

      <label v-if="form.backend === 'cli'" class="field">
        <span>启动命令 *</span>
        <input
          v-model="form.command"
          type="text"
          required
          placeholder="sh /path/to/agent.sh（stdin/stdout 走 JSONL 协议）"
        />
        <small>子进程从 stdin 逐行读取 JSON 指令（含长期指令与记忆摘要），向 stdout 逐行输出 message / blocked / completed / failed 事件。</small>
      </label>

      <div v-if="form.backend === 'tiangong_session'" class="field">
        <span>关联会话 *</span>
        <div v-if="form.sessionId" class="selected-session">
          <strong>{{ selectedSessionTitle() || form.sessionId }}</strong>
          <small>{{ form.sessionId }}</small>
          <button class="btn btn-ghost" type="button" @click="form.sessionId = null">重选</button>
        </div>
        <template v-else>
          <input
            v-model="sessionSearch"
            class="session-search"
            type="text"
            placeholder="按标题或 ID 搜索会话…"
            @input="() => {}"
          />
          <p v-if="sessionLoading" class="session-hint">加载会话列表…</p>
          <p v-else-if="sessionError" class="session-hint error">{{ sessionError }}</p>
          <div v-else class="session-list">
            <button
              v-for="session in filteredSessions()"
              :key="session.id"
              class="session-item"
              type="button"
              @click="form.sessionId = session.id"
            >
              <span class="session-title">{{ session.title }}</span>
              <span class="session-meta">{{ session.message_count }} 条消息 · {{ session.updated_at }}</span>
            </button>
            <p v-if="!filteredSessions().length" class="session-hint">没有匹配的会话</p>
          </div>
        </template>
        <small>该会话的全部上下文与历史将成为此 Subagent 的能力；任务经消息通道投递，完成后自动回报。</small>
      </div>

      <label class="field">
        <span>Workspace 策略</span>
        <select v-model="form.workspacePolicy">
          <option v-for="(label, key) in WORKSPACE_POLICY_LABELS" :key="key" :value="key">
            {{ label }}
          </option>
        </select>
        <small>读可以并行；独占写要求同一工作区同时只有一个写入者；隔离 worktree 需要工作区是 git 仓库。</small>
      </label>

      <label v-if="editing" class="field field-inline">
        <input v-model="form.enabled" type="checkbox" />
        <span>启用该 Subagent</span>
      </label>

      <label class="field">
        <span>长期指令</span>
        <textarea
          v-model="form.instructions"
          class="instructions-input"
          rows="6"
          placeholder="该成员的职责与工作要求（跨会话保留；每次派活与协作时自动注入）"
        />
      </label>

      <p v-if="error" class="form-error">{{ error }}</p>

      <div class="form-actions">
        <button class="btn" type="button" @click="emit('cancel')">取消</button>
        <button class="btn btn-primary" type="submit">保存</button>
      </div>
    </form>
  </div>
</template>

<style scoped>
.form-overlay {
  position: fixed;
  inset: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
  background: hsl(var(--background, 0 0% 100%) / 0.6);
  backdrop-filter: blur(2px);
}

.form-panel {
  --ui-foreground: hsl(var(--foreground, 222.2 47.4% 11.2%));
  --ui-muted-foreground: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
  --ui-border: hsl(var(--border, 214.3 31.8% 91.4%));
  --ui-card: hsl(var(--card, 0 0% 100%));
  --ui-primary: hsl(var(--primary, 222.2 47.4% 11.2%));
  --ui-primary-foreground: hsl(var(--primary-foreground, 210 40% 98%));
  --ui-input: hsl(var(--input, 214.3 31.8% 91.4%));
  --ui-destructive: hsl(var(--destructive, 0 84.2% 60.2%));

  display: flex;
  flex-direction: column;
  gap: 12px;
  width: min(480px, 100%);
  max-height: 90%;
  padding: 20px;
  overflow-y: auto;
  border: 1px solid var(--ui-border);
  border-radius: 14px;
  background: var(--ui-card);
  color: var(--ui-foreground);
  font-size: 13px;
}

.form-panel h2 {
  margin: 0;
  font-size: 15px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.field > span {
  color: var(--ui-muted-foreground);
  font-size: 12px;
}

.field small {
  color: var(--ui-muted-foreground);
  font-size: 11px;
  line-height: 1.5;
}

.instructions-input {
  padding: 6px 10px;
  border: 1px solid var(--ui-input, hsl(var(--input, 214.3 31.8% 91.4%)));
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 12px;
  font-family: inherit;
  resize: vertical;
}

.field input[type='text'],
.field select {
  padding: 7px 10px;
  border: 1px solid var(--ui-input);
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 13px;
}

.field-inline {
  flex-direction: row;
  align-items: center;
  gap: 8px;
}

.session-search {
  padding: 7px 10px;
  border: 1px solid var(--ui-input);
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 13px;
}

.session-hint {
  margin: 4px 0 0;
  color: var(--ui-muted-foreground);
  font-size: 12px;
}

.session-hint.error {
  color: var(--ui-destructive);
}

.session-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 180px;
  overflow-y: auto;
}

.session-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 7px 10px;
  border: 1px solid var(--ui-border);
  border-radius: 8px;
  background: transparent;
  text-align: left;
  cursor: pointer;
}

.session-item:hover {
  border-color: var(--ui-primary);
}

.session-title {
  font-size: 13px;
}

.session-meta {
  color: var(--ui-muted-foreground);
  font-size: 11px;
}

.selected-session {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 8px 10px;
  border: 1px solid var(--ui-border);
  border-radius: 8px;
}

.selected-session small {
  color: var(--ui-muted-foreground);
}

.form-error {
  margin: 0;
  color: var(--ui-destructive);
  font-size: 12px;
}

.form-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.btn {
  padding: 6px 14px;
  border: 1px solid var(--ui-border);
  border-radius: 8px;
  background: transparent;
  color: var(--ui-foreground);
  font-size: 12px;
  cursor: pointer;
}

.btn-primary {
  border-color: var(--ui-primary);
  background: var(--ui-primary);
  color: var(--ui-primary-foreground);
}
</style>
