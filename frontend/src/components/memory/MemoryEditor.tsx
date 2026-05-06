//! Memory 编辑器组件

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { Label } from '@/components/ui/label';
import { Card, CardContent } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Plus, Save, Loader2 } from 'lucide-react';
import type { ManualMemoryDraft, MemoryCognitiveType } from '@/api/tauri';
import { MEMORY_TYPE_OPTIONS } from './constants';

interface MemoryEditorProps {
  draft: ManualMemoryDraft;
  keywordsText: string;
  isSaving: boolean;
  onDraftChange: (draft: ManualMemoryDraft) => void;
  onKeywordsChange: (text: string) => void;
  onSave: () => void;
  onNew: () => void;
}

export function MemoryEditor({
  draft,
  keywordsText,
  isSaving,
  onDraftChange,
  onKeywordsChange,
  onSave,
  onNew,
}: MemoryEditorProps) {
  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <div className="flex items-center justify-between">
          <h4 className="text-sm font-medium">手动附加 / 调整</h4>
          <Button variant="ghost" size="sm" onClick={onNew}>
            <Plus className="w-3.5 h-3.5 mr-1" />
            新增
          </Button>
        </div>
        <div>
          <Label className="text-xs">类型</Label>
          <Select
            value={draft.memory_type}
            onValueChange={(value) => onDraftChange({ ...draft, memory_type: value as MemoryCognitiveType })}
          >
            <SelectTrigger className="h-8 text-sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MEMORY_TYPE_OPTIONS.map((item) => (
                <SelectItem key={item.value} value={item.value}>{item.label}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div>
          <Label className="text-xs">标题</Label>
          <Input
            value={draft.title}
            onChange={(event) => onDraftChange({ ...draft, title: event.target.value })}
            className="h-8 text-sm"
            placeholder="记忆标题"
          />
        </div>
        <div>
          <Label className="text-xs">内容</Label>
          <Textarea
            value={draft.summary}
            onChange={(event) => onDraftChange({ ...draft, summary: event.target.value })}
            className="min-h-28 resize-y text-sm"
            placeholder="需要长期保存或修正的记忆内容"
          />
        </div>
        <div>
          <Label className="text-xs">关键词</Label>
          <Input
            value={keywordsText}
            onChange={(event) => onKeywordsChange(event.target.value)}
            className="h-8 text-sm"
            placeholder="用逗号分隔"
          />
        </div>
        <div>
          <Label className="text-xs">重要度</Label>
          <Input
            type="number"
            min={0}
            max={1}
            step={0.1}
            value={draft.importance}
            onChange={(event) => onDraftChange({ ...draft, importance: Number(event.target.value) || 0.6 })}
            className="h-8 text-sm"
          />
        </div>
        <Button onClick={onSave} disabled={isSaving} className="w-full">
          {isSaving ? <Loader2 className="w-4 h-4 mr-2 animate-spin" /> : <Save className="w-4 h-4 mr-2" />}
          保存记忆
        </Button>
      </CardContent>
    </Card>
  );
}
