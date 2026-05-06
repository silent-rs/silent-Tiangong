//! Memory 列表和批量操作组件

import { Archive, Edit2, RotateCcw } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import type { MemoryNode, MemoryStatus } from '@/api/tauri';
import { memoryTypeLabel } from './constants';

interface MemoryListProps {
  nodes: MemoryNode[];
  selectedId?: string;
  selectedIds: string[];
  status: MemoryStatus;
  isBulkBusy: boolean;
  onSelectNode: (node: MemoryNode) => void;
  onToggleSelection: (nodeId: string) => void;
  onToggleAll: () => void;
  onSetStatus: (node: MemoryNode, status: MemoryStatus) => void;
  onBulkStatus: (status: MemoryStatus) => void;
}

export function MemoryList({
  nodes,
  selectedId,
  selectedIds,
  status,
  isBulkBusy,
  onSelectNode,
  onToggleSelection,
  onToggleAll,
  onSetStatus,
  onBulkStatus,
}: MemoryListProps) {
  const selectedSet = new Set(selectedIds);
  const allSelected = nodes.length > 0 && nodes.every((node) => selectedSet.has(node.id));
  const nextStatus: MemoryStatus = status === 'active' ? 'archived' : 'active';

  return (
    <div className="h-full min-h-0 rounded-md border bg-background flex flex-col">
      <div className="flex items-center justify-between gap-3 border-b px-3 py-2">
        <div className="flex items-center gap-2 min-w-0">
          <input
            type="checkbox"
            checked={allSelected}
            onChange={onToggleAll}
            disabled={nodes.length === 0}
            className="size-4 rounded border-border"
            aria-label="全选记忆"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium">记忆列表</div>
            <div className="text-xs text-muted-foreground">
              已选 {selectedIds.length} / {nodes.length}
            </div>
          </div>
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={() => onBulkStatus(nextStatus)}
          disabled={selectedIds.length === 0 || isBulkBusy}
        >
          {status === 'active' ? (
            <Archive className="size-3.5 mr-1" />
          ) : (
            <RotateCcw className="size-3.5 mr-1" />
          )}
          {status === 'active' ? '批量归档' : '批量恢复'}
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto divide-y">
        {nodes.map((node) => {
          const checked = selectedSet.has(node.id);
          const active = node.id === selectedId;
          return (
            <div
              key={node.id}
              className={`flex items-start gap-2 px-3 py-2 ${active ? 'bg-muted/60' : ''}`}
            >
              <input
                type="checkbox"
                checked={checked}
                onChange={() => onToggleSelection(node.id)}
                className="mt-1 size-4 rounded border-border"
                aria-label={`选择 ${node.title}`}
              />
              <button
                type="button"
                className="min-w-0 flex-1 text-left"
                onClick={() => onSelectNode(node)}
              >
                <div className="flex items-center gap-2 min-w-0">
                  <div className="truncate text-sm font-medium">{node.title}</div>
                  <Badge variant="secondary" className="shrink-0 text-[10px]">
                    {memoryTypeLabel(node.memory_type)}
                  </Badge>
                </div>
                <div className="mt-1 text-xs text-muted-foreground line-clamp-2">
                  {node.summary}
                </div>
                {node.keywords.length > 0 && (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {node.keywords.slice(0, 4).map((keyword) => (
                      <span
                        key={keyword}
                        className="rounded border px-1.5 py-0.5 text-[10px] text-muted-foreground"
                      >
                        {keyword}
                      </span>
                    ))}
                  </div>
                )}
              </button>
              <div className="flex shrink-0 items-center gap-1">
                <Button variant="ghost" size="icon" className="size-7" onClick={() => onSelectNode(node)} title="编辑">
                  <Edit2 className="size-3.5" />
                </Button>
                {node.status === 'active' ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-7"
                    onClick={() => onSetStatus(node, 'archived')}
                    title="归档"
                  >
                    <Archive className="size-3.5" />
                  </Button>
                ) : (
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-7"
                    onClick={() => onSetStatus(node, 'active')}
                    title="恢复"
                  >
                    <RotateCcw className="size-3.5" />
                  </Button>
                )}
              </div>
            </div>
          );
        })}
        {nodes.length === 0 && (
          <div className="px-3 py-8 text-center text-sm text-muted-foreground">
            暂无匹配记忆
          </div>
        )}
      </div>
    </div>
  );
}
