import { useEffect, useRef, useState } from 'react';
import { api, type MentionRequest, type MentionTarget } from '@/api/tauri';
import { registerMentionMarks } from '@/utils/mentionMarks';

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
  useEffect(() => {
    if (!active) return;
    const requestId = ++requestRef.current;
    const timer = setTimeout(() => {
      const request: MentionRequest = { target, query, max_per_group: 1000 };
      api
        .getMentionGroups(undefined, undefined, request)
        .then((groups) => {
          // 只有序号匹配的最新请求才生效：足以丢弃切换目标/查询词后的迟到
          // 响应。不做 target 引用比较——父组件无关重渲染会生成新对象引用，
          // 误杀在途请求且不会补发。
          if (requestRef.current !== requestId) return;
          setGroups(groups);
          // 注册插件提供的标记字符：编辑器 chip 与消息气泡从 token 重建时查表。
          registerMentionMarks(groups.flatMap(group => group.candidates));
        })
        .catch((error) => console.error('加载提及候选失败:', error));
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // target 按值比较（序列化）：会话切换/草稿工作区变化都触发新查询。
  }, [JSON.stringify(target), query, active]);
  return groups;
}
