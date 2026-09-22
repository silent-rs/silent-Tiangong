import { describe, expect, it } from 'vitest';
import {
  MENTION_GROUP_DISPLAY_LIMIT,
  isScanningPlaceholder,
  selectDisplayGroups,
  selectableCandidates,
  type MentionCandidateView,
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

  it('空过滤词时保留只含索引建立中提示的文件组', () => {
    const groups: MentionGroup[] = [
      { kind: 'file', label: 'file', candidates: [scanningPlaceholder()] },
    ];
    const result = selectDisplayGroups(groups, '');
    expect(result.map(g => g.kind)).toEqual(['file']);
    expect(result[0].candidates).toHaveLength(1);
  });

  it('空过滤词时文件组混有真实候选仍剔除', () => {
    const groups: MentionGroup[] = [
      {
        kind: 'file',
        label: 'file',
        candidates: [scanningPlaceholder(), ...group('file', 2).candidates],
      },
    ];
    expect(selectDisplayGroups(groups, '')).toEqual([]);
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

/** 索引建立中占位候选：index 插件用 value 为空透传 scanning 状态。 */
function scanningPlaceholder(): MentionCandidateView {
  return {
    value: '',
    label: '索引创建中…',
    kind: 'file',
    hint: '正在扫描工作区文件，请稍候',
    mark: '…',
  };
}

describe('isScanningPlaceholder', () => {
  it('value 为空即占位候选', () => {
    expect(isScanningPlaceholder(scanningPlaceholder())).toBe(true);
  });

  it('真实候选不是占位', () => {
    expect(
      isScanningPlaceholder({ value: '@file:src/main.rs', label: 'main.rs', kind: 'file', hint: '' }),
    ).toBe(false);
  });
});

describe('selectableCandidates', () => {
  it('剔除索引建立中占位，保留真实候选', () => {
    const candidates = [
      scanningPlaceholder(),
      { value: '@file:src/lib.rs', label: 'lib.rs', kind: 'file', hint: '' },
      { value: '@skill:foo', label: 'foo', kind: 'skill', hint: '' },
    ];
    const result = selectableCandidates(candidates);
    expect(result.map(c => c.value)).toEqual(['@file:src/lib.rs', '@skill:foo']);
  });

  it('没有占位时原样返回全部候选', () => {
    const candidates = [
      { value: '@file:a.rs', label: 'a.rs', kind: 'file', hint: '' },
      { value: '@file:b.rs', label: 'b.rs', kind: 'file', hint: '' },
    ];
    expect(selectableCandidates(candidates)).toHaveLength(2);
  });

  it('不修改入参数组', () => {
    const candidates = [scanningPlaceholder(), { value: '@file:a.rs', label: 'a.rs', kind: 'file', hint: '' }];
    const before = JSON.stringify(candidates);
    selectableCandidates(candidates);
    expect(JSON.stringify(candidates)).toBe(before);
  });

  it('空列表返回空列表', () => {
    expect(selectableCandidates([])).toEqual([]);
  });
});
