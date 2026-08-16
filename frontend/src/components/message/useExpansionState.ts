import { useState, useCallback } from "react";

/**
 * 展开/收起状态：工具结果默认收起（调用完成后不自动展开），仅用户手动展开的进入 expanded。
 * 轮次完成（key 变化）触发组件重挂载，expanded 重置。
 */
export function useExpansionState() {
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const isExpanded = (id: string) => expanded.has(id);

  const toggle = useCallback((id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      next.has(id) ? next.delete(id) : next.add(id);
      return next;
    });
  }, []);

  return { isExpanded, toggle };
}

export type ExpansionState = ReturnType<typeof useExpansionState>;
