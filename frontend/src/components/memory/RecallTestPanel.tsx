//! Memory 召回测试组件

import { useState } from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Card, CardContent } from '@/components/ui/card';
import { Search, Loader2 } from 'lucide-react';
import type { RecallHit } from '@/api/tauri';
import { api } from '@/api/tauri';
import { useToast } from '@/components/Toast';

interface RecallTestPanelProps {
  onRecallComplete?: (hits: RecallHit[]) => void;
}

export function RecallTestPanel({ onRecallComplete }: RecallTestPanelProps) {
  const [recallQuery, setRecallQuery] = useState('');
  const [recallHits, setRecallHits] = useState<RecallHit[]>([]);
  const [isRecalling, setIsRecalling] = useState(false);
  const { showError } = useToast();

  const runRecall = async () => {
    const value = recallQuery.trim();
    if (!value) {
      setRecallHits([]);
      return;
    }
    setIsRecalling(true);
    try {
      const hits = await api.testMemoryRecall(value, 8);
      setRecallHits(hits);
      onRecallComplete?.(hits);
    } catch (error) {
      console.error('召回测试失败:', error);
      showError('召回失败', `无法执行 Memory 召回：${error}`);
    } finally {
      setIsRecalling(false);
    }
  };

  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <h4 className="text-sm font-medium">召回测试</h4>
        <div className="flex gap-2">
          <Input
            value={recallQuery}
            onChange={(event) => setRecallQuery(event.target.value)}
            placeholder="输入要测试的回忆问题"
            className="h-8 text-sm"
            onKeyDown={(e) => e.key === 'Enter' && runRecall()}
          />
          <Button size="icon" className="h-8 w-8" onClick={runRecall} disabled={isRecalling}>
            {isRecalling ? <Loader2 className="w-4 h-4 animate-spin" /> : <Search className="w-4 h-4" />}
          </Button>
        </div>
        <div className="space-y-2 max-h-48 overflow-y-auto">
          {recallHits.map((hit) => (
            <div key={hit.node_id} className="rounded-md border p-2.5">
              <div className="flex items-center justify-between gap-2 min-w-0">
                <span className="text-xs font-medium truncate">{hit.title}</span>
                <Badge variant={hit.score >= 0.8 ? 'default' : 'secondary'} className="shrink-0 text-[10px]">
                  {(hit.score * 100).toFixed(0)}%
                </Badge>
              </div>
              <div className="text-xs text-muted-foreground mt-1 line-clamp-2">
                {hit.summary}
              </div>
              <div className="mt-2 flex items-center gap-1 text-[10px] text-muted-foreground">
                <span>重要度 {hit.importance.toFixed(1)}</span>
                <span>·</span>
                <span>{hit.depth1_loaded ? '已展开' : '基础命中'}</span>
                <span>·</span>
                <span>{hit.kind}</span>
              </div>
            </div>
          ))}
          {!isRecalling && recallQuery.trim() && recallHits.length === 0 && (
            <div className="text-xs text-muted-foreground">暂无召回结果</div>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
