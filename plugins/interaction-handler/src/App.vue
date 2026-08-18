<script setup lang="ts">
import { computed, onMounted, onUnmounted, reactive, ref } from 'vue';
import {
  createInteractionHandler,
  createTiangongBridge,
  type HostBridge,
  type InteractionHandler,
} from '@tiangong/plugin-sdk';

/**
 * 默认交互处理器（Vue 工程）。
 *
 * 职责（宿主保留闭合判定/授权/超时权威，方案 §2）：
 * - 监听 interaction.requested 展示六种请求（approval/confirm/choice/
 *   multi_choice/input/form），按宿主权威 deadline 显示剩余时间；
 * - 提交 interaction.resolve（提交后进入 submitting，禁止重复点击）；
 * - 以 interaction.closed 为最终状态，迟到/过期/取消展示真实结果。
 * - 主题：消费宿主 hostContext 的设计 token CSS 变量。
 */

interface InteractionRequestPayload {
  request_id: string;
  kind: 'approval' | 'confirm' | 'choice' | 'multi_choice' | 'input' | 'form';
  title: string;
  description?: string;
  payload: string;
  deadline?: string;
}

interface FormField {
  key: string;
  label: string;
  type: 'string' | 'number' | 'boolean' | 'select';
  options?: string[];
}

const empty = ref(true);
const request = ref<InteractionRequestPayload | null>(null);
const closedStatus = ref<string | null>(null);
const submitting = ref(false);
const error = ref('');
const formValues = reactive<Record<string, string>>({});
const selected = ref<string[]>([]);
const remainingMs = ref<number | null>(null);

let handler: InteractionHandler | null = null;
let countdownTimer: ReturnType<typeof setInterval> | null = null;
let bridgeRef: HostBridge | null = null;

const options = computed<string[]>(() => {
  if (!request.value) return [];
  try {
    return JSON.parse(request.value.payload) as string[];
  } catch {
    return [];
  }
});

const fields = computed<FormField[]>(() => {
  if (!request.value) return [];
  try {
    return JSON.parse(request.value.payload) as FormField[];
  } catch {
    return [];
  }
});

const question = computed(() => {
  try {
    return request.value ? (JSON.parse(request.value.payload) as string) : '';
  } catch {
    return request.value?.payload ?? '';
  }
});

const remainingText = computed(() => {
  if (remainingMs.value == null || remainingMs.value <= 0) return '已到期限';
  const seconds = Math.ceil(remainingMs.value / 1000);
  return `剩余 ${seconds} 秒`;
});

const localExpired = computed(
  () => remainingMs.value != null && remainingMs.value <= 0,
);
const locked = computed(
  () => submitting.value || closedStatus.value != null || localExpired.value,
);

function startCountdown(deadline?: string) {
  stopCountdown();
  if (!deadline) {
    remainingMs.value = null;
    return;
  }
  const deadlineMs = Date.parse(deadline);
  if (Number.isNaN(deadlineMs)) {
    remainingMs.value = null;
    return;
  }
  const tick = () => {
    remainingMs.value = deadlineMs - Date.now();
  };
  tick();
  countdownTimer = setInterval(tick, 250);
}

function stopCountdown() {
  if (countdownTimer) {
    clearInterval(countdownTimer);
    countdownTimer = null;
  }
}

function resetForm() {
  Object.keys(formValues).forEach((key) => delete formValues[key]);
  selected.value = [];
  error.value = '';
}

function toggleChoice(option: string) {
  if (locked.value) return;
  if (request.value?.kind === 'multi_choice') {
    selected.value = selected.value.includes(option)
      ? selected.value.filter((item) => item !== option)
      : [...selected.value, option];
  } else {
    selected.value = [option];
  }
}

async function resolve(result: unknown) {
  if (!handler || !request.value || locked.value) return;
  submitting.value = true;
  error.value = '';
  try {
    await handler.resolve(request.value.request_id, result);
    // 提交成功后等待 interaction.closed；保持 submitting 防重复
  } catch (e) {
    error.value = String(e);
    submitting.value = false;
  }
}

function resolveKindResult() {
  const kind = request.value?.kind;
  if (kind === 'choice') {
    resolve(selected.value[0]);
  } else if (kind === 'multi_choice') {
    resolve(selected.value);
  } else if (kind === 'input') {
    resolve(formValues.input ?? '');
  } else {
    const answers: Record<string, unknown> = {};
    for (const field of fields.value) {
      const raw = formValues[field.key] ?? '';
      if (field.type === 'number') {
        answers[field.key] = Number(raw) || 0;
      } else if (field.type === 'boolean') {
        answers[field.key] = raw === 'true';
      } else {
        answers[field.key] = raw;
      }
    }
    resolve(answers);
  }
}

/** 消费宿主 hostContext：主题 token 写入 CSS 变量。 */
function applyHostContext(data: Record<string, unknown>) {
  const tokens = (data.tokens ?? {}) as Record<string, string>;
  for (const [name, value] of Object.entries(tokens)) {
    if (value) document.documentElement.style.setProperty(`--${name}`, value);
  }
  if (typeof data.fontFamily === 'string' && data.fontFamily) {
    document.body.style.fontFamily = data.fontFamily;
  }
}

onMounted(async () => {
  window.addEventListener('message', onHostMessage);
  try {
    const bridge = await createTiangongBridge();
    bridgeRef = bridge;
    handler = createInteractionHandler(bridge);
    handler.onRequested((payload) => {
      let parsed: InteractionRequestPayload;
      try {
        parsed = JSON.parse(payload) as InteractionRequestPayload;
      } catch {
        return;
      }
      empty.value = false;
      request.value = parsed;
      closedStatus.value = null;
      submitting.value = false;
      resetForm();
      startCountdown(parsed.deadline);
    });
    handler.onClosed((closed) => {
      if (request.value && closed.request_id === request.value.request_id) {
        closedStatus.value = closed.status;
        submitting.value = false;
        stopCountdown();
      }
    });
  } catch (e) {
    error.value = `桥接连线失败：${String(e)}`;
  }
});

function onHostMessage(event: MessageEvent) {
  const data = event.data as Record<string, unknown> | null;
  if (data?.type === 'tiangong_host_context') {
    applyHostContext(data as Record<string, unknown>);
  }
}

onUnmounted(() => {
  stopCountdown();
  window.removeEventListener('message', onHostMessage);
  void bridgeRef;
});
</script>

<template>
  <div class="wrap">
    <div v-if="empty" class="muted">等待 Agent 发起交互请求…</div>

    <section v-else-if="request">
      <h3>{{ request.title }}</h3>
      <p v-if="request.description" class="muted">{{ request.description }}</p>
      <div class="deadline" :class="{ overdue: localExpired }">
        {{ closedStatus ? `已${closedStatus === 'answered' ? '提交' : closedStatus === 'expired' ? '过期' : '取消'}` : remainingText }}
      </div>

      <!-- choice / multi_choice -->
      <div v-if="request.kind === 'choice' || request.kind === 'multi_choice'" class="content">
        <label v-for="option in options" :key="option" class="option">
          <input
            :type="request.kind === 'multi_choice' ? 'checkbox' : 'radio'"
            :checked="selected.includes(option)"
            :disabled="locked"
            @change="toggleChoice(option)"
          />
          <span>{{ option }}</span>
        </label>
      </div>

      <!-- confirm -->
      <div v-else-if="request.kind === 'confirm'" class="content muted">{{ question }}</div>

      <!-- input -->
      <div v-else-if="request.kind === 'input'" class="content">
        <input
          v-model="formValues.input"
          type="text"
          placeholder="请输入…"
          :disabled="locked"
        />
      </div>

      <!-- form -->
      <div v-else-if="request.kind === 'form'" class="content">
        <template v-for="field in fields" :key="field.key">
          <label class="field">
            <span class="field-label">{{ field.label }}</span>
            <input
              v-if="field.type === 'boolean'"
              v-model="formValues[field.key]"
              type="checkbox"
              :true-value="'true'"
              :false-value="'false'"
              :disabled="locked"
            />
            <select
              v-else-if="field.type === 'select'"
              v-model="formValues[field.key]"
              :disabled="locked"
            >
              <option value="" disabled>请选择</option>
              <option v-for="option in field.options ?? []" :key="option" :value="option">
                {{ option }}
              </option>
            </select>
            <input
              v-else
              v-model="formValues[field.key]"
              :type="field.type === 'number' ? 'number' : 'text'"
              :disabled="locked"
            />
          </label>
        </template>
      </div>

      <div class="actions">
        <template v-if="request.kind === 'approval'">
          <button class="primary" :disabled="locked" @click="resolve({ decision: 'approve_once' })">仅本次允许</button>
          <button class="runtime" :disabled="locked" @click="resolve({ decision: 'approve_for_runtime' })">本次运行内允许</button>
          <button class="reject" :disabled="locked" @click="resolve({ decision: 'reject' })">拒绝</button>
        </template>
        <template v-else-if="request.kind === 'confirm'">
          <button class="primary" :disabled="locked" @click="resolve(true)">是</button>
          <button class="reject" :disabled="locked" @click="resolve(false)">否</button>
        </template>
        <button
          v-else
          class="primary"
          :disabled="locked || ((request.kind === 'choice' || request.kind === 'multi_choice') && selected.length === 0)"
          @click="resolveKindResult()"
        >
          {{ submitting ? '提交中…' : '提交' }}
        </button>
      </div>

      <div v-if="error" class="error">{{ error }}</div>
    </section>
  </div>
</template>

<style scoped>
.wrap { padding: 14px; color: var(--foreground, #222); }
h3 { margin: 0 0 6px; font-size: 15px; }
.muted { color: var(--muted-foreground, #777); font-size: 12px; margin: 0 0 10px; }
.deadline { font-size: 12px; color: var(--muted-foreground, #888); margin-bottom: 10px; }
.deadline.overdue { color: var(--status-error, #dc2626); }
.content { display: grid; gap: 7px; margin-bottom: 10px; }
.option { display: flex; align-items: center; gap: 7px; font-size: 13px; }
.field { display: flex; align-items: center; gap: 8px; font-size: 13px; }
.field-label { width: 88px; flex-shrink: 0; color: var(--muted-foreground, #777); }
input[type='text'], input[type='number'], select {
  box-sizing: border-box; width: 100%; padding: 7px;
  border: 1px solid var(--border, #8886); border-radius: 6px;
  background: transparent; color: inherit; font: inherit;
}
.actions { display: flex; flex-wrap: wrap; gap: 7px; }
button {
  border: 0; border-radius: 6px; padding: 7px 11px; cursor: pointer;
  font: inherit; font-size: 13px; background: var(--primary, #2563eb); color: white;
}
button.runtime { background: var(--status-success, #16803f); }
button.reject { background: var(--status-error, #dc2626); }
button:disabled { opacity: 0.45; cursor: not-allowed; }
.error { margin-top: 8px; color: var(--status-error, #dc2626); font-size: 12px; }
</style>
