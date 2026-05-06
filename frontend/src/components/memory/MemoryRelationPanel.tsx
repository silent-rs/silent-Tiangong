//! Memory 关联管理组件

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Card, CardContent } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Link, Trash2 } from 'lucide-react';
import type { MemoryNode, MemoryRelation, MemoryRelationKind } from '@/api/tauri';
import { MEMORY_RELATION_OPTIONS, relationKindLabel } from './constants';

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
  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <h4 className="text-sm font-medium">记忆关联</h4>
        <div className="grid grid-cols-[minmax(0,1fr)_104px] gap-2">
          <Select value={relationTargetId} onValueChange={onTargetChange} disabled={!draftId}>
            <SelectTrigger className="h-8 text-sm">
              <SelectValue placeholder="选择关联目标" />
            </SelectTrigger>
            <SelectContent>
              {nodes.filter((node) => node.id !== draftId).map((node) => (
                <SelectItem key={node.id} value={node.id}>{node.title}</SelectItem>
              ))}
            </SelectContent>
          </Select>
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
