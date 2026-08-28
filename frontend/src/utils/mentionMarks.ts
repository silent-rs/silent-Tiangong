/**
 * @提及标记（chip 角标字符）注册表。
 *
 * 标记完全由数据提供方注册，前端不做按 kind 的硬编码默认：
 * - 插件候选：动态候选的 `mark` 字段（wasm `__tiangong.mention_candidates.v1`）
 *   或清单 `mention.mark`（宿主生成 `@plugin:<id>` 静态候选），随候选加载注册；
 * - 前端本地候选：活跃 Agent / @all 由输入框在生成候选时注册。
 *
 * 消息气泡与输入框编辑器从 token 重建 chip 时手里只有文本，经本表还原标记。
 * 查找顺序：token 精确匹配 → kind 兜底（首个提供该 kind 标记的值）→
 * kind 首字母大写（未知 kind 的通用规则）。
 */

const markByToken = new Map<string, string>();
const markByKind = new Map<string, string>();

/** 注册一个候选的标记（mark 为空时不注册，保留既有兜底）。 */
export function registerMentionMark(value: string, kind: string, mark?: string) {
  const trimmed = mark?.trim();
  if (!trimmed) return;
  markByToken.set(value, trimmed);
  if (!markByKind.has(kind)) markByKind.set(kind, trimmed);
}

/** 批量注册（加载候选分组后调用）。 */
export function registerMentionMarks(
  candidates: { value: string; kind: string; mark?: string }[],
) {
  for (const c of candidates) registerMentionMark(c.value, c.kind, c.mark);
}

/** 取某提及的标记字符：token 精确匹配 → kind 兜底 → kind 首字母。 */
export function mentionMarkFor(kind: string, token?: string): string {
  if (token) {
    const mark = markByToken.get(token);
    if (mark) return mark;
  }
  const kindMark = markByKind.get(kind);
  if (kindMark) return kindMark;
  return kind.slice(0, 1).toUpperCase() || '·';
}
