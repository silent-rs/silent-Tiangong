import type {
  ToolClosed,
  ToolInvocation,
  ToolResolution,
} from '@tiangong/plugin-sdk';

export type InteractionKind = 'approval' | 'confirm' | 'choice' | 'multi_choice' | 'input' | 'form';
export type ApprovalOpinion = 'approve' | 'reject';
type RequestStatus = 'pending' | 'submitting' | ToolClosed['status'];

interface FormField {
  key: string;
  label: string;
  type: 'string' | 'number' | 'boolean' | 'select';
  options?: string[];
  placeholder?: string;
  required?: boolean;
}

export interface InteractionRequest {
  invocationId: string;
  sessionId: string;
  kind: InteractionKind;
  title: string;
  description: string;
  question: string;
  options: string[];
  fields: FormField[];
  deadlineMs: number;
  createdAtMs: number;
  status: RequestStatus;
  error: string;
  selected: string[];
  values: Record<string, string | boolean>;
}

export const USER_TIMEOUT_MS = 15_000;

const HSL_CHANNELS = /^-?(?:\d+(?:\.\d+)?)(?:deg|rad|grad|turn)?\s+-?(?:\d+(?:\.\d+)?)%\s+-?(?:\d+(?:\.\d+)?)%(?:\s*\/\s*(?:\d+(?:\.\d+)?%?))?$/;

export function approvalOpinion(decision: ApprovalOpinion) {
  return { decision } as const;
}

export function normalizeHostTokenValue(value: string): string {
  const normalized = value.trim();
  return HSL_CHANNELS.test(normalized) ? `hsl(${normalized})` : normalized;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function optionalText(value: unknown, fallback = ''): string {
  if (typeof value !== 'string') return fallback;
  return value.trim() || fallback;
}

function parseOptions(value: unknown, fieldName: string): string[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error(`${fieldName} 必须是非空字符串数组`);
  }
  const options = value.map((item) => {
    if (typeof item !== 'string' || item.trim() === '') {
      throw new Error(`${fieldName} 必须是非空字符串数组`);
    }
    return item.trim();
  });
  if (new Set(options).size !== options.length) {
    throw new Error(`${fieldName} 不能包含重复项`);
  }
  return options;
}

function parseFields(value: unknown): FormField[] {
  if (!Array.isArray(value) || value.length === 0) {
    throw new Error('fields 必须是非空字段数组');
  }
  const keys = new Set<string>();
  return value.map((item, index) => {
    if (!isRecord(item)) throw new Error(`fields[${index}] 必须是对象`);
    const key = optionalText(item.key);
    if (!key) throw new Error(`fields[${index}] 缺少 key`);
    if (keys.has(key)) throw new Error(`fields 包含重复 key：${key}`);
    keys.add(key);
    const type = optionalText(item.type, 'string');
    if (!['string', 'number', 'boolean', 'select'].includes(type)) {
      throw new Error(`fields[${index}] 的 type 无效`);
    }
    return {
      key,
      label: optionalText(item.label, key),
      type: type as FormField['type'],
      options: type === 'select' ? parseOptions(item.options, `fields[${index}].options`) : undefined,
      placeholder: optionalText(item.placeholder) || undefined,
      required: item.required === true,
    };
  });
}

export function parseInvocation(
  invocation: ToolInvocation,
  currentTimeMs = Date.now(),
): InteractionRequest {
  if (invocation.name !== 'request_user') {
    throw new Error(`不支持工具 ${invocation.name}`);
  }
  if (!isRecord(invocation.arguments)) throw new Error('工具参数必须是对象');
  const args = invocation.arguments;
  const rawKind = optionalText(args.kind);
  if (!['approval', 'confirm', 'choice', 'multi_choice', 'input', 'form'].includes(rawKind)) {
    throw new Error(rawKind ? `kind 无效：${rawKind}` : '缺少 kind');
  }
  const kind = rawKind as InteractionKind;
  const parsedCreatedAt = Date.parse(invocation.created_at);
  const createdAtMs = Number.isFinite(parsedCreatedAt) ? parsedCreatedAt : currentTimeMs;

  return {
    invocationId: invocation.invocation_id,
    sessionId: invocation.session_id,
    kind,
    title: optionalText(args.title, '需要您的输入'),
    description: optionalText(args.description),
    question: optionalText(args.question),
    options: kind === 'choice' || kind === 'multi_choice'
      ? parseOptions(args.options, 'options')
      : [],
    fields: kind === 'form' ? parseFields(args.fields) : [],
    deadlineMs: createdAtMs + USER_TIMEOUT_MS,
    createdAtMs,
    status: 'pending',
    error: '',
    selected: [],
    values: {},
  };
}

export function payloadResult(
  invocationId: string,
  kind: InteractionKind | 'unknown',
  status: 'answered' | 'expired' | 'cancelled' | 'invalid',
  extra: Record<string, unknown>,
  ok: boolean,
): ToolResolution['result'] {
  const payload = JSON.stringify({
    status,
    kind,
    request_id: invocationId,
    ...extra,
  });
  return {
    ok,
    summary: payload,
    stdout: '',
    stderr: ok ? '' : String(extra.message ?? '用户征询未完成'),
    exit_code: ok ? 0 : 1,
  };
}
