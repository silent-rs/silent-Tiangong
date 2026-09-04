<script setup lang="ts">
import { reactive } from 'vue';
import {
  BACKEND_IMPLEMENTED,
  BACKEND_LABELS,
  WORKSPACE_POLICY_LABELS,
  type BackendKind,
  type WorkspacePolicy,
} from '../types';

export interface AgentFormValue {
  agentId?: string;
  name: string;
  description: string;
  backend: BackendKind;
  command: string;
  workspacePolicy: WorkspacePolicy;
  enabled: boolean;
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
  backend: props.initial?.backend ?? 'cli',
  command: props.initial?.command ?? '',
  workspacePolicy: props.initial?.workspacePolicy ?? 'read-only',
  enabled: props.initial?.enabled ?? true,
});

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
        <small>子进程从 stdin 逐行读取 JSON 指令，向 stdout 逐行输出 message / blocked / completed / failed 事件。</small>
      </label>

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
  width: min(460px, 100%);
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
