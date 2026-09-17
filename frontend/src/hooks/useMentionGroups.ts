import { useEffect, useRef, useState } from 'react';
import { api, type MentionRequest, type MentionTarget } from '@/api/tauri';

export type MentionGroup = {
  kind: string;
  label: string;
  candidates: { value: string; label: string; kind: string; hint: string; mark?: string }[];
};

const DEBOUNCE_MS = 120;

/**
 * 会话上下文 mention 查询：目标/查询词变化即重新请求，旧响应一律丢弃。
 * 关闭面板时保留上次结果供 chip 重建查表，不发起新请求。
 */
export function useMentionGroups(target: MentionTarget, query: string, active: boolean): MentionGroup[] {
  const [groups, setGroups] = useState<MentionGroup[]>([]);
  const requestRef = useRef(0);
  const targetRef = useRef(target);
  targetRef.current = target;

  useEffect(() => {
    if (!active) return;
    const requestId = ++requestRef.current;
    const timer = setTimeout(() => {
      const request: MentionRequest = { target, query, max_per_group: 1000 };
      api
        .getMentionGroups(undefined, undefined, request)
        .then((groups) => {
          // 仅接受仍对应当前目标的响应，防止切换会话后旧结果覆盖。
          if (requestRef.current === requestId && targetRef.current === target) {
            setGroups(groups);
          }
        })
        .catch((error) => console.error('加载提及候选失败:', error));
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // target 按值比较：会话切换/草稿工作区变化都会触发新查询。
  }, [target.kind, JSON.stringify(target), query, active]);

  return groups;
}
