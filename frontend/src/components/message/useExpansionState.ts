import { useState, useCallback } from "react";

/**
 * 展开/收起状态：活跃态默认全展开，用户仍可手动收起（记录到 userCollapsed 覆盖默认）；
 * 非活跃态默认全收起，用户手动展开才进入 expanded。
 * 轮次完成（key 变化）触发组件重挂载，两个集合都会重置。
 */
export function useExpansionState(isActive: boolean) {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [userCollapsed, setUserCollapsed] = useState<Set<string>>(new Set());

  const isExpanded = (id: string) =>
    isActive ? !userCollapsed.has(id) : expanded.has(id);

  const toggle = useCallback((id: string) => {
    if (isActive) {
      setUserCollapsed((prev) => {
        const next = new Set(prev);
        next.has(id) ? next.delete(id) : next.add(id);
        return next;
      });
    } else {
      setExpanded((prev) => {
        const next = new Set(prev);
        next.has(id) ? next.delete(id) : next.add(id);
        return next;
      });
    }
  }, [isActive]);

  return { isExpanded, toggle };
}

export type ExpansionState = ReturnType<typeof useExpansionState>;
