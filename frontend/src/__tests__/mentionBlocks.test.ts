import { describe, expect, it } from 'vitest';

import {
  classifyMention,
  hasMention,
  parseBlocks,
  serializeBlocks,
} from '@/utils/mentionBlocks';

describe('classifyMention', () => {
  it('识别 @skill:<id>', () => {
    expect(classifyMention('@skill:my-skill')).toEqual({ kind: 'skill', label: 'my-skill' });
    expect(classifyMention('@skill:a1_b-2')).toEqual({ kind: 'skill', label: 'a1_b-2' });
  });

  it('识别 @mcp:<name>', () => {
    expect(classifyMention('@mcp:github')).toEqual({ kind: 'mcp', label: 'github' });
    // mcp 名字字符集不做强约束，只要有内容即可
    expect(classifyMention('@mcp:my.server-1')).toEqual({ kind: 'mcp', label: 'my.server-1' });
  });

  it('识别 @all', () => {
    expect(classifyMention('@all')).toEqual({ kind: 'all', label: 'All' });
  });

  it('识别 @<role>', () => {
    expect(classifyMention('@dev')).toEqual({ kind: 'agent', label: 'dev' });
    expect(classifyMention('@Reviewer_1')).toEqual({ kind: 'agent', label: 'Reviewer_1' });
  });

  it('识别 @index 与 @plugin:<id>', () => {
    expect(classifyMention('@index')).toEqual({ kind: 'index', label: '工作区搜索' });
    expect(classifyMention('@plugin:text-to-speech')).toEqual({
      kind: 'plugin',
      label: 'text-to-speech',
    });
  });

  it('拒绝不合法 token', () => {
    expect(classifyMention('@')).toBeNull();
    expect(classifyMention('hello')).toBeNull();
    expect(classifyMention('@skill:')).toBeNull();
    expect(classifyMention('@mcp:')).toBeNull();
    expect(classifyMention('@all-dev')).toBeNull();
    // role 不允许非字母数字下划线
    expect(classifyMention('@dev-bot!')).toBeNull();
    // plugin id 不允许点号等字符（带点的会被当成普通文本）
    expect(classifyMention('@plugin:a.b')).toBeNull();
    expect(classifyMention('@plugin:')).toBeNull();
  });
});

describe('parseBlocks', () => {
  it('纯文本无提及返回单文本块', () => {
    expect(parseBlocks('hello world')).toEqual([{ type: 'text', value: 'hello world' }]);
  });

  it('切分前后文本与提及', () => {
    expect(parseBlocks('用 @skill:my-skill 重构')).toEqual([
      { type: 'text', value: '用 ' },
      { type: 'mention', token: '@skill:my-skill', kind: 'skill', label: 'my-skill' },
      { type: 'text', value: ' 重构' },
    ]);
  });

  it('开头与结尾的提及', () => {
    expect(parseBlocks('@all 广播')).toEqual([
      { type: 'mention', token: '@all', kind: 'all', label: 'All' },
      { type: 'text', value: ' 广播' },
    ]);
    expect(parseBlocks('广播给 @dev')).toEqual([
      { type: 'text', value: '广播给 ' },
      { type: 'mention', token: '@dev', kind: 'agent', label: 'dev' },
    ]);
  });

  it('多个连续提及', () => {
    expect(parseBlocks('@skill:a @mcp:b @all')).toEqual([
      { type: 'mention', token: '@skill:a', kind: 'skill', label: 'a' },
      { type: 'text', value: ' ' },
      { type: 'mention', token: '@mcp:b', kind: 'mcp', label: 'b' },
      { type: 'text', value: ' ' },
      { type: 'mention', token: '@all', kind: 'all', label: 'All' },
    ]);
  });

  it('邮箱里的 @ 不误判（前置非空白）', () => {
    expect(parseBlocks('联系 me@host.com')).toEqual([
      { type: 'text', value: '联系 me@host.com' },
    ]);
    expect(hasMention('联系 me@host.com')).toBe(false);
  });

  it('多行文本里仍按空白切分', () => {
    expect(parseBlocks('line1\n@skill:x\nline3')).toEqual([
      { type: 'text', value: 'line1\n' },
      { type: 'mention', token: '@skill:x', kind: 'skill', label: 'x' },
      { type: 'text', value: '\nline3' },
    ]);
  });

  it('不合法的 @ 当作普通文本', () => {
    expect(parseBlocks('@ 不带名字')).toEqual([{ type: 'text', value: '@ 不带名字' }]);
  });

  it('空串与纯提及', () => {
    expect(parseBlocks('')).toEqual([]);
    expect(parseBlocks('@all')).toEqual([
      { type: 'mention', token: '@all', kind: 'all', label: 'All' },
    ]);
  });
});

describe('serializeBlocks 往返不变量', () => {
  const cases = [
    '',
    '纯文本',
    '用 @skill:my-skill 重构',
    '@all 广播给 @dev',
    '@skill:a @mcp:b @all @qa',
    '联系 me@host.com 别误判',
    'line1\n@skill:x\nline3 @mcp:server-1 收尾',
    '@skill:只字面量',
    '中@间也算文本',
    '@all',
  ];

  for (const text of cases) {
    it(`serialize(parse(${JSON.stringify(text)})) === 原文`, () => {
      expect(serializeBlocks(parseBlocks(text))).toBe(text);
    });
  }
});

describe('@file 文件提及', () => {
  it('识别不含空白的相对路径，label 取文件名', () => {
    expect(classifyMention('@file:src/core/session.rs')).toEqual({
      kind: 'file',
      label: 'session.rs',
    });
    expect(classifyMention('@file:README.md')).toEqual({ kind: 'file', label: 'README.md' });
  });

  it('反引号包裹的路径可含空格与中文', () => {
    expect(classifyMention('@file:`设计 文档/需求 说明.md`')).toEqual({
      kind: 'file',
      label: '需求 说明.md',
    });
  });

  it('空路径不构成提及', () => {
    expect(classifyMention('@file:')).toBeNull();
    expect(classifyMention('@file:``')).toBeNull();
  });

  it('含空格的文件提及在解析后可原样序列化回去', () => {
    const text = '请看 @file:`设计 文档/需求 说明.md` 这份文档';
    const blocks = parseBlocks(text);
    // 契约：serialize(parse(text)) === text，后端原文直送模型。
    expect(serializeBlocks(blocks)).toBe(text);
    const mention = blocks.find((block) => block.type === 'mention');
    expect(mention).toMatchObject({
      kind: 'file',
      token: '@file:`设计 文档/需求 说明.md`',
      label: '需求 说明.md',
    });
  });

  it('不含空格的文件提及同样可往返', () => {
    const text = '改一下 @file:src/main.rs 谢谢';
    expect(serializeBlocks(parseBlocks(text))).toBe(text);
    expect(hasMention(text)).toBe(true);
  });

  it('多个文件提及与普通文本混排互不干扰', () => {
    const text = '@file:a.rs 和 @file:`b 2.rs` 都要改';
    const blocks = parseBlocks(text);
    expect(serializeBlocks(blocks)).toBe(text);
    const tokens = blocks
      .filter((block) => block.type === 'mention')
      .map((block) => (block.type === 'mention' ? block.token : ''));
    expect(tokens).toEqual(['@file:a.rs', '@file:`b 2.rs`']);
  });

  it('未配对的反引号不吞掉后续文本', () => {
    const text = '@file:`未闭合 后面还有字';
    // 未配对时按普通文本处理，序列化仍需完全一致。
    expect(serializeBlocks(parseBlocks(text))).toBe(text);
  });
});
