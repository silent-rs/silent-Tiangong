/**
 * 定时任务 cron 表达式工具：与后端 cron 0.16 语义对齐（6 字段：秒 分 时 日 月 周）。
 *
 * 简单模式（分/时/星期）<—> 6 字段 cron 字符串 的互转，以及校验与下次触发时间预览。
 * 所有日期计算使用本地时区（与后端执行 `chrono::Local` 一致）。
 */

import { CronExpressionParser } from 'cron-parser';
import cronstrue from 'cronstrue';

/** 默认本地时区（与后端 chrono::Local 一致）。cron-parser 用 Intl 时区名。 */
function localTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
}

/** 6 字段 cron 的各字段下标。 */
const FIELD_SEC = 0;
const FIELD_MIN = 1;
const FIELD_HOUR = 2;
const FIELD_DOM = 3;
const FIELD_MON = 4;
const FIELD_DOW = 5;
const EXPECTED_FIELDS = 6;

/**
 * 简单模式可表达的「星期几」预设。
 *
 * cron 周字段：0/7=周日, 1=周一 … 6=周六（与后端 cron 0.16 一致）。
 */
export interface WeekdayOption {
  /** 用于 Select 的稳定 key。 */
  value: string;
  /** 展示名。 */
  label: string;
  /** 写入 cron 周字段的值。 */
  cron: string;
}

export const WEEKDAY_OPTIONS: WeekdayOption[] = [
  { value: '*', label: '每天', cron: '*' },
  { value: '1-5', label: '工作日（周一至周五）', cron: '1-5' },
  { value: '0,6', label: '周末', cron: '0,6' },
  { value: '1', label: '周一', cron: '1' },
  { value: '2', label: '周二', cron: '2' },
  { value: '3', label: '周三', cron: '3' },
  { value: '4', label: '周四', cron: '4' },
  { value: '5', label: '周五', cron: '5' },
  { value: '6', label: '周六', cron: '6' },
  { value: '0', label: '周日', cron: '0' },
];

/** 简单模式表单值。 */
export interface SimpleSchedule {
  minute: number;
  hour: number;
  /** Select 的 value（见 WEEKDAY_OPTIONS）；默认 '*'。 */
  weekday: string;
}

/** 简单模式默认值：每天 09:00。 */
export const DEFAULT_SIMPLE: SimpleSchedule = { minute: 0, hour: 9, weekday: '*' };

/** 把简单模式合成为 6 字段 cron 字符串（秒固定 0、日/月补 *）。 */
export function buildSchedule(s: SimpleSchedule): string {
  const weekdayCron =
    WEEKDAY_OPTIONS.find((o) => o.value === s.weekday)?.cron ?? '*';
  const mm = clamp(s.minute, 0, 59);
  const hh = clamp(s.hour, 0, 23);
  return `0 ${mm} ${hh} * * ${weekdayCron}`;
}

/**
 * 尝试把现有 cron 字符串解析回简单模式字段。
 *
 * 仅当表达式严格符合简单模式可表达的形态（秒=0、日=*、月=*、分/时为单个整数、
 * 周字段命中某个预设）时才返回 SimpleSchedule；否则返回 null（表单留在 cron 模式）。
 */
export function tryParseToSimple(expr: string | null | undefined): SimpleSchedule | null {
  if (!expr) return null;
  const fields = expr.trim().split(/\s+/);
  if (fields.length !== EXPECTED_FIELDS) return null;
  if (fields[FIELD_SEC] !== '0') return null;
  if (fields[FIELD_DOM] !== '*' || fields[FIELD_MON] !== '*') return null;
  const minute = parseInt(fields[FIELD_MIN], 10);
  const hour = parseInt(fields[FIELD_HOUR], 10);
  if (!Number.isInteger(minute) || !Number.isInteger(hour)) return null;
  if (minute < 0 || minute > 59 || hour < 0 || hour > 23) return null;
  // 字段必须就是单个整数（排除 "*/5"、"5,10" 等）
  if (fields[FIELD_MIN] !== String(minute) || fields[FIELD_HOUR] !== String(hour)) {
    return null;
  }
  const weekday = WEEKDAY_OPTIONS.find((o) => o.cron === fields[FIELD_DOW])?.value;
  if (!weekday) return null;
  return { minute, hour, weekday };
}

export interface ValidationResult {
  ok: boolean;
  /** 非法时的简要说明（用于 UI 红字提示）。 */
  error?: string;
}

/**
 * 校验 cron 表达式是否合法且为 6 字段。
 *
 * 注意：cron-parser v5 默认宽松（5/6 字段都接受），但后端 cron 0.16 严格要求 6 字段。
 * 这里先断言字段数 == 6，再用 cron-parser 验证语义，确保前后端一致。
 *
 * 说明：后端 cron 0.16 还支持 7 字段（带年），但 cron-parser 不支持；定时任务实际
 * 不需要「年」字段（年度周期用 6 字段即可表达），故前端严格按 6 字段校验。即便用户
 * 确需 7 字段，后端 job_create/job_update 会兜底校验。
 */
export function validateCron(expr: string): ValidationResult {
  const trimmed = expr.trim();
  if (!trimmed) return { ok: false, error: '请输入 cron 表达式' };
  const fields = trimmed.split(/\s+/);
  if (fields.length < EXPECTED_FIELDS) {
    return { ok: false, error: `需要 6 个字段（秒 分 时 日 月 周），当前只有 ${fields.length} 个` };
  }
  if (fields.length > EXPECTED_FIELDS) {
    return { ok: false, error: '应为 6 个字段（秒 分 时 日 月 周）；带年的 7 字段请直接保存由后端校验' };
  }
  try {
    CronExpressionParser.parse(trimmed, { tz: localTimeZone() });
    return { ok: true };
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { ok: false, error: firstLine(msg) || '表达式语法错误' };
  }
}

/**
 * 计算接下来 count 次触发时间（本地时区）。
 *
 * @returns 成功返回 Date 数组；表达式非法或无解返回 null。
 */
export function nextRuns(expr: string, count: number): Date[] | null {
  if (!validateCron(expr).ok) return null;
  try {
    const it = CronExpressionParser.parse(expr.trim(), { tz: localTimeZone() });
    const out: Date[] = [];
    for (let i = 0; i < count; i++) {
      out.push(it.next().toDate());
    }
    return out;
  } catch {
    return null;
  }
}

/**
 * 把 cron 表达式转成人话描述（优先中文）。
 *
 * cronstrue 面向 5 字段，这里 strip 掉秒字段（第 1 个）再传入。
 * 失败时返回 null（调用方优雅降级，不影响其它预览）。
 */
export function humanizeCron(expr: string): string | null {
  const trimmed = expr.trim();
  if (!validateCron(trimmed).ok) return null;
  // 去秒字段：6 字段 -> 5 字段
  const fields = trimmed.split(/\s+/);
  const five = fields.slice(1).join(' ');
  try {
    return cronstrue.toString(five, { throwExceptionOnParseError: false }) || null;
  } catch {
    return null;
  }
}

// ── 内部辅助 ──────────────────────────────────────────────────

function clamp(n: number, min: number, max: number): number {
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, Math.trunc(n)));
}

function firstLine(s: string): string {
  const idx = s.indexOf('\n');
  return idx === -1 ? s : s.slice(0, idx);
}
