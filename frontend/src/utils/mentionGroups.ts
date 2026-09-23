/** @提及候选项（与 `getMention_groups` 的 wire 格式一致）。 */
export interface MentionCandidateView {
  value: string;
  label: string;
  kind: string;
  hint: string;
  mark?: string;
}

/** @提及候选分组：宿主按 kind 首次出现顺序聚合。 */
export interface MentionGroup {
  kind: string;
  label: string;
  candidates: MentionCandidateView[];
}

/** 每组最多展示的候选数：搜索已下推后端，这里只防大列表撑爆 UI。 */
export const MENTION_GROUP_DISPLAY_LIMIT = 50;

/**
 * 渲染层候选过滤：截断每组数量，空过滤词时剔除文件组。
 *
 * - 截断必须在键盘导航的平铺数组之前完成，保证索引与可见项对齐；
 * - 空过滤词时罗列全部文件既昂贵也无意义，等用户输入至少一个字符再检索，
 *   枚举型候选（skill/mcp/agent/plugin/index）不受影响；
 * - 例外：文件组只有「索引建立中」占位提示（没有任何真实候选）时保留——此时
 *   剔除它会让用户对着空面板以为工作区里没有可提及的文件，而真相是索引正在重建。
 */
export function selectDisplayGroups(groups: MentionGroup[], filter: string): MentionGroup[] {
  const hasFilter = filter.trim() !== '';
  return groups
    .filter(group => {
      if (hasFilter || group.kind !== 'file') return true;
      // 空过滤词 + 文件组：仅当整组都是占位提示（无真实候选可罗列）时才保留。
      const candidates = group.candidates;
      return candidates.length > 0 && candidates.every(isScanningPlaceholder);
    })
    .map(group => ({
      ...group,
      candidates: group.candidates.slice(0, MENTION_GROUP_DISPLAY_LIMIT),
    }));
}

/**
 * 是否「索引建立中」占位候选。
 *
 * 索引正在建立/重建时，sidecar 返回空候选 + `scanning: true`；mention 协议只允许
 * 返回候选数组，index 插件因此插入一条 value 为空的占位候选把状态透传出来。
 * 它只作提示行展示，不可选中、不参与键盘导航。
 */
export function isScanningPlaceholder(candidate: MentionCandidateView): boolean {
  return candidate.value === '';
}

/**
 * 头尾截断：保留开头与结尾，中间以一个省略号代替。
 *
 * 候选的说明文本关键信息常落在两端——开头是主体身份，结尾是补充说明；只保留
 * 开头会把收尾信息整段丢掉（Agent 职责、文件路径都如此）。单行展示不换行，
 * 全文由 title 的 hover 提示提供。
 *
 * 长度按字符数估算：中西文混排下宽度不等，但配合 `whitespace-nowrap` 与
 * `min-w-0` 由 flex 收敛，极端超长仍会被容器裁掉，不会撑破面板。
 */
export function truncateMiddle(text: string, maxChars = 40): string {
  if (maxChars < 3 || text.length <= maxChars) {
    return text;
  }
  // 头部占六成：主体信息优先级高于结尾补充。
  const headLength = Math.ceil((maxChars - 1) * 0.6);
  const tailLength = maxChars - 1 - headLength;
  return `${text.slice(0, headLength)}…${text.slice(text.length - tailLength)}`;
}

/**
 * 可选中候选：剔除「索引建立中」占位提示。
 *
 * 键盘导航与选中都基于该数组，保证 Enter / 方向键不会落到不可选中的提示行上。
 */
export function selectableCandidates(
  candidates: MentionCandidateView[],
): MentionCandidateView[] {
  return candidates.filter(candidate => !isScanningPlaceholder(candidate));
}
