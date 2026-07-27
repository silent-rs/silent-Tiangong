import { describe, expect, it } from 'vitest';
import {
  buildSchedule,
  DEFAULT_SIMPLE,
  humanizeCron,
  nextRuns,
  tryParseToSimple,
  validateCron,
  WEEKDAY_OPTIONS,
} from '../lib/cron';

describe('buildSchedule', () => {
  it('合成 6 字段表达式（秒固定 0、日/月补 *）', () => {
    expect(buildSchedule({ minute: 25, hour: 21, weekday: '*' })).toBe('0 25 21 * * *');
    expect(buildSchedule({ minute: 0, hour: 9, weekday: '1-5' })).toBe('0 0 9 * * 1-5');
    expect(buildSchedule({ minute: 30, hour: 8, weekday: '0' })).toBe('0 30 8 * * 0');
  });

  it('分钟/小时越界自动 clamp', () => {
    expect(buildSchedule({ minute: 99, hour: 25, weekday: '*' })).toBe('0 59 23 * * *');
    expect(buildSchedule({ minute: -5, hour: -1, weekday: '*' })).toBe('0 0 0 * * *');
  });

  it('未知 weekday 值回退为 *', () => {
    expect(buildSchedule({ minute: 0, hour: 9, weekday: 'bogus' })).toBe('0 0 9 * * *');
  });
});

describe('tryParseToSimple', () => {
  it('把简单模式可表达的 6 字段解析回字段', () => {
    expect(tryParseToSimple('0 25 21 * * *')).toEqual({
      minute: 25,
      hour: 21,
      weekday: '*',
    });
    expect(tryParseToSimple('0 0 9 * * 1-5')).toEqual({
      minute: 0,
      hour: 9,
      weekday: '1-5',
    });
  });

  it('往返一致：build 出来的都能 parse 回来', () => {
    for (const opt of WEEKDAY_OPTIONS) {
      const s = { minute: 15, hour: 10, weekday: opt.value };
      expect(tryParseToSimple(buildSchedule(s))).toEqual(s);
    }
  });

  it('null/空串/缺省返回 null', () => {
    expect(tryParseToSimple(null)).toBeNull();
    expect(tryParseToSimple('')).toBeNull();
    expect(tryParseToSimple(undefined)).toBeNull();
  });

  it('5 字段返回 null（留在 cron 模式）', () => {
    expect(tryParseToSimple('0 9 * * *')).toBeNull();
    expect(tryParseToSimple('25 21 * * *')).toBeNull();
  });

  it('秒非 0 返回 null', () => {
    expect(tryParseToSimple('30 0 9 * * *')).toBeNull();
  });

  it('日/月非 * 返回 null', () => {
    expect(tryParseToSimple('0 0 9 1 * *')).toBeNull();
    expect(tryParseToSimple('0 0 9 * 1 *')).toBeNull();
  });

  it('分/时带步进或列表返回 null（简单模式无法表达）', () => {
    expect(tryParseToSimple('0 */5 9 * * *')).toBeNull();
    expect(tryParseToSimple('0 0,30 9 * * *')).toBeNull();
  });

  it('周字段非预设值返回 null', () => {
    expect(tryParseToSimple('0 0 9 * * 2/2')).toBeNull();
  });
});

describe('validateCron', () => {
  it('合法 6 字段通过', () => {
    expect(validateCron('0 0 9 * * *').ok).toBe(true);
    expect(validateCron('0 25 21 * * *').ok).toBe(true);
    expect(validateCron('0 */15 * * * *').ok).toBe(true);
    expect(validateCron('0 0 9 * * 1-5').ok).toBe(true);
  });

  it('5 字段被拒（与后端 cron 0.16 一致，关键回归点）', () => {
    const r = validateCron('0 9 * * *');
    expect(r.ok).toBe(false);
    expect(r.error).toMatch(/6 个字段|当前只有 5/);
    const r2 = validateCron('25 21 * * *');
    expect(r2.ok).toBe(false);
  });

  it('字段过多被拒（>6）', () => {
    expect(validateCron('0 0 0 * * * *').ok).toBe(false);
    expect(validateCron('0 0 0 * * * * *').ok).toBe(false);
  });

  it('空串被拒', () => {
    expect(validateCron('').ok).toBe(false);
    expect(validateCron('   ').ok).toBe(false);
  });

  it('语法错误被拒并带说明', () => {
    const r = validateCron('0 0 99 * * *');
    expect(r.ok).toBe(false);
    expect(r.error).toBeTruthy();
  });
});

describe('nextRuns', () => {
  it('返回指定数量的触发时间，且递增', () => {
    const runs = nextRuns('0 0 9 * * *', 3);
    expect(runs).not.toBeNull();
    expect(runs!.length).toBe(3);
    expect(runs![0].getTime()).toBeLessThan(runs![1].getTime());
    expect(runs![1].getTime()).toBeLessThan(runs![2].getTime());
  });

  it('非法表达式返回 null', () => {
    expect(nextRuns('0 9 * * *', 3)).toBeNull();
    expect(nextRuns('bogus', 3)).toBeNull();
  });

  it('每天的下次触发应为约 24 小时后', () => {
    const runs = nextRuns('0 0 9 * * *', 2);
    if (!runs) return;
    const gap = runs[1].getTime() - runs[0].getTime();
    // 允许 DST 切换造成的轻微偏移，正常约 24h
    expect(gap).toBeGreaterThanOrEqual(23 * 3600_000);
    expect(gap).toBeLessThanOrEqual(25 * 3600_000);
  });
});

describe('humanizeCron', () => {
  it('合法表达式返回非空字符串', () => {
    const h = humanizeCron('0 0 9 * * *');
    expect(typeof h).toBe('string');
    expect(h!.length).toBeGreaterThan(0);
  });

  it('非法表达式返回 null（优雅降级）', () => {
    expect(humanizeCron('0 9 * * *')).toBeNull();
    expect(humanizeCron('bogus')).toBeNull();
  });
});

describe('DEFAULT_SIMPLE', () => {
  it('默认为每天 9:00', () => {
    expect(buildSchedule(DEFAULT_SIMPLE)).toBe('0 0 9 * * *');
  });
});
