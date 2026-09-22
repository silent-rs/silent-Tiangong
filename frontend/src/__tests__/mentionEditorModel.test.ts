import { parseBlocks, serializeBlocks } from '@/utils/mentionBlocks';
import { describe, expect, it } from 'vitest';

import {
  deactivateBlocksInRange,
  deleteMentionSelection,
  getMentionBoundaries,
  insertTextAtMentionBoundary,
  mentionReplaceEnd,
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

  it('选中替换用触发时记录的区间末端，而非当前光标', () => {
    // 过滤词在面板搜索框里，消息文本中只有 `@` 本身
    expect(mentionReplaceEnd(0, 1)).toBe(1);
    // 触发时已打了 `@ab`（mentionEnd=3）：选中后整段替换，不留游离字符
    expect(mentionReplaceEnd(0, 3)).toBe(3);
    // mentionEnd 未记录/非法：退化为只替换 `@`
    expect(mentionReplaceEnd(5, -1)).toBe(6);
    expect(mentionReplaceEnd(5, 5)).toBe(6);
    expect(mentionReplaceEnd(5, 3)).toBe(6);
  });

  it('过滤词不入消息文本：`@` 后接游离字符也能整段替换', () => {
    // 用户在消息框打了 `@ab` 后面板打开，随后在搜索框输入 `design`：
    // 消息文本仍是 `@ab`，选中文件后应整体被 chip token 取代。
    const text = '请看 @ab 谢谢';
    const replacement = replaceMentionCompletion(
      text,
      3,
      mentionReplaceEnd(3, 6),
      '@file:design.md',
    );
    expect(replacement?.value).toBe('请看 @file:design.md  谢谢');
  });

  it('粘贴文本统一 Windows 和旧式换行', () => {
    expect(normalizePastedText('第一行\r\n第二行\r第三行')).toBe('第一行\n第二行\n第三行');
  });
});

describe('活跃区降级', () => {
  it('无活跃区时原样返回', () => {
    const parsed = parseBlocks('@dev @qa');
    expect(deactivateBlocksInRange(parsed, null)).toEqual(parsed);
    expect(deactivateBlocksInRange(parsed, undefined)).toEqual(parsed);
  });

  it('活跃区内的 mention 降级为纯文本，区外保持 chip', () => {
    // 已固化 @dev，用户在其后新输入 `@qa`（活跃区覆盖第二个 @ 到光标）
    const text = '@dev @qa';
    const blocks = parseBlocks(text);
    expect(blocks.map(b => b.type)).toEqual(['mention', 'text', 'mention']);
    const result = deactivateBlocksInRange(blocks, { start: 5, end: 8 });
    expect(result.map(b => b.type)).toEqual(['mention', 'text', 'text']);
    // 降级后序列化结果不变：契约 serialize(parse(text)) === text 保持
    expect(serializeBlocks(result)).toBe(text);
  });

  it('活跃区只覆盖到光标：其后已固化文本不受影响', () => {
    const text = '@dev @qa 说明';
    const blocks = parseBlocks(text);
    const result = deactivateBlocksInRange(blocks, { start: 5, end: 8 });
    expect(result.map(b => b.type)).toEqual(['mention', 'text', 'text', 'text']);
  });

  it('区间未覆盖 mention 起点时不降级', () => {
    const text = '@dev @qa';
    const blocks = parseBlocks(text);
    // 空区间：不降级
    expect(
      deactivateBlocksInRange(blocks, { start: 5, end: 5 }).map(b => b.type),
    ).toEqual(['mention', 'text', 'mention']);
    // 区间起点落在 mention 内部（@qa 起点是 5）：不降级
    expect(
      deactivateBlocksInRange(blocks, { start: 6, end: 8 }).map(b => b.type),
    ).toEqual(['mention', 'text', 'mention']);
    // 区间从 mention 起点开始：降级
    expect(
      deactivateBlocksInRange(blocks, { start: 5, end: 8 }).map(b => b.type),
    ).toEqual(['mention', 'text', 'text']);
  });

  it('空文本与纯文本不受影响', () => {
    expect(deactivateBlocksInRange(parseBlocks(''), { start: 0, end: 0 })).toEqual([]);
    expect(
      deactivateBlocksInRange(parseBlocks('普通文本'), { start: 0, end: 4 }).map(b => b.type),
    ).toEqual(['text']);
  });
});
