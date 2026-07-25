import { useState, useEffect, useCallback } from 'react';
import { Button } from '@/components/ui/button';
import { RefreshCw, Trash2, RotateCw, Database, FolderOpen, Loader2 } from 'lucide-react';
import { api } from '@/api/tauri';
import type { WorkspaceIndexInfo } from '@/api/tauri';
import { useToast } from '@/components/Toast';

export function IndexManagementSettings() {
  const [indexes, setIndexes] = useState<WorkspaceIndexInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [rebuilding, setRebuilding] = useState<string | null>(null);
  const { showSuccess, showError } = useToast();

  const loadIndexes = useCallback(async () => {
    setLoading(true);
    try {
      const list = await api.listWorkspaceIndexes();
      setIndexes(list);
    } catch (err) {
      showError('加载索引列表失败', String(err));
    } finally {
      setLoading(false);
    }
  }, [showError]);

  useEffect(() => {
    loadIndexes();
  }, [loadIndexes]);

  const handleDelete = async (id: string, root: string) => {
    setDeleting(id);
    try {
      await api.deleteWorkspaceIndex(id, root);
      showSuccess('索引已删除');
      await loadIndexes();
    } catch (err) {
      showError('删除索引失败', String(err));
    } finally {
      setDeleting(null);
    }
  };

  const handleRebuild = async (root: string, id: string) => {
    setRebuilding(id);
    try {
      const count = await api.rebuildWorkspaceIndex(root);
      showSuccess('索引重建完成', `共 ${count} 个文件`);
      await loadIndexes();
    } catch (err) {
      showError('重建索引失败', String(err));
    } finally {
      setRebuilding(null);
    }
  };

  const formatTime = (t: string) => {
    if (!t) return '-';
    return t.replace('T', ' ').substring(0, 19);
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-6 py-4 border-b">
        <div>
          <h3 className="text-lg font-medium">索引管理</h3>
          <p className="text-sm text-muted-foreground mt-0.5">
            管理工作区文件索引。Session 索引随会话生命周期自动管理。
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={loadIndexes} disabled={loading}>
          {loading ? <Loader2 className="w-4 h-4 mr-1 animate-spin" /> : <RefreshCw className="w-4 h-4 mr-1" />}
          刷新
        </Button>
      </div>

      <div className="flex-1 overflow-y-auto px-6 py-4">
        {loading && indexes.length === 0 ? (
          <div className="flex items-center justify-center py-12 text-muted-foreground">
            <Loader2 className="w-5 h-5 mr-2 animate-spin" />
            加载中...
          </div>
        ) : indexes.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-12 text-muted-foreground">
            <Database className="w-10 h-10 mb-3 opacity-40" />
            <p>暂无工作区索引</p>
            <p className="text-xs mt-1">切换工作目录后自动创建索引</p>
          </div>
        ) : (
          <div className="space-y-3">
            {indexes.map((idx) => (
              <div
                key={idx.id}
                className="border rounded-lg p-4 flex items-start gap-4"
              >
                <FolderOpen className="w-5 h-5 mt-0.5 text-muted-foreground shrink-0" />
                <div className="flex-1 min-w-0">
                  <p className="font-medium text-sm truncate" title={idx.root || idx.id}>
                    {idx.root || `未知来源 (${idx.id.slice(0, 12)}…)`}
                  </p>
                  <div className="flex items-center gap-4 mt-1 text-xs text-muted-foreground">
                    <span>{idx.entry_count} 个文件</span>
                    {idx.updated_at ? (
                      <span>更新于 {formatTime(idx.updated_at)}</span>
                    ) : (
                      <span className="text-yellow-500">未记录时间</span>
                    )}
                  </div>
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  {idx.root && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => handleRebuild(idx.root, idx.id)}
                      disabled={rebuilding === idx.id}
                    >
                      {rebuilding === idx.id ? (
                        <Loader2 className="w-4 h-4 animate-spin" />
                      ) : (
                        <RotateCw className="w-4 h-4" />
                      )}
                    </Button>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => handleDelete(idx.id, idx.root)}
                    disabled={deleting === idx.id}
                    className="text-destructive hover:text-destructive"
                  >
                    {deleting === idx.id ? (
                      <Loader2 className="w-4 h-4 animate-spin" />
                    ) : (
                      <Trash2 className="w-4 h-4" />
                    )}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
