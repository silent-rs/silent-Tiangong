//! Memory 管理设置组件（主入口）

import { useState, useEffect, useCallback } from 'react';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { RefreshCw, Archive, RotateCcw, Edit2, Database, Activity, TrendingUp, Target, Loader2, Network, List } from 'lucide-react';
import { api } from '@/api/tauri';
import type { MemoryNode, MemoryRelation, MemoryStatus, ManualMemoryDraft, MemoryRelationKind } from '@/api/tauri';
import { useToast } from '@/components/Toast';
import { MemoryGraphCanvas } from './MemoryGraph';
import { MemoryList } from './MemoryList';
import { MemoryEditor } from './MemoryEditor';
import { MemoryRelationPanel } from './MemoryRelationPanel';
import { RecallTestPanel } from './RecallTestPanel';
import { emptyMemoryDraft, relationKindLabel } from './constants';

const MEMORY_LIST_PAGE_SIZE = 50;
const MEMORY_GRAPH_INITIAL_LIMIT = 240;
const MEMORY_GRAPH_LIMIT_STEP = 120;
const MEMORY_GRAPH_MAX_LIMIT = 500;

function formatLocalDateTime(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, '0');
  return [
    date.getFullYear(),
    pad(date.getMonth() + 1),
    pad(date.getDate()),
  ].join('-') + ` ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ============================================================================
// 统计卡片组件
// ============================================================================

interface MemoryStatsProps {
  nodes: MemoryNode[];
  relations: MemoryRelation[];
  totalNodeCount: number;
  activeNodeCount: number;
  weekNewCount: number;
}

function MemoryStats({ nodes, relations, totalNodeCount, activeNodeCount, weekNewCount }: MemoryStatsProps) {
  return (
    <div className="grid grid-cols-4 gap-2 shrink-0">
      <div className="rounded-md border p-3 text-center">
        <div className="flex items-center justify-center gap-1 text-muted-foreground mb-1">
          <Database className="w-3 h-3" />
          <span className="text-xs">总计</span>
        </div>
        <div className="text-lg font-semibold">{totalNodeCount}</div>
        {totalNodeCount > nodes.length ? (
          <div className="text-[10px] text-muted-foreground">已加载 {nodes.length}</div>
        ) : null}
      </div>
      <div className="rounded-md border p-3 text-center">
        <div className="flex items-center justify-center gap-1 text-muted-foreground mb-1">
          <Activity className="w-3 h-3" />
          <span className="text-xs">活跃</span>
        </div>
        <div className="text-lg font-semibold text-green-600">{activeNodeCount}</div>
      </div>
      <div className="rounded-md border p-3 text-center">
        <div className="flex items-center justify-center gap-1 text-muted-foreground mb-1">
          <TrendingUp className="w-3 h-3" />
          <span className="text-xs">本周新增</span>
        </div>
        <div className="text-lg font-semibold text-blue-600">{weekNewCount}</div>
      </div>
      <div className="rounded-md border p-3 text-center">
        <div className="flex items-center justify-center gap-1 text-muted-foreground mb-1">
          <Target className="w-3 h-3" />
          <span className="text-xs">关联数</span>
        </div>
        <div className="text-lg font-semibold">{relations.length}</div>
      </div>
    </div>
  );
}

// ============================================================================
// 主组件
// ============================================================================

type MemoryViewMode = 'graph' | 'list';

export function MemoryManagementSettings() {
  const [nodes, setNodes] = useState<MemoryNode[]>([]);
  const [query, setQuery] = useState('');
  const [status, setStatus] = useState<MemoryStatus>('active');
  const [viewMode, setViewMode] = useState<MemoryViewMode>('graph');
  const [listPage, setListPage] = useState(1);
  const [graphLimit, setGraphLimit] = useState(MEMORY_GRAPH_INITIAL_LIMIT);
  const [isLoading, setIsLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isBulkBusy, setIsBulkBusy] = useState(false);
  const [selectedNodeIds, setSelectedNodeIds] = useState<string[]>([]);
  const [draft, setDraft] = useState<ManualMemoryDraft>(emptyMemoryDraft());
  const [keywordsText, setKeywordsText] = useState('');
  const [relations, setRelations] = useState<MemoryRelation[]>([]);
  const [graphRelations, setGraphRelations] = useState<MemoryRelation[]>([]);
  const [totalNodeCount, setTotalNodeCount] = useState(0);
  const [activeNodeCount, setActiveNodeCount] = useState(0);
  const [weekNewCount, setWeekNewCount] = useState(0);
  const [relationTargetId, setRelationTargetId] = useState('');
  const [relationKind, setRelationKind] = useState<MemoryRelationKind>('related_to');
  const [relationNote, setRelationNote] = useState('');
  const { showSuccess, showError } = useToast();

  // 使用批量 API 加载图谱关联（修复 N+1 性能问题）
  const loadGraphRelations = useCallback(async (items: MemoryNode[]) => {
    if (items.length === 0) {
      setGraphRelations([]);
      return;
    }
    try {
      const allRelations = await api.listMemoryRelationsBatch(items.map(n => n.id));
      setGraphRelations(allRelations);
    } catch (error) {
      console.error('批量加载图谱关联失败:', error);
      // 降级到逐个加载
      const relationGroups = await Promise.all(items.map((node) => api.listMemoryRelations(node.id)));
      const relationMap = new Map<string, MemoryRelation>();
      relationGroups.flat().forEach((relation) => {
        relationMap.set(relation.id, relation);
      });
      setGraphRelations(Array.from(relationMap.values()));
    }
  }, []);

  const loadNodes = useCallback(async () => {
    setIsLoading(true);
    try {
      const normalizedQuery = query.trim() || undefined;
      const isListView = viewMode === 'list';
      const pageSize = isListView ? MEMORY_LIST_PAGE_SIZE : graphLimit;
      const offset = isListView ? (listPage - 1) * MEMORY_LIST_PAGE_SIZE : 0;
      const weekAgo = new Date();
      weekAgo.setDate(weekAgo.getDate() - 7);
      const createdAfter = formatLocalDateTime(weekAgo);
      const [data, total, activeTotal, weekTotal] = await Promise.all([
        api.listMemoryNodes(normalizedQuery, status, pageSize, offset),
        api.countMemoryNodes(normalizedQuery, status),
        api.countMemoryNodes(normalizedQuery, 'active'),
        api.countMemoryNodes(normalizedQuery, status, createdAfter),
      ]);
      setNodes(data);
      setTotalNodeCount(total);
      setActiveNodeCount(activeTotal);
      setWeekNewCount(weekTotal);
      setSelectedNodeIds((current) => current.filter((id) => data.some((node) => node.id === id)));
      await loadGraphRelations(data);
    } catch (error) {
      console.error('加载记忆失败:', error);
      showError('加载失败', `无法加载 Memory 记忆：${error}`);
    } finally {
      setIsLoading(false);
    }
  }, [graphLimit, listPage, loadGraphRelations, query, status, showError, viewMode]);

  useEffect(() => {
    loadNodes();
  }, [loadNodes]);

  useEffect(() => {
    setListPage(1);
    setGraphLimit(MEMORY_GRAPH_INITIAL_LIMIT);
    setSelectedNodeIds([]);
  }, [query, status]);

  const startNew = useCallback(() => {
    setDraft(emptyMemoryDraft());
    setKeywordsText('');
    setRelations([]);
    setRelationTargetId('');
    setRelationKind('related_to');
    setRelationNote('');
  }, []);

  const editNode = useCallback((node: MemoryNode) => {
    setDraft({
      id: node.id,
      memory_type: node.memory_type,
      title: node.title,
      summary: node.summary,
      keywords: node.keywords,
      importance: node.importance,
    });
    setKeywordsText(node.keywords.join(', '));
    setRelationTargetId('');
    setRelationNote('');
    api.listMemoryRelations(node.id)
      .then(setRelations)
      .catch((error) => {
        console.error('加载记忆关系失败:', error);
        setRelations([]);
      });
  }, []);

  const saveDraft = async () => {
    const title = draft.title.trim();
    const summary = draft.summary.trim();
    if (!title || !summary) {
      showError('内容不完整', '标题和内容都不能为空');
      return;
    }
    setIsSaving(true);
    try {
      const saved = await api.upsertManualMemory({
        ...draft,
        title,
        summary,
        keywords: keywordsText
          .split(',')
          .map((item) => item.trim())
          .filter(Boolean),
        importance: Number(draft.importance) || 0.6,
      });
      editNode(saved);
      showSuccess('记忆已保存', saved.title);
      await loadNodes();
    } catch (error) {
      console.error('保存记忆失败:', error);
      showError('保存失败', `无法保存记忆：${error}`);
    } finally {
      setIsSaving(false);
    }
  };

  const setNodeStatus = async (node: MemoryNode, nextStatus: MemoryStatus) => {
    try {
      await api.setMemoryNodeStatus(node.id, nextStatus);
      showSuccess(nextStatus === 'archived' ? '记忆已归档' : '记忆已恢复', node.title);
      await loadNodes();
    } catch (error) {
      console.error('更新记忆状态失败:', error);
      showError('操作失败', `无法更新记忆状态：${error}`);
    }
  };

  const toggleNodeSelection = (nodeId: string) => {
    setSelectedNodeIds((current) =>
      current.includes(nodeId)
        ? current.filter((id) => id !== nodeId)
        : [...current, nodeId]
    );
  };

  const toggleAllNodes = () => {
    setSelectedNodeIds((current) => {
      const allSelected = nodes.length > 0 && nodes.every((node) => current.includes(node.id));
      return allSelected ? [] : nodes.map((node) => node.id);
    });
  };

  const setSelectedNodesStatus = async (nextStatus: MemoryStatus) => {
    if (selectedNodeIds.length === 0) {
      return;
    }
    setIsBulkBusy(true);
    try {
      await Promise.all(selectedNodeIds.map((nodeId) => api.setMemoryNodeStatus(nodeId, nextStatus)));
      showSuccess(nextStatus === 'archived' ? '已批量归档' : '已批量恢复', `${selectedNodeIds.length} 条记忆`);
      setSelectedNodeIds([]);
      await loadNodes();
    } catch (error) {
      console.error('批量更新记忆状态失败:', error);
      showError('批量操作失败', `无法更新选中的记忆：${error}`);
    } finally {
      setIsBulkBusy(false);
    }
  };

  const saveRelation = async () => {
    if (!draft.id) {
      showError('请先保存记忆', '新增记忆保存后才能建立关联');
      return;
    }
    if (!relationTargetId || relationTargetId === draft.id) {
      showError('关联目标无效', '请选择另一条记忆作为关联目标');
      return;
    }
    try {
      await api.upsertMemoryRelation({
        from_node_id: draft.id,
        to_node_id: relationTargetId,
        relation_kind: relationKind,
        weight: 1,
        note: relationNote.trim() || undefined,
      });
      setRelations(await api.listMemoryRelations(draft.id));
      await loadGraphRelations(nodes);
      setRelationTargetId('');
      setRelationNote('');
      showSuccess('关联已保存', relationKindLabel(relationKind));
    } catch (error) {
      console.error('保存记忆关系失败:', error);
      showError('关联失败', `无法保存记忆关系：${error}`);
    }
  };

  const removeRelation = async (relation: MemoryRelation) => {
    try {
      await api.deleteMemoryRelation(relation.id);
      if (draft.id) {
        setRelations(await api.listMemoryRelations(draft.id));
      }
      await loadGraphRelations(nodes);
      showSuccess('关联已删除', relationKindLabel(relation.relation_kind));
    } catch (error) {
      console.error('删除记忆关系失败:', error);
      showError('删除失败', `无法删除记忆关系：${error}`);
    }
  };

  const selectedNode = draft.id ? nodes.find((node) => node.id === draft.id) : undefined;
  const visibleRelationCount = graphRelations.filter((relation) =>
    nodes.some((node) => node.id === relation.from_node_id) &&
    nodes.some((node) => node.id === relation.to_node_id),
  ).length;
  const graphCanLoadMore = viewMode === 'graph' && nodes.length < totalNodeCount && graphLimit < MEMORY_GRAPH_MAX_LIMIT;
  const nextGraphLimit = Math.min(MEMORY_GRAPH_MAX_LIMIT, graphLimit + MEMORY_GRAPH_LIMIT_STEP, totalNodeCount);

  return (
    <div className="h-full min-h-0 p-4 flex flex-col gap-4">
      {/* 统计概览 */}
      <MemoryStats
        nodes={nodes}
        relations={graphRelations}
        totalNodeCount={totalNodeCount}
        activeNodeCount={activeNodeCount}
        weekNewCount={weekNewCount}
      />

      <div className="min-h-0 flex-1 grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_340px] gap-4">
        <div className="min-h-0 flex flex-col gap-3">
          <div className="flex flex-wrap gap-2">
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索标题、内容或关键词"
              className="h-9 min-w-64 flex-1"
            />
            <Select value={status} onValueChange={(value) => setStatus(value as MemoryStatus)}>
              <SelectTrigger className="w-28 h-9">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="active">活跃</SelectItem>
                <SelectItem value="archived">归档</SelectItem>
              </SelectContent>
            </Select>
            <div className="flex rounded-md border p-0.5 h-9">
              <Button
                variant={viewMode === 'graph' ? 'secondary' : 'ghost'}
                size="sm"
                className="h-7 px-2"
                onClick={() => setViewMode('graph')}
              >
                <Network className="w-4 h-4 mr-1.5" />
                图谱
              </Button>
              <Button
                variant={viewMode === 'list' ? 'secondary' : 'ghost'}
                size="sm"
                className="h-7 px-2"
                onClick={() => setViewMode('list')}
              >
                <List className="w-4 h-4 mr-1.5" />
                列表
              </Button>
            </div>
            <Button variant="outline" size="icon" className="h-9 w-9" onClick={loadNodes} disabled={isLoading}>
              {isLoading ? <Loader2 className="w-4 h-4 animate-spin" /> : <RefreshCw className="w-4 h-4" />}
            </Button>
            <div className="h-9">
              <RecallTestPanel />
            </div>
          </div>

          <div className="min-h-0 flex-1">
            {viewMode === 'graph' ? (
              <div className="h-full min-h-0 rounded-md border overflow-hidden bg-background flex flex-col">
                {nodes.length > 0 && (
                  <div className="flex items-center justify-between gap-3 border-b px-3 py-2">
                    <div className="min-w-0">
                      <div className="text-sm font-medium truncate">
                        {selectedNode?.title ?? 'Memory 图谱'}
                      </div>
                      <div className="text-xs text-muted-foreground">
                        {totalNodeCount > nodes.length
                          ? `已加载 ${nodes.length} / 共 ${totalNodeCount} 个节点 · ${visibleRelationCount} 条连接`
                          : `${nodes.length} 个节点 · ${visibleRelationCount} 条连接`}
                      </div>
                    </div>
                    <div className="flex items-center gap-1 shrink-0">
                      {graphCanLoadMore && (
                        <Button
                          variant="outline"
                          size="sm"
                          className="h-8"
                          onClick={() => setGraphLimit(nextGraphLimit)}
                          disabled={isLoading}
                        >
                          加载更多
                        </Button>
                      )}
                      {selectedNode && (
                        <>
                          <Button variant="ghost" size="icon" className="size-8" onClick={() => editNode(selectedNode)} title="编辑">
                            <Edit2 className="size-4" />
                          </Button>
                          {selectedNode.status === 'active' ? (
                            <Button variant="ghost" size="icon" className="size-8" onClick={() => setNodeStatus(selectedNode, 'archived')} title="归档">
                              <Archive className="size-4" />
                            </Button>
                          ) : (
                            <Button variant="ghost" size="icon" className="size-8" onClick={() => setNodeStatus(selectedNode, 'active')} title="恢复">
                              <RotateCcw className="size-4" />
                            </Button>
                          )}
                        </>
                      )}
                    </div>
                  </div>
                )}
                <div className="min-h-0 flex-1">
                  <MemoryGraphCanvas
                    nodes={nodes}
                    relations={graphRelations}
                    selectedId={draft.id}
                    isLoading={isLoading}
                    onSelect={editNode}
                    onClearSelection={startNew}
                  />
                </div>
              </div>
            ) : (
              <MemoryList
                nodes={nodes}
                page={listPage}
                pageSize={MEMORY_LIST_PAGE_SIZE}
                totalCount={totalNodeCount}
                selectedId={draft.id}
                selectedIds={selectedNodeIds}
                status={status}
                isBulkBusy={isBulkBusy}
                onPageChange={setListPage}
                onSelectNode={editNode}
                onToggleSelection={toggleNodeSelection}
                onToggleAll={toggleAllNodes}
                onSetStatus={setNodeStatus}
                onBulkStatus={setSelectedNodesStatus}
              />
            )}
          </div>
        </div>

        <div className="min-h-0 overflow-y-auto pr-1 flex flex-col gap-3">
          <MemoryEditor
            draft={draft}
            keywordsText={keywordsText}
            isSaving={isSaving}
            onDraftChange={setDraft}
            onKeywordsChange={setKeywordsText}
            onSave={saveDraft}
            onNew={startNew}
          />

          <MemoryRelationPanel
            draftId={draft.id}
            nodes={nodes}
            relations={relations}
            relationTargetId={relationTargetId}
            relationKind={relationKind}
            relationNote={relationNote}
            onTargetChange={setRelationTargetId}
            onKindChange={setRelationKind}
            onNoteChange={setRelationNote}
            onSave={saveRelation}
            onRemove={removeRelation}
          />
        </div>
      </div>
    </div>
  );
}
