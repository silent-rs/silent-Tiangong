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
const rootElement = ref<HTMLElement | null>(null);

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
  const seconds = Math.ceil(remainingMs.value / 1000);
  if (seconds >= 60) {
    return `剩余 ${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  }
  return `剩余 ${seconds} 秒`;
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
  const element = rootElement.value;
  if (element && !props.shadowContainer) {
    const tokens = isRecord(data.tokens) ? data.tokens : {};
    for (const [name, value] of Object.entries(tokens)) {
      if (typeof value === 'string' && value) {
        element.style.setProperty(`--${name}`, value.trim());
      }
    }
    if (typeof data.fontFamily === 'string' && data.fontFamily) {
      element.style.fontFamily = data.fontFamily;
    }
    if (data.theme === 'light' || data.theme === 'dark') {
      element.style.colorScheme = data.theme;
      element.dataset.theme = data.theme;
    }
  }
  if (!props.shadowContainer && (data.theme === 'light' || data.theme === 'dark')) {
    document.documentElement.style.colorScheme = data.theme;
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
  <div ref="rootElement" class="wrap">
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

        <div v-if="request.kind === 'choice' || request.kind === 'multi_choice'" class="content choices">
          <label
            v-for="option in request.options"
            :key="option"
            class="option"
            :class="{ selected: request.selected.includes(option), disabled: locked }"
          >
            <input
              class="control-native"
              :type="request.kind === 'multi_choice' ? 'checkbox' : 'radio'"
              :name="`interaction-${request.invocationId}`"
              :checked="request.selected.includes(option)"
              :disabled="locked"
              @change="toggleChoice(option)"
            />
            <span
              class="selection-indicator"
              :class="request.kind === 'multi_choice' ? 'checkbox' : 'radio'"
              aria-hidden="true"
            />
            <span class="option-label">{{ option }}</span>
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
          <label
            v-for="field in request.fields"
            :key="field.key"
            class="field"
            :class="{ 'boolean-field': field.type === 'boolean' }"
          >
            <span class="field-label">{{ field.label }}<b v-if="field.required">*</b></span>
            <template v-if="field.type === 'boolean'">
              <input
                v-model="request.values[field.key]"
                class="control-native"
                type="checkbox"
                :disabled="locked"
              />
              <span class="selection-indicator checkbox" aria-hidden="true" />
            </template>
            <span v-else-if="field.type === 'select'" class="select-control">
              <select
                v-model="request.values[field.key]"
                :disabled="locked"
              >
                <option value="" disabled>请选择</option>
                <option v-for="option in field.options ?? []" :key="option" :value="option">
                  {{ option }}
                </option>
              </select>
              <span class="select-chevron" aria-hidden="true" />
            </span>
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

<style scoped>
.wrap {
  --ui-background: hsl(var(--background, 0 0% 100%));
  --ui-foreground: hsl(var(--foreground, 222.2 47.4% 11.2%));
  --ui-card: hsl(var(--card, 0 0% 100%));
  --ui-muted-foreground: hsl(var(--muted-foreground, 215.4 16.3% 46.9%));
  --ui-accent: hsl(var(--accent, 210 40% 96.1%));
  --ui-accent-foreground: hsl(var(--accent-foreground, 222.2 47.4% 11.2%));
  --ui-primary: hsl(var(--primary, 222.2 47.4% 11.2%));
  --ui-primary-foreground: hsl(var(--primary-foreground, 210 40% 98%));
  --ui-destructive: hsl(var(--destructive, 0 84.2% 60.2%));
  --ui-input: hsl(var(--input, 214.3 31.8% 91.4%));
  --ui-ring: hsl(var(--ring, 222.2 47.4% 11.2%));
  --ui-status-error: hsl(var(--status-error, 0 84% 60%));
  --ui-radius: var(--radius, 0.5rem);

  box-sizing: border-box;
  width: 100%;
  height: 100%;
  max-height: inherit;
  min-height: 0;
  padding: 14px 16px;
  overflow: hidden;
  color: var(--ui-foreground, #0f172a);
  background: var(--ui-card, #fff);
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
  color: var(--ui-muted-foreground, #64748b);
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
  color: var(--ui-muted-foreground, #64748b);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
  line-height: 1.4;
}

.deadline.overdue {
  color: var(--ui-status-error, #dc2626);
}

.content {
  display: grid;
  gap: 8px;
  margin: 12px 0 0;
}

.question {
  border-left: 2px solid var(--ui-primary, #0f172a);
  padding-left: 10px;
  color: var(--ui-foreground, #0f172a);
  font-size: 13px;
  line-height: 1.5;
  overflow-wrap: anywhere;
}

.input-question {
  color: var(--ui-muted-foreground, #64748b);
  font-size: 12px;
}

.choices {
  gap: 4px;
}

.option {
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 40px;
  border: 1px solid transparent;
  border-radius: var(--ui-radius, 8px);
  padding: 8px 10px;
  color: var(--ui-foreground, #0f172a);
  font-size: 14px;
  line-height: 20px;
  cursor: pointer;
  transition: background-color 120ms ease, color 120ms ease, opacity 120ms ease;
}

.option:hover:not(.disabled),
.option.selected {
  background: var(--ui-accent, #f1f5f9);
  color: var(--ui-accent-foreground, #0f172a);
}

.option.disabled {
  cursor: not-allowed;
  opacity: 0.5;
}

.option-label {
  min-width: 0;
  overflow-wrap: anywhere;
}

.control-native {
  position: absolute;
  width: 1px;
  height: 1px;
  margin: -1px;
  padding: 0;
  overflow: hidden;
  border: 0;
  white-space: nowrap;
  clip: rect(0 0 0 0);
  clip-path: inset(50%);
}

.selection-indicator {
  position: relative;
  display: inline-flex;
  box-sizing: border-box;
  width: 16px;
  height: 16px;
  flex: 0 0 16px;
  align-items: center;
  justify-content: center;
  border: 1px solid var(--ui-input, #e2e8f0);
  background: var(--ui-background, #fff);
  color: var(--ui-primary-foreground, #f8fafc);
  transition: border-color 120ms ease, background-color 120ms ease, box-shadow 120ms ease;
}

.selection-indicator.checkbox {
  border-radius: 4px;
}

.selection-indicator.radio {
  border-radius: 999px;
}

.selection-indicator::after {
  content: '';
  display: block;
  opacity: 0;
}

.selection-indicator.checkbox::after {
  width: 7px;
  height: 4px;
  border-bottom: 2px solid currentColor;
  border-left: 2px solid currentColor;
  transform: translateY(-1px) rotate(-45deg);
}

.selection-indicator.radio::after {
  width: 6px;
  height: 6px;
  border-radius: 999px;
  background: currentColor;
}

.control-native:checked + .selection-indicator {
  border-color: var(--ui-primary, #0f172a);
  background: var(--ui-primary, #0f172a);
}

.control-native:checked + .selection-indicator::after {
  opacity: 1;
}

.control-native:focus-visible + .selection-indicator {
  box-shadow:
    0 0 0 2px var(--ui-background, #fff),
    0 0 0 4px var(--ui-ring, #0f172a);
}

.control-native:disabled + .selection-indicator {
  cursor: not-allowed;
}

.field {
  display: grid;
  grid-template-columns: minmax(80px, 112px) minmax(0, 1fr);
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.field-label {
  color: var(--ui-muted-foreground, #64748b);
  overflow-wrap: anywhere;
}

.field-label b {
  margin-left: 2px;
  color: var(--ui-status-error, #dc2626);
}

.boolean-field {
  cursor: pointer;
}

.boolean-field:has(.control-native:disabled) {
  cursor: not-allowed;
  opacity: 0.5;
}

.select-control {
  position: relative;
  display: block;
  min-width: 0;
}

input[type='text'],
input[type='number'],
select {
  box-sizing: border-box;
  width: 100%;
  height: 40px;
  min-width: 0;
  padding: 8px 12px;
  border: 1px solid var(--ui-input, #e2e8f0);
  border-radius: var(--ui-radius, 8px);
  background: var(--ui-background, #fff);
  color: var(--ui-foreground, #0f172a);
  font: inherit;
  font-size: 14px;
  line-height: 20px;
  outline: none;
  transition: border-color 120ms ease, box-shadow 120ms ease, opacity 120ms ease;
}

input[type='text']::placeholder,
input[type='number']::placeholder {
  color: var(--ui-muted-foreground, #64748b);
}

select {
  appearance: none;
  padding-right: 38px;
  cursor: pointer;
}

input[type='text']:focus-visible,
input[type='number']:focus-visible,
select:focus-visible {
  box-shadow:
    0 0 0 2px var(--ui-background, #fff),
    0 0 0 4px var(--ui-ring, #0f172a);
}

input[type='text']:disabled,
input[type='number']:disabled,
select:disabled {
  cursor: not-allowed;
  opacity: 0.5;
}

select option {
  background: var(--ui-background, #fff);
  color: var(--ui-foreground, #0f172a);
}

.select-chevron {
  position: absolute;
  top: 50%;
  right: 14px;
  width: 7px;
  height: 7px;
  border-right: 1.5px solid currentColor;
  border-bottom: 1.5px solid currentColor;
  color: var(--ui-foreground, #0f172a);
  pointer-events: none;
  opacity: 0.5;
  transform: translateY(-70%) rotate(45deg);
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
  display: inline-flex;
  height: 40px;
  min-width: 72px;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px solid transparent;
  border-radius: var(--ui-radius, 8px);
  padding: 8px 16px;
  cursor: pointer;
  font: inherit;
  font-size: 14px;
  font-weight: 500;
  line-height: 20px;
  letter-spacing: 0;
  white-space: nowrap;
  transition: background-color 120ms ease, color 120ms ease, opacity 120ms ease;
}

button.primary,
button.approve {
  background: var(--ui-primary, #0f172a);
  color: var(--ui-primary-foreground, #f8fafc);
}

button.primary:hover:not(:disabled),
button.approve:hover:not(:disabled) {
  background: color-mix(in srgb, var(--ui-primary, #0f172a) 90%, transparent);
}

button.reject {
  background: var(--ui-destructive, #dc2626);
  color: #f8fafc;
}

button.reject:hover:not(:disabled) {
  background: color-mix(in srgb, var(--ui-destructive, #dc2626) 90%, transparent);
}

button.secondary {
  border-color: var(--ui-input, #e2e8f0);
  background: var(--ui-background, #fff);
  color: var(--ui-foreground, #0f172a);
}

button.secondary:hover:not(:disabled) {
  background: var(--ui-accent, #f1f5f9);
  color: var(--ui-accent-foreground, #0f172a);
}

button.close {
  width: 32px;
  min-width: 32px;
  height: 32px;
  border-color: transparent;
  padding: 0;
  background: transparent;
  color: var(--ui-muted-foreground, #64748b);
  font-size: 20px;
  font-weight: 400;
  line-height: 1;
}

button.close:hover:not(:disabled) {
  background: var(--ui-accent, #f1f5f9);
  color: var(--ui-accent-foreground, #0f172a);
}

button:focus-visible {
  outline: none;
  box-shadow:
    0 0 0 2px var(--ui-background, #fff),
    0 0 0 4px var(--ui-ring, #0f172a);
}

button:disabled {
  cursor: not-allowed;
  opacity: 0.5;
}

.error {
  margin-top: 8px;
  color: var(--ui-status-error, #dc2626);
  font-size: 12px;
  overflow-wrap: anywhere;
}

@media (max-width: 420px) {
  .field { grid-template-columns: 1fr; gap: 4px; }
  .actions button { flex: 1 1 auto; }
}
</style>
