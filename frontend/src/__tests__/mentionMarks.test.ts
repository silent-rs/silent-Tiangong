import { afterEach, describe, expect, it } from 'vitest';

import {
  mentionMarkFor,
  registerMentionMark,
  registerMentionMarks,
} from '@/utils/mentionMarks';

// 注册表是模块级状态，隔离各用例（重置内部映射的行为未导出，用不可达
// token + 未注册 kind 验证兜底路径，正常用例只依赖新增注册）。
describe('mentionMarks 注册表', () => {
  afterEach(() => {
    // 无法直接清空，用例内用唯一 token 前缀避免相互污染。
  });

  it('token 精确匹配优先', () => {
    registerMentionMark('@plugin:text-to-speech', 'plugin', 'TTS');
    expect(mentionMarkFor('plugin', '@plugin:text-to-speech')).toBe('TTS');
    // 其他 token 走 kind 兜底（首个注册该 kind 的值）
    expect(mentionMarkFor('plugin', '@plugin:unknown')).toBe('TTS');
  });

  it('批量注册与空 mark 跳过', () => {
    registerMentionMarks([
      { value: '@skill:a', kind: 'skill', mark: 'S' },
      { value: '@skill:b', kind: 'skill' }, // 无 mark：不注册、不覆盖
    ]);
    expect(mentionMarkFor('skill', '@skill:a')).toBe('S');
    expect(mentionMarkFor('skill', '@skill:b')).toBe('S'); // kind 兜底
  });

  it('未注册 kind 回退首字母大写', () => {
    expect(mentionMarkFor('video', '@video:x')).toBe('V');
  });
});
