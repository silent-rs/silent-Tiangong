import { describe, expect, it } from 'vitest';
import {
  MENTION_GROUP_DISPLAY_LIMIT,
  selectDisplayGroups,
  type MentionGroup,
} from '@/utils/mentionGroups';

function group(kind: string, count: number): MentionGroup {
  return {
    kind,
    label: kind,
    candidates: Array.from({ length: count }, (_, index) => ({
      value: `@${kind}:${index}`,
      label: `${kind}-${index}`,
      kind,
      hint: '',
    })),
  };
}

describe('selectDisplayGroups', () => {
  it('空过滤词时剔除文件组，枚举型候选不受影响', () => {
    const groups = [group('file', 3), group('skill', 2), group('mcp', 1)];
    const result = selectDisplayGroups(groups, '');
    expect(result.map(g => g.kind)).toEqual(['skill', 'mcp']);
  });

  it('全空白过滤词同样不出文件组', () => {
    const groups = [group('file', 3), group('agent', 1)];
    expect(selectDisplayGroups(groups, '   ').map(g => g.kind)).toEqual(['agent']);
  });

  it('有过滤词时保留文件组', () => {
    const groups = [group('file', 2), group('skill', 1)];
    const result = selectDisplayGroups(groups, 'main');
    expect(result.map(g => g.kind)).toEqual(['file', 'skill']);
    expect(result[0].candidates).toHaveLength(2);
  });

  it('每组截断到展示上限', () => {
    const result = selectDisplayGroups([group('file', MENTION_GROUP_DISPLAY_LIMIT + 20)], 'x');
    expect(result[0].candidates).toHaveLength(MENTION_GROUP_DISPLAY_LIMIT);
    // 截断保留前 N 条，顺序不变
    expect(result[0].candidates[0].label).toBe('file-0');
  });

  it('保留组的原始顺序，不重排', () => {
    const groups = [group('mcp', 1), group('skill', 1), group('agent', 1)];
    expect(selectDisplayGroups(groups, 'a').map(g => g.kind)).toEqual(['mcp', 'skill', 'agent']);
  });

  it('不修改入参数组', () => {
    const groups = [group('file', MENTION_GROUP_DISPLAY_LIMIT + 1)];
    const before = JSON.stringify(groups);
    selectDisplayGroups(groups, 'x');
    expect(JSON.stringify(groups)).toBe(before);
  });
});
