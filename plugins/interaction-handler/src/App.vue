<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import {
  createTiangongBridge,
  createToolProvider,
  type HostContext,
  type HostBridge,
  type ToolClosed,
  type ToolInvocation,
} from '@tiangong/plugin-sdk';
import {
  approvalOpinion,
  isRecord,
  normalizeHostTokenValue,
  parseInvocation,
  payloadResult,
  userClosedFeedback,
  type InteractionKind,
  type InteractionRequest,
} from './interaction';

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

const requests = ref<InteractionRequest[]>([]);
const currentSessionId = ref<string | null>(null);
const nowMs = ref(Date.now());
const connectionError = ref('');

let bridge: HostBridge | null = null;
let provider: ReturnType<typeof createToolProvider> | null = null;
let stopRequested: (() => void) | null = null;
let stopClosed: (() => void) | null = null;
let stopHostContext: (() => void) | null = null;
let countdownTimer: ReturnType<typeof setInterval> | null = null;

const request = computed(() => {
  const matches = requests.value.filter((item) => item.sessionId === currentSessionId.value);
  return matches.find((item) => item.status === 'pending' || item.status === 'submitting')
    ?? matches[0]
    ?? null;
});

const remainingMs = computed(() => request.value
  ? Math.max(0, request.value.deadlineMs - nowMs.value)
  : 0);

const remainingText = computed(() => {
  if (!request.value) return '';
  if (request.value.status === 'answered') return '已提交';
  if (request.value.status === 'expired') return '已过期';
  if (request.value.status === 'cancelled') return '已取消';
  if (request.value.status === 'submitting') return '正在提交';
  if (remainingMs.value <= 0) return '已到期限';
  return `剩余 ${Math.ceil(remainingMs.value / 1000)} 秒`;
});

const locked = computed(() => !request.value || request.value.status !== 'pending');

const formIncomplete = computed(() => {
  if (!request.value || request.value.kind !== 'form') return false;
  return request.value.fields.some((field) => {
    if (!field.required) return false;
    const value = request.value?.values[field.key];
    return value == null || value === '';
  });
});

function replaceRequest(invocationId: string, update: Partial<InteractionRequest>) {
  requests.value = requests.value.map((item) => item.invocationId === invocationId
    ? { ...item, ...update }
    : item);
}

async function resolveInvalid(invocation: ToolInvocation, message: string) {
  if (!provider) return;
  const kind = isRecord(invocation.arguments)
    && typeof invocation.arguments.kind === 'string'
    && ['approval', 'confirm', 'choice', 'multi_choice', 'input', 'form'].includes(invocation.arguments.kind)
    ? invocation.arguments.kind as InteractionKind
    : 'unknown';
  try {
    await provider.resolve({
      invocation_id: invocation.invocation_id,
      result: payloadResult(
        invocation.invocation_id,
        kind,
        'invalid',
        { message: `request_user 参数无效：${message}` },
        false,
      ),
    });
  } catch (error) {
    connectionError.value = `提交无效参数结果失败：${String(error)}`;
  }
}

async function expire(item: InteractionRequest) {
  if (!provider || item.status !== 'pending') return;
  replaceRequest(item.invocationId, { status: 'submitting', error: '' });
  const message = '用户未在规定时间内回应本次征询';
  try {
    await provider.resolve({
      invocation_id: item.invocationId,
      status: 'expired',
      result: payloadResult(
        item.invocationId,
        item.kind,
        'expired',
        { message },
        false,
      ),
    });
  } catch (error) {
    replaceRequest(item.invocationId, { status: 'expired', error: String(error) });
  }
}

async function cancelRequest() {
  const item = request.value;
  if (!provider || !item || item.status !== 'pending') return;
  replaceRequest(item.invocationId, { status: 'submitting', error: '' });
  const feedback = userClosedFeedback();
  try {
    await provider.resolve({
      invocation_id: item.invocationId,
      status: 'cancelled',
      result: payloadResult(
        item.invocationId,
        item.kind,
        'cancelled',
        feedback,
        false,
      ),
    });
  } catch (error) {
    replaceRequest(item.invocationId, {
      status: Date.now() >= item.deadlineMs ? 'expired' : 'pending',
      error: String(error),
    });
  }
}

async function answer(result: unknown) {
  const item = request.value;
  if (!provider || !item || item.status !== 'pending') return;
  if (Date.now() >= item.deadlineMs) {
    await expire(item);
    return;
  }
  replaceRequest(item.invocationId, { status: 'submitting', error: '' });
  try {
    await provider.resolve({
      invocation_id: item.invocationId,
      result: payloadResult(
        item.invocationId,
        item.kind,
        'answered',
        { result },
        true,
      ),
    });
  } catch (error) {
    replaceRequest(item.invocationId, {
      status: Date.now() >= item.deadlineMs ? 'expired' : 'pending',
      error: String(error),
    });
  }
}

function toggleChoice(option: string) {
  const item = request.value;
  if (!item || item.status !== 'pending') return;
  const selected = item.kind === 'multi_choice'
    ? item.selected.includes(option)
      ? item.selected.filter((value) => value !== option)
      : [...item.selected, option]
    : [option];
  replaceRequest(item.invocationId, { selected });
}

function resolveKindResult() {
  const item = request.value;
  if (!item) return;
  if (item.kind === 'choice') {
    void answer(item.selected[0]);
  } else if (item.kind === 'multi_choice') {
    void answer(item.selected);
  } else if (item.kind === 'input') {
    void answer(String(item.values.input ?? ''));
  } else if (item.kind === 'form') {
    const answers: Record<string, unknown> = {};
    for (const field of item.fields) {
      const raw = item.values[field.key];
      if (field.type === 'number') {
        answers[field.key] = raw === '' || raw == null ? 0 : Number(raw);
      } else if (field.type === 'boolean') {
        answers[field.key] = raw === true;
      } else {
        answers[field.key] = String(raw ?? '');
      }
    }
    void answer(answers);
  }
}

function onRequested(invocation: ToolInvocation) {
  if (requests.value.some((item) => item.invocationId === invocation.invocation_id)) return;
  try {
    const parsed = parseInvocation(invocation);
    requests.value = [...requests.value, parsed]
      .sort((left, right) => left.createdAtMs - right.createdAtMs);
    if (Date.now() >= parsed.deadlineMs) void expire(parsed);
  } catch (error) {
    void resolveInvalid(invocation, error instanceof Error ? error.message : String(error));
  }
}

function onClosed(closed: ToolClosed) {
  const item = requests.value.find((candidate) => candidate.invocationId === closed.invocation_id);
  if (!item) return;
  replaceRequest(closed.invocation_id, { status: closed.status, error: '' });
  window.setTimeout(() => {
    requests.value = requests.value.filter((candidate) => candidate.invocationId !== closed.invocation_id);
  }, 1600);
}

function applyHostContext(data: unknown) {
  if (!isRecord(data)) return;
  if (!props.shadowContainer) {
    const tokens = isRecord(data.tokens) ? data.tokens : {};
    for (const [name, value] of Object.entries(tokens)) {
      if (typeof value === 'string' && value) {
        document.documentElement.style.setProperty(`--${name}`, normalizeHostTokenValue(value));
      }
    }
    if (typeof data.fontFamily === 'string' && data.fontFamily) {
      document.body.style.fontFamily = data.fontFamily;
    }
    if (data.theme === 'light' || data.theme === 'dark') {
      document.documentElement.style.colorScheme = data.theme;
    }
  }
  const session = isRecord(data.session) ? data.session : null;
  currentSessionId.value = session && typeof session.id === 'string' ? session.id : null;
}

function onHostMessage(event: MessageEvent) {
  const data = event.data as Record<string, unknown> | null;
  if (data?.type === 'tiangong_host_context') applyHostContext(data);
}

onMounted(async () => {
  if (props.initialHostContext) applyHostContext(props.initialHostContext);
  if (props.subscribeHostContext) {
    stopHostContext = props.subscribeHostContext(applyHostContext);
  } else {
    window.addEventListener('message', onHostMessage);
  }
  countdownTimer = window.setInterval(() => {
    nowMs.value = Date.now();
    for (const item of requests.value) {
      if (item.status === 'pending' && nowMs.value >= item.deadlineMs) void expire(item);
    }
  }, 250);
  try {
    bridge = await createTiangongBridge();
    provider = createToolProvider(bridge);
    stopRequested = provider.onRequested(onRequested);
    stopClosed = provider.onClosed(onClosed);
  } catch (error) {
    connectionError.value = `桥接连线失败：${String(error)}`;
  }
});

onUnmounted(() => {
  stopRequested?.();
  stopClosed?.();
  stopHostContext?.();
  if (countdownTimer) window.clearInterval(countdownTimer);
  window.removeEventListener('message', onHostMessage);
  bridge = null;
  provider = null;
});
</script>

<template>
  <div class="wrap">
    <section v-if="request">
      <div class="heading">
        <div class="heading-copy">
          <h3>{{ request.title }}</h3>
        </div>
        <div class="heading-actions">
          <span
            class="deadline"
            :class="{ overdue: remainingMs <= 0 || request.status === 'expired' }"
            aria-live="polite"
          >
            {{ remainingText }}
          </span>
          <button
            type="button"
            class="close"
            aria-label="关闭本次操作"
            title="关闭本次操作"
            :disabled="locked"
            @click="cancelRequest"
          >
            <span aria-hidden="true">&times;</span>
          </button>
        </div>
      </div>

      <div class="request-body">
        <p v-if="request.description" class="muted">{{ request.description }}</p>

        <div v-if="request.kind === 'choice' || request.kind === 'multi_choice'" class="content">
          <label
            v-for="option in request.options"
            :key="option"
            class="option"
            :class="{ selected: request.selected.includes(option) }"
          >
            <input
              :type="request.kind === 'multi_choice' ? 'checkbox' : 'radio'"
              :checked="request.selected.includes(option)"
              :disabled="locked"
              @change="toggleChoice(option)"
            />
            <span>{{ option }}</span>
          </label>
        </div>

        <div
          v-else-if="request.kind === 'confirm' && (request.question || !request.description)"
          class="content question"
        >
          {{ request.question || '是否继续？' }}
        </div>

        <div v-else-if="request.kind === 'approval' && request.question" class="content question">
          {{ request.question }}
        </div>

        <div v-else-if="request.kind === 'input'" class="content">
          <label v-if="request.question" class="input-question">{{ request.question }}</label>
          <input
            v-model="request.values.input"
            type="text"
            placeholder="请输入"
            :disabled="locked"
            @keydown.enter.prevent="answer(String(request.values.input ?? ''))"
          />
        </div>

        <div v-else-if="request.kind === 'form'" class="content">
          <label v-for="field in request.fields" :key="field.key" class="field">
            <span class="field-label">{{ field.label }}<b v-if="field.required">*</b></span>
            <input
              v-if="field.type === 'boolean'"
              v-model="request.values[field.key]"
              type="checkbox"
              :disabled="locked"
            />
            <select
              v-else-if="field.type === 'select'"
              v-model="request.values[field.key]"
              :disabled="locked"
            >
              <option value="" disabled>请选择</option>
              <option v-for="option in field.options ?? []" :key="option" :value="option">
                {{ option }}
              </option>
            </select>
            <input
              v-else
              v-model="request.values[field.key]"
              :type="field.type === 'number' ? 'number' : 'text'"
              :placeholder="field.placeholder"
              :disabled="locked"
            />
          </label>
        </div>

        <div v-if="request.error" class="error">{{ request.error }}</div>
      </div>

      <div class="actions">
        <template v-if="request.kind === 'approval'">
          <button type="button" class="approve" :disabled="locked" @click="answer(approvalOpinion('approve'))">
            同意
          </button>
          <button type="button" class="reject" :disabled="locked" @click="answer(approvalOpinion('reject'))">
            拒绝
          </button>
        </template>
        <template v-else-if="request.kind === 'confirm'">
          <button type="button" class="primary" :disabled="locked" @click="answer(true)">是</button>
          <button type="button" class="secondary" :disabled="locked" @click="answer(false)">否</button>
        </template>
        <button
          v-else
          type="button"
          class="primary"
          :disabled="locked || formIncomplete || ((request.kind === 'choice' || request.kind === 'multi_choice') && request.selected.length === 0)"
          @click="resolveKindResult"
        >
          提交
        </button>
        <button
          v-if="request.kind !== 'approval' && request.kind !== 'confirm'"
          type="button"
          class="secondary"
          :disabled="locked"
          @click="cancelRequest"
        >
          取消
        </button>
      </div>
    </section>
    <div v-else-if="connectionError" class="error">{{ connectionError }}</div>
  </div>
</template>

<style>
html,
body,
#app {
  width: 100%;
  margin: 0;
  background: transparent;
}

html,
body {
  height: 100%;
}

body {
  overflow: hidden;
}

:host {
  display: block;
  width: 100%;
  height: 100%;
  max-height: inherit;
  overflow: hidden;
}

#app {
  box-sizing: border-box;
  height: 100%;
  max-height: inherit;
}
</style>

<style scoped>
.wrap {
  box-sizing: border-box;
  width: 100%;
  height: 100%;
  max-height: inherit;
  min-height: 0;
  padding: 14px 16px;
  overflow: hidden;
  color: var(--foreground, #222);
  background: var(--card, transparent);
}

section {
  display: flex;
  height: 100%;
  max-height: inherit;
  min-height: 0;
  flex-direction: column;
}

.heading {
  display: flex;
  flex: 0 0 auto;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.heading-copy {
  min-width: 0;
  max-height: 64px;
  overflow-x: hidden;
  overflow-y: auto;
  overscroll-behavior: contain;
}

.heading-actions {
  display: flex;
  flex: 0 0 auto;
  align-items: center;
  gap: 8px;
}

h3 {
  margin: 0;
  overflow-wrap: anywhere;
  font-size: 15px;
  font-weight: 600;
  line-height: 1.4;
  letter-spacing: 0;
}

.muted {
  margin: 4px 0 0;
  color: var(--muted-foreground, #777);
  font-size: 12px;
  line-height: 1.45;
}

.request-body {
  min-height: 0;
  overflow-x: hidden;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-gutter: stable;
}

.deadline {
  flex: 0 0 auto;
  color: var(--muted-foreground, #888);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  line-height: 1.4;
}

.deadline.overdue {
  color: var(--status-error, #dc2626);
}

.content {
  display: grid;
  gap: 8px;
  margin: 12px 0 0;
}

.question {
  border-left: 2px solid var(--primary, #2563eb);
  padding-left: 10px;
  color: var(--foreground, #222);
  font-size: 13px;
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.input-question {
  color: var(--muted-foreground, #777);
  font-size: 12px;
}

.option {
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 32px;
  border: 1px solid var(--border, #d1d5db);
  border-radius: 6px;
  padding: 6px 9px;
  font-size: 13px;
  cursor: pointer;
}

.option.selected {
  border-color: var(--primary, #2563eb);
  background: var(--accent, #f1f5f9);
}

.option input {
  margin: 0;
  accent-color: var(--primary, #2563eb);
}

.field {
  display: grid;
  grid-template-columns: minmax(80px, 112px) minmax(0, 1fr);
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.field-label {
  color: var(--muted-foreground, #777);
  overflow-wrap: anywhere;
}

.field-label b {
  margin-left: 2px;
  color: var(--status-error, #dc2626);
}

input[type='text'],
input[type='number'],
select {
  box-sizing: border-box;
  width: 100%;
  min-width: 0;
  padding: 7px 8px;
  border: 1px solid var(--border, #d1d5db);
  border-radius: 6px;
  background: var(--background, transparent);
  color: inherit;
  font: inherit;
  outline: none;
}

input[type='text']:focus,
input[type='number']:focus,
select:focus {
  border-color: var(--ring, #2563eb);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--ring, #2563eb) 20%, transparent);
}

.actions {
  display: flex;
  flex: 0 0 auto;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: auto;
  padding-top: 12px;
}

button {
  min-width: 72px;
  min-height: 34px;
  border: 1px solid transparent;
  border-radius: 6px;
  padding: 7px 14px;
  cursor: pointer;
  font: inherit;
  font-size: 13px;
  font-weight: 500;
  letter-spacing: 0;
  transition: filter 120ms ease, opacity 120ms ease;
}

button.primary {
  background: var(--primary, #2563eb);
  color: var(--primary-foreground, white);
}

button.approve {
  background: var(--status-success, #16803f);
  color: white;
}

button.reject {
  border-color: var(--status-error, #dc2626);
  background: transparent;
  color: var(--status-error, #dc2626);
}

button.secondary {
  border-color: var(--border, #d1d5db);
  background: var(--muted, #e5e7eb);
  color: var(--foreground, #222);
}

button.close {
  width: 28px;
  min-width: 28px;
  height: 28px;
  min-height: 28px;
  border-color: transparent;
  padding: 0;
  background: transparent;
  color: var(--muted-foreground, #777);
  font-size: 20px;
  font-weight: 400;
  line-height: 1;
}

button.close:hover:not(:disabled) {
  background: var(--muted, #e5e7eb);
  color: var(--foreground, #222);
  filter: none;
}

button:hover:not(:disabled) {
  filter: brightness(0.94);
}

button:focus-visible {
  outline: 2px solid var(--ring, #2563eb);
  outline-offset: 2px;
}

button:disabled {
  cursor: not-allowed;
  opacity: 0.45;
}

.error {
  margin-top: 8px;
  color: var(--status-error, #dc2626);
  font-size: 12px;
  overflow-wrap: anywhere;
}

@media (max-width: 420px) {
  .field { grid-template-columns: 1fr; gap: 4px; }
  .actions button { flex: 1 1 auto; }
}
</style>
