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
 *   枚举型候选（skill/mcp/agent/plugin/index）不受影响。
 */
export function selectDisplayGroups(groups: MentionGroup[], filter: string): MentionGroup[] {
  const hasFilter = filter.trim() !== '';
  return groups
    .filter(group => group.kind !== 'file' || hasFilter)
    .map(group => ({
      ...group,
      candidates: group.candidates.slice(0, MENTION_GROUP_DISPLAY_LIMIT),
    }));
}
