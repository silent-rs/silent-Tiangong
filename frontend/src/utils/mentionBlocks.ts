/**
 * @提及结构化块模型
 *
 * 把输入文本里的 @ 提及（@skill: / @mcp: / @plugin: / @index / @<role> / @all）
 * 切分为原子块，供输入框 contenteditable 编辑器与已发送消息气泡渲染成内联标签。
 *
 * 关键不变量（契约）：`serialize(parse(text)) === text`。
 * 后端从不解析 @ 提及、原文直送模型，所以编辑器底层模型任何改动都必须能
 * 序列化回与原来完全一致的字符串。
 *
 * 块边界规则（镜像 MessageInput 的 handleInputChange）：
 *   - `@` 仅在位于 index 0 或前置字符为空白时才视为提及起点；
 *   - 提及延伸到下一个空白或文末，无引号/转义。
 */

export type MentionKind = 'skill' | 'mcp' | 'agent' | 'all' | 'index' | 'plugin';

export type Block =
  | { type: 'text'; value: string }
  | { type: 'mention'; token: string; kind: MentionKind; label: string };

/** `@` 之后允许构成提及 token 的字符（不含空白）。 */
const TOKEN_CHAR = /[^\s]/;

/**
 * 判定一个已捕获的提及 token 的类型与展示标签。
 *
 * - `@skill:<id>`：`<id>` ∈ `[a-z0-9][a-z0-9_-]*`（后端 normalize_skill_id 产物）
 * - `@mcp:<name>`：取到下一空白，字符集不做强约束
 * - `@plugin:<id>`：能力型插件整体点名（清单 mention 声明的候选）
 * - `@index`：字面量（工作区搜索插件）
 * - `@all`：字面量
 * - `@<role>`：`@[A-Za-z0-9_]+`（后端 validate_role_identifier）
 *
 * 不符合上述语法的 token 视为普通文本（返回 null），不渲染成块。
 */
export function classifyMention(token: string): { kind: MentionKind; label: string } | null {
  // token 一定以 @ 开头
  if (!token.startsWith('@')) return null;

  if (token === '@all') return { kind: 'all', label: 'All' };

  if (token === '@index') return { kind: 'index', label: '工作区搜索' };

  const mSkill = /^@skill:([a-z0-9][a-z0-9_-]*)$/.exec(token);
  if (mSkill) return { kind: 'skill', label: mSkill[1] };

  const mMcp = /^@mcp:(\S+)$/.exec(token);
  if (mMcp) return { kind: 'mcp', label: mMcp[1] };

  const mPlugin = /^@plugin:([A-Za-z0-9][A-Za-z0-9_-]*)$/.exec(token);
  if (mPlugin) return { kind: 'plugin', label: mPlugin[1] };

  const mRole = /^@([A-Za-z0-9_]+)$/.exec(token);
  if (mRole) return { kind: 'agent', label: mRole[1] };

  return null;
}

/**
 * 将纯文本切分为 text / mention 块序列。
 *
 * 扫描每个 `@`：仅当它位于 index 0 或前置字符为空白时尝试捕获一个 token，
 * token 从 `@` 起延伸到下一个空白或文末；若 token 能被 {@link classifyMention}
 * 识别，则作为一个 mention 块，否则当作普通文本继续。
 */
export function parseBlocks(text: string): Block[] {
  const blocks: Block[] = [];
  let buffer = '';
  let i = 0;

  const flush = () => {
    if (buffer) {
      blocks.push({ type: 'text', value: buffer });
      buffer = '';
    }
  };

  while (i < text.length) {
    const ch = text[i];
    const atStart = i === 0 || /\s/.test(text[i - 1]);
    if (ch === '@' && atStart) {
      // 捕获从 i 到下一个空白/文末的 token
      let j = i + 1;
      while (j < text.length && TOKEN_CHAR.test(text[j])) j++;
      const token = text.slice(i, j);
      const cls = classifyMention(token);
      if (cls) {
        flush();
        blocks.push({ type: 'mention', token, kind: cls.kind, label: cls.label });
        i = j;
        continue;
      }
    }
    buffer += ch;
    i += 1;
  }
  flush();
  return blocks;
}

/** 将块序列拼回字符串。保证 `serialize(parse(text)) === text`。 */
export function serializeBlocks(blocks: Block[]): string {
  let out = '';
  for (const b of blocks) {
    out += b.type === 'text' ? b.value : b.token;
  }
  return out;
}

/** 是否包含至少一个 mention 块（用于气泡决定走块渲染或纯文本路径）。 */
export function hasMention(text: string): boolean {
  for (const b of parseBlocks(text)) {
    if (b.type === 'mention') return true;
  }
  return false;
}
