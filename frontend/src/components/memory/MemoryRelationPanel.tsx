//! Memory 关联管理组件

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Badge } from '@/components/ui/badge';
import { Card, CardContent } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Link, Trash2, ChevronDown } from 'lucide-react';
import { cn } from '@/lib/utils';
import type { MemoryNode, MemoryRelation, MemoryRelationKind } from '@/api/tauri';
import { MEMORY_RELATION_OPTIONS, memoryTypeLabel, relationKindLabel } from './constants';

interface MemoryRelationPanelProps {
  draftId: string | undefined;
  nodes: MemoryNode[];
  relations: MemoryRelation[];
  relationTargetId: string;
  relationKind: MemoryRelationKind;
  relationNote: string;
  onTargetChange: (id: string) => void;
  onKindChange: (kind: MemoryRelationKind) => void;
  onNoteChange: (note: string) => void;
  onSave: () => void;
  onRemove: (relation: MemoryRelation) => void;
}

export function MemoryRelationPanel({
  draftId,
  nodes,
  relations,
  relationTargetId,
  relationKind,
  relationNote,
  onTargetChange,
  onKindChange,
  onNoteChange,
  onSave,
  onRemove,
}: MemoryRelationPanelProps) {
  const [comboOpen, setComboOpen] = useState(false);
  const [highlightIndex, setHighlightIndex] = useState(0);
  const [comboQuery, setComboQuery] = useState('');
  const [panelStyle, setPanelStyle] = useState<React.CSSProperties>({});
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const inputRef = useRef<HTMLInputElement>(null);

  const candidateNodes = useMemo(() => nodes.filter((node) => node.id !== draftId), [draftId, nodes]);
  const normalizedQuery = comboQuery.trim().toLowerCase();
  const filteredCandidateNodes = useMemo(() => {
    if (!normalizedQuery) {
      return candidateNodes;
    }
    return candidateNodes.filter((node) => {
      const searchable = [
        node.title,
        node.summary,
        node.memory_type,
        ...node.keywords,
      ].join(' ').toLowerCase();
      return searchable.includes(normalizedQuery);
    });
  }, [candidateNodes, normalizedQuery]);

  const selectedTargetNode = useMemo(
    () => candidateNodes.find((node) => node.id === relationTargetId),
    [candidateNodes, relationTargetId],
  );

  // 面板打开时计算位置并聚焦搜索框
  useEffect(() => {
    if (!comboOpen) return;
    const trigger = triggerRef.current;
    if (trigger) {
      const rect = trigger.getBoundingClientRect();
      const panelWidth = 480;
      const viewportWidth = window.innerWidth;
      const right = viewportWidth - rect.right;
      const top = Math.max(8, rect.top - 296);
      setPanelStyle({
        position: 'fixed',
        top: `${top}px`,
        right: `${right}px`,
        width: `${Math.min(panelWidth, viewportWidth - 16)}px`,
        maxHeight: `${rect.top - 12}px`,
      });
    }
    inputRef.current?.focus();
  }, [comboOpen]);

  // 点击外部关闭面板
  useEffect(() => {
    if (!comboOpen) return;
    const handleClickOutside = (event: MouseEvent) => {
      const target = event.target as Node;
      if (
        triggerRef.current?.contains(target) ||
        panelRef.current?.contains(target)
      ) {
        return;
      }
      setComboOpen(false);
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, [comboOpen]);

  // 过滤结果变化时重置高亮索引
  useEffect(() => {
    setHighlightIndex(0);
  }, [filteredCandidateNodes.length]);

  // 高亮项滚动到可见区域
  useEffect(() => {
    if (comboOpen && filteredCandidateNodes.length > 0) {
      itemRefs.current[highlightIndex]?.scrollIntoView({ block: 'nearest' });
    }
  }, [highlightIndex, comboOpen, filteredCandidateNodes.length]);

  const selectCandidate = useCallback((nodeId: string) => {
    onTargetChange(nodeId);
    setComboOpen(false);
    setComboQuery('');
  }, [onTargetChange]);

  const handleComboKeyDown = useCallback((e: React.KeyboardEvent<HTMLInputElement>) => {
    if (!comboOpen) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHighlightIndex((i) => (i + 1) % filteredCandidateNodes.length);
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHighlightIndex((i) => (i - 1 + filteredCandidateNodes.length) % filteredCandidateNodes.length);
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const node = filteredCandidateNodes[highlightIndex];
      if (node) {
        selectCandidate(node.id);
      }
      return;
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      setComboOpen(false);
    }
  }, [comboOpen, filteredCandidateNodes, highlightIndex, selectCandidate]);

  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <h4 className="text-sm font-medium">记忆关联</h4>
        <div className="grid grid-cols-[minmax(0,1fr)_104px] gap-2 items-start">
          <div className="min-w-0">
            <button
              type="button"
              ref={triggerRef}
              className={cn(
                'flex h-8 w-full items-center justify-between rounded-md border border-input bg-background px-3 text-sm',
                'ring-offset-background hover:bg-accent hover:text-accent-foreground',
                'focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2',
                'disabled:cursor-not-allowed disabled:opacity-50',
              )}
              onClick={() => {
                setComboOpen(!comboOpen);
                setComboQuery('');
                setHighlightIndex(0);
              }}
              disabled={!draftId}
            >
              {selectedTargetNode ? (
                <span className="truncate">{selectedTargetNode.title}</span>
              ) : (
                <span className="text-muted-foreground">选择关联目标...</span>
              )}
              <ChevronDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
            </button>

            {comboOpen && draftId && createPortal(
              <div
                ref={panelRef}
                className="fixed z-[9999] rounded-md border bg-popover shadow-lg flex flex-col"
                style={panelStyle}
              >
                <div className="p-2 border-b shrink-0">
                  <Input
                    ref={inputRef}
                    value={comboQuery}
                    onChange={(event) => setComboQuery(event.target.value)}
                    onKeyDown={handleComboKeyDown}
                    className="h-7 text-xs"
                    placeholder="搜索标题、内容或关键词"
                  />
                </div>
                <div className="overflow-y-auto p-1 flex-1">
                  {filteredCandidateNodes.map((node, i) => (
                    <button
                      key={node.id}
                      type="button"
                      ref={(el) => { itemRefs.current[i] = el; }}
                      className={cn(
                        'flex w-full items-start gap-2 rounded-sm px-2 py-1.5 text-left text-xs transition-colors',
                        'hover:bg-accent',
                        (highlightIndex === i || relationTargetId === node.id) && 'bg-accent text-accent-foreground',
                      )}
                      onClick={() => selectCandidate(node.id)}
                      onMouseEnter={() => setHighlightIndex(i)}
                    >
                      <Badge variant="secondary" className="shrink-0 text-[10px] leading-tight px-1.5 py-0">
                        {memoryTypeLabel(node.memory_type)}
                      </Badge>
                      <div className="min-w-0 flex-1 overflow-hidden">
                        <div className="truncate font-medium">{node.title}</div>
                        {node.summary && (
                          <div className="mt-0.5 truncate text-muted-foreground">
                            {node.summary}
                          </div>
                        )}
                      </div>
                    </button>
                  ))}
                  {filteredCandidateNodes.length === 0 && (
                    <div className="px-2 py-3 text-xs text-muted-foreground text-center">
                      没有匹配目标
                    </div>
                  )}
                </div>
              </div>,
              document.body,
            )}
          </div>
          <Select value={relationKind} onValueChange={(value) => onKindChange(value as MemoryRelationKind)} disabled={!draftId}>
            <SelectTrigger className="h-8 text-sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MEMORY_RELATION_OPTIONS.map((item) => (
                <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Input
          value={relationNote}
          onChange={(event) => onNoteChange(event.target.value)}
          className="h-8 text-sm"
          placeholder="关联备注"
          disabled={!draftId}
        />
        <Button variant="outline" onClick={onSave} disabled={!draftId} className="w-full">
          <Link className="w-4 h-4 mr-2" />
          保存关联
        </Button>
        <div className="space-y-2 max-h-40 overflow-y-auto">
          {relations.map((relation) => {
            const otherId = relation.from_node_id === draftId ? relation.to_node_id : relation.from_node_id;
            const otherNode = nodes.find((node) => node.id === otherId);
            return (
              <div key={relation.id} className="rounded-md border p-2 flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="text-xs font-medium truncate">
                    {relationKindLabel(relation.relation_kind)}：{otherNode?.title ?? otherId}
                  </div>
                  {relation.note && (
                    <div className="text-xs text-muted-foreground mt-1 line-clamp-2">{relation.note}</div>
                  )}
                </div>
                <Button variant="ghost" size="icon" className="h-7 w-7 shrink-0" onClick={() => onRemove(relation)} title="删除关联">
                  <Trash2 className="w-3.5 h-3.5" />
                </Button>
              </div>
            );
          })}
          {draftId && relations.length === 0 && (
            <div className="text-xs text-muted-foreground">暂无关联</div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
