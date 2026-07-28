import { describe, expect, it } from 'vitest';

import {
  deleteMentionSelection,
  getMentionBoundaries,
  insertTextAtMentionBoundary,
  normalizePastedText,
  replaceMentionCompletion,
  resolveMentionKeyAction,
  type MentionKeyAction,
} from '@/utils/mentionEditorModel';

function applyKeyAction(text: string, action: MentionKeyAction | null): string {
  if (!action || action.type === 'move') return text;
  return text.slice(0, action.start) + text.slice(action.end);
}

describe('提及编辑规则', () => {
  it('计算连续提及及两侧分隔位置', () => {
    expect(getMentionBoundaries('@dev @qa ')).toEqual([
      {
        start: 0,
        end: 4,
        leadingSeparatorStart: null,
        trailingSeparatorEnd: 5,
      },
      {
        start: 5,
        end: 8,
        leadingSeparatorStart: 4,
        trailingSeparatorEnd: 9,
      },
    ]);
  });

  it('Delete 在提及左侧一次删除提及及右侧分隔', () => {
    const text = '@dev 继续';
    const action = resolveMentionKeyAction(text, 0, 'Delete');

    expect(action).toEqual({ type: 'delete', start: 0, end: 5, offset: 0 });
    expect(applyKeyAction(text, action)).toBe('继续');
  });

  it('Backspace 在提及右侧一次删除提及及左侧分隔', () => {
    const text = '继续 @dev';
    const action = resolveMentionKeyAction(text, text.length, 'Backspace');

    expect(action).toEqual({ type: 'delete', start: 2, end: 7, offset: 2 });
    expect(applyKeyAction(text, action)).toBe('继续');
  });

  it('连续提及左右移动时自动跨过标签及分隔', () => {
    const text = '@dev @qa ';
    const right1 = resolveMentionKeyAction(text, 0, 'ArrowRight');
    const right2 = resolveMentionKeyAction(text, right1?.type === 'move' ? right1.offset : -1, 'ArrowRight');
    const left1 = resolveMentionKeyAction(text, 9, 'ArrowLeft');
    const left2 = resolveMentionKeyAction(text, left1?.type === 'move' ? left1.offset : -1, 'ArrowLeft');

    expect(right1).toEqual({ type: 'move', offset: 5 });
    expect(right2).toEqual({ type: 'move', offset: 9 });
    expect(left1).toEqual({ type: 'move', offset: 4 });
    expect(left2).toEqual({ type: 'move', offset: 0 });
  });

  it('跨多个提及选择删除时扩展到完整标签并保留单个分隔', () => {
    expect(deleteMentionSelection('前 @dev @qa 后', 4, 8)).toEqual({
      value: '前 后',
      offset: 2,
    });
  });

  it('全选删除后结果为空', () => {
    const text = '@dev\n第二行 @qa ';
    expect(deleteMentionSelection(text, 0, text.length)).toEqual({
      value: '',
      offset: 0,
    });
  });

  it('候选替换后返回标签右侧的光标位置', () => {
    expect(replaceMentionCompletion('@de', 0, 3, '@dev')).toEqual({
      value: '@dev ',
      offset: 5,
    });
    expect(replaceMentionCompletion('请 @de处理', 2, 5, '@dev')).toEqual({
      value: '请 @dev 处理',
      offset: 7,
    });
  });

  it('在连续标签的共享分隔两侧输入时都保留两个标签', () => {
    expect(insertTextAtMentionBoundary('@dev @qa ', 4, '中')).toEqual({
      value: '@dev 中 @qa ',
      offset: 6,
    });
    expect(insertTextAtMentionBoundary('@dev @qa ', 5, '中')).toEqual({
      value: '@dev 中 @qa ',
      offset: 6,
    });
  });

  it('拒绝越界的候选替换位置', () => {
    expect(replaceMentionCompletion('@de', -1, 3, '@dev')).toBeNull();
    expect(replaceMentionCompletion('@de', 2, 1, '@dev')).toBeNull();
  });

  it('粘贴文本统一 Windows 和旧式换行', () => {
    expect(normalizePastedText('第一行\r\n第二行\r第三行')).toBe('第一行\n第二行\n第三行');
  });
});
