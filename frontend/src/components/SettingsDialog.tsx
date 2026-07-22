import { useState, useEffect, useRef, useCallback } from 'react';
import type { ReactNode } from 'react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Textarea } from './ui/textarea';
import { Label } from './ui/label';
import { Badge } from './ui/badge';
import { Card, CardContent } from './ui/card';
import { Switch } from './ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './ui/tabs';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Settings, Eye, EyeOff, Server, Puzzle, Plus, Trash2, Loader2, Github, Globe, Edit2, KeyRound, RefreshCw, Info, FolderOpen, Save, ShieldCheck, Database, X, HardDrive, Clock, Bot as BotIcon } from 'lucide-react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type { DownloadEvent, Update } from '@tauri-apps/plugin-updater';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { api } from '@/api/tauri';
import type { McpServer, Skill, SkillDetail, ServerConfig, ModelsConfigView, ProviderConfigView, ModelEntryView, ModelCapabilityInfo, MemoryConfigView } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { useToast } from './Toast';
import { MemoryManagementSettings } from './memory';
import { IndexManagementSettings } from './index/IndexManagementSettings';
import { AutomationSettings } from './automation/AutomationSettings';
import { WebhookPanel } from './automation/WebhookPanel';
import { BotPanel } from './bots/BotPanel';

const appWindow = getCurrentWindow();

type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';
type McpTransportDraft = 'stdio' | 'http' | 'sse';

function parseListArgs(value: string): string[] {
  return value
    .split(/\s+/)
    .map((arg) => arg.trim())
    .filter(Boolean);
}

function parseKeyValueText(value: string, label: string): Record<string, string> | undefined {
  const entries: Record<string, string> = {};
  for (const rawLine of value.split(/[,\n]/)) {
    const line = rawLine.trim();
    if (!line) continue;
    const separatorIndex = line.includes('=') ? line.indexOf('=') : line.indexOf(':');
    if (separatorIndex <= 0) {
      throw new Error(`${label}格式错误：${line}`);
    }
    const key = line.slice(0, separatorIndex).trim();
    const itemValue = line.slice(separatorIndex + 1).trim();
    if (!key || !itemValue) {
      throw new Error(`${label}格式错误：${line}`);
    }
    entries[key] = itemValue;
  }

  return Object.keys(entries).length > 0 ? entries : undefined;
}

/// parseKeyValueText 的逆函数：把 Record 格式化为 KEY=VALUE 文本，每行一条。
function formatKeyValue(record?: Record<string, string>): string {
  if (!record) return '';
  return Object.entries(record)
    .map(([key, value]) => `${key}=${value}`)
    .join('\n');
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function SettingsDialog() {
  const [open, setOpen] = useState(false);
  const [activeTab, setActiveTab] = useState('agent');
  const [saveStatus, setSaveStatus] = useState<SaveStatus>('idle');
  const pendingSettingsTab = useStore((s) => s.pendingSettingsTab);
  const setPendingSettingsTab = useStore((s) => s.setPendingSettingsTab);

  // 响应外部触发打开设置页
  useEffect(() => {
    if (pendingSettingsTab) {
      setActiveTab(pendingSettingsTab);
      setOpen(true);
      setPendingSettingsTab(null);
    }
  }, [pendingSettingsTab, setPendingSettingsTab]);

  useEffect(() => {
    if (!open) {
      window.dispatchEvent(new Event('tiangong:restore-browser-panel'));
      return;
    }

    const sid = useStore.getState().activeSessionId || useStore.getState().newConversationId;
    if (sid) api.browserHide(sid).catch(console.error);
  }, [open]);

  return (
    <>
      <Button
        variant="ghost"
        className="w-full justify-start"
        onClick={() => setOpen(true)}
      >
        <Settings className="w-4 h-4 mr-2" />
        设置
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent showCloseButton={false} className="w-screen max-w-none h-screen max-h-screen overflow-hidden flex flex-col rounded-none border-0 p-0">
          {/* 顶部标题栏 — 可拖动窗口 */}
          <header
            className="flex h-12 shrink-0 items-center border-b pr-4 select-none"
            style={{ paddingLeft: navigator.platform.includes('Mac') ? '80px' : '16px' }}
            onMouseDown={(e) => {
              const tag = (e.target as HTMLElement).tagName;
              if (tag === 'INPUT' || tag === 'BUTTON') return;
              if ((e.target as HTMLElement).closest('[data-no-drag]')) return;
              appWindow.startDragging();
            }}
          >
            <span className="text-sm font-medium">设置</span>
            <span className={`ml-auto text-xs flex items-center transition-opacity ${saveStatus === 'idle' ? 'opacity-0' : 'opacity-100'} ${saveStatus === 'error' ? 'text-destructive' : 'text-muted-foreground'}`}>
              {saveStatus === 'saving' && (
                <><Loader2 className="w-3 h-3 mr-1 animate-spin" />保存中...</>
              )}
              {(saveStatus === 'saved' || saveStatus === 'idle') && '已自动保存'}
              {saveStatus === 'error' && '保存失败'}
            </span>
          </header>

          <Tabs value={activeTab} onValueChange={setActiveTab} className="flex-1 overflow-hidden flex">
            <aside className="w-60 shrink-0 border-r bg-muted/30 flex flex-col">
              <TabsList className="h-auto w-full flex-1 flex-col items-stretch justify-start rounded-none bg-transparent p-2 pt-4">
                <TabsTrigger value="agent" className="w-full justify-start px-3 py-2">
                  <ShieldCheck className="w-4 h-4 mr-2" />
                  智能体
                </TabsTrigger>
                <TabsTrigger value="llm" className="w-full justify-start px-3 py-2">
                  <Settings className="w-4 h-4 mr-2" />
                  模型配置
                </TabsTrigger>
                <TabsTrigger value="memory" className="w-full justify-start px-3 py-2">
                  <Database className="w-4 h-4 mr-2" />
                  记忆管理
                </TabsTrigger>
                <TabsTrigger value="index" className="w-full justify-start px-3 py-2">
                  <HardDrive className="w-4 h-4 mr-2" />
                  索引管理
                </TabsTrigger>
                <TabsTrigger value="mcp" className="w-full justify-start px-3 py-2">
                  <Server className="w-4 h-4 mr-2" />
                  MCP
                </TabsTrigger>
                <TabsTrigger value="skill" className="w-full justify-start px-3 py-2">
                  <Puzzle className="w-4 h-4 mr-2" />
                  Skills
                </TabsTrigger>
                <TabsTrigger value="server" className="w-full justify-start px-3 py-2">
                  <Globe className="w-4 h-4 mr-2" />
                  Server
                </TabsTrigger>
                <TabsTrigger value="automation" className="w-full justify-start px-3 py-2">
                  <Clock className="w-4 h-4 mr-2" />
                  定时任务
                </TabsTrigger>
                <TabsTrigger value="bots" className="w-full justify-start px-3 py-2">
                  <BotIcon className="w-4 h-4 mr-2" />
                  移动端控制
                </TabsTrigger>
                <TabsTrigger value="about" className="w-full justify-start px-3 py-2">
                  <Info className="w-4 h-4 mr-2" />
                  关于与更新
                </TabsTrigger>
              </TabsList>
              <div className="border-t p-2">
                <Button
                  variant="ghost"
                  className="w-full justify-start"
                  onClick={() => setOpen(false)}
                >
                  <X className="w-4 h-4 mr-2" />
                  退出设置
                </Button>
              </div>
            </aside>

            <div className="min-w-0 flex-1 flex flex-col overflow-hidden">
              <TabsContent value="agent" className="m-0 flex-1 min-h-0 overflow-hidden flex flex-col">
                <AgentSettings onSaveStatusChange={setSaveStatus} />
              </TabsContent>
              <TabsContent value="llm" className="m-0 flex-1 min-h-0 overflow-hidden">
                <LLMSettings onSaveStatusChange={setSaveStatus} />
              </TabsContent>
              <TabsContent value="memory" className="m-0 flex-1 min-h-0 overflow-hidden">
                <MemoryManagementSettings />
              </TabsContent>
              <TabsContent value="index" className="m-0 flex-1 min-h-0 overflow-hidden">
                <IndexManagementSettings />
              </TabsContent>
              <TabsContent value="mcp" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <McpSettings />
              </TabsContent>
              <TabsContent value="skill" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <SkillSettings />
              </TabsContent>
              <TabsContent value="server" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <ServerSettings />
              </TabsContent>
              <TabsContent value="automation" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <AutomationSettings />
              </TabsContent>
              <TabsContent value="bots" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <BotPanel />
              </TabsContent>
              <TabsContent value="about" className="m-0 flex-1 min-h-0 overflow-y-auto">
                <AppUpdateSettings />
              </TabsContent>
            </div>
          </Tabs>
        </DialogContent>
      </Dialog>
    </>
  );
}

function AgentSettings({ onSaveStatusChange }: { onSaveStatusChange: (status: SaveStatus) => void }) {
  const [defaultTrustMode, setDefaultTrustMode] = useState('full_trust');
  const [customPrompt, setCustomPrompt] = useState('');
  const [lastSavedPrompt, setLastSavedPrompt] = useState('');
  const { showError } = useToast();
  const { workspaceDir, setWorkspaceDir } = useStore();
  const [editWorkspaceDir, setEditWorkspaceDir] = useState(workspaceDir);
  const [isSavingWorkspace, setIsSavingWorkspace] = useState(false);
  const { showSuccess } = useToast();

  useEffect(() => {
    setEditWorkspaceDir(workspaceDir);
  }, [workspaceDir]);

  useEffect(() => {
    Promise.all([
      api.getDefaultTrustMode(),
      api.getCustomSystemPrompt(),
    ])
      .then(([mode, prompt]) => {
        setDefaultTrustMode(mode);
        setCustomPrompt(prompt);
        setLastSavedPrompt(prompt);
      })
      .catch((error) => {
        console.error('加载 Agent 配置失败:', error);
        showError('加载失败', '无法加载 Agent 配置');
      });
  }, [showError]);

  const saveCustomPrompt = useCallback(async (prompt: string) => {
    onSaveStatusChange('saving');
    try {
      await api.setCustomSystemPrompt(prompt);
      setLastSavedPrompt(prompt);
      onSaveStatusChange('saved');
      setTimeout(() => onSaveStatusChange('idle'), 2000);
    } catch (error) {
      console.error('保存自定义 Prompt 失败:', error);
      onSaveStatusChange('error');
      showError('保存失败', '无法保存自定义 Prompt');
    }
  }, [onSaveStatusChange, showError]);

  const handlePromptBlur = () => {
    if (customPrompt !== lastSavedPrompt) {
      saveCustomPrompt(customPrompt);
    }
  };

  const handleTrustModeChange = async (mode: string) => {
    setDefaultTrustMode(mode);
    onSaveStatusChange('saving');
    try {
      await api.setDefaultTrustMode(mode);
      onSaveStatusChange('saved');
      setTimeout(() => onSaveStatusChange('idle'), 2000);
    } catch (error) {
      console.error('保存默认审核模式失败:', error);
      onSaveStatusChange('error');
      showError('保存失败', '无法保存默认审核模式');
    }
  };

  const handleSelectDirectory = async () => {
    try {
      const selected = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: editWorkspaceDir || workspaceDir || undefined,
        title: '选择工作区目录',
      });
      if (selected && typeof selected === 'string') {
        setEditWorkspaceDir(selected);
      }
    } catch (error) {
      console.error('选择工作区目录失败:', error);
      showError('选择失败', '无法打开目录选择器');
    }
  };

  const handleSaveWorkspace = async () => {
    const nextWorkspaceDir = editWorkspaceDir.trim();
    if (!nextWorkspaceDir) {
      showError('路径为空', '请选择或输入工作区目录');
      return;
    }
    setIsSavingWorkspace(true);
    try {
      await setWorkspaceDir(nextWorkspaceDir);
      showSuccess('工作区已更新', '未指定对话目录时会默认使用该工作区');
    } catch (error) {
      console.error('保存工作区失败:', error);
      showError('保存失败', error instanceof Error ? error.message : '无法保存工作区目录');
    } finally {
      setIsSavingWorkspace(false);
    }
  };

  return (
    <div className="p-4 flex flex-col flex-1 min-h-0 overflow-hidden">
      {/* 固定区域：审核模式 + 工作区 */}
      <div className="shrink-0 space-y-5 pb-4">
        <div className="space-y-2">
          <Label>新对话默认审核权限</Label>
          <Select value={defaultTrustMode} onValueChange={handleTrustModeChange}>
            <SelectTrigger className="w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="full_trust">完全信任</SelectItem>
              <SelectItem value="supervised">监督审核</SelectItem>
            </SelectContent>
          </Select>
        </div>

        <div className="space-y-2">
          <Label htmlFor="workspacePath">默认工作区目录</Label>
          <div className="flex gap-2">
            <Input
              id="workspacePath"
              value={editWorkspaceDir}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditWorkspaceDir(e.target.value)}
              placeholder="选择或输入工作区目录"
              disabled={isSavingWorkspace}
            />
            <Button variant="outline" onClick={handleSelectDirectory} disabled={isSavingWorkspace}>
              <FolderOpen className="w-4 h-4 mr-2" />
              选择
            </Button>
          </div>
          <div className="flex items-center justify-between min-h-7">
            <p className="text-xs text-muted-foreground">
              {workspaceDir
                ? <><span className="opacity-60">当前：</span><span className="font-mono">{workspaceDir}</span></>
                : <span className="opacity-60">未设置</span>
              }
            </p>
            {editWorkspaceDir.trim() !== workspaceDir && (
              <Button size="sm" className="h-6 text-xs" onClick={handleSaveWorkspace} disabled={isSavingWorkspace}>
                {isSavingWorkspace
                  ? <><Loader2 className="w-3 h-3 mr-1 animate-spin" />保存中...</>
                  : <><Save className="w-3 h-3 mr-1" />保存</>
                }
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* 弹性区域：自定义 Prompt */}
      <div className="flex flex-col flex-1 min-h-0">
        <Label htmlFor="customSystemPrompt" className="shrink-0 mb-2">自定义 Prompt</Label>
        <Textarea
          id="customSystemPrompt"
          value={customPrompt}
          onChange={(event) => setCustomPrompt(event.target.value)}
          onBlur={handlePromptBlur}
          className="flex-1 min-h-0 resize-none"
          placeholder="例如：回复时保持简洁，优先给出可执行步骤。"
        />
      </div>
    </div>
  );
}

// ============================================================================
// LLM 设置组件（三层架构：Providers / Models / Routing）
// ============================================================================

type LLMSubTab = 'providers' | 'routing' | 'memory';

function LLMSettings({ onSaveStatusChange }: { onSaveStatusChange: (status: SaveStatus) => void }) {
  const [subTab, setSubTab] = useState<LLMSubTab>('providers');
  const [modelsConfig, setModelsConfig] = useState<ModelsConfigView>({
    providers: {},
    models: {},
    routing: {},
  });
  const [capabilities, setCapabilities] = useState<ModelCapabilityInfo[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const { showError } = useToast();
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const loadConfig = async () => {
    setIsLoading(true);
    try {
      const [cfg, caps] = await Promise.all([
        api.getModelsConfig(),
        api.getModelCapabilities(),
      ]);
      // Ensure default providers are always present
      const mergedProviders = { ...cfg.providers };
      for (const [name, providerConfig] of Object.entries(DEFAULT_PROVIDERS)) {
        if (!mergedProviders[name]) {
          mergedProviders[name] = { ...providerConfig };
        }
      }
      setModelsConfig({ ...cfg, providers: mergedProviders });
      setCapabilities(caps);
    } catch (error) {
      console.error('加载配置失败:', error);
      showError('加载失败', '无法加载模型配置，请重试');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  // 自动保存：配置变更时 debounce 500ms 后保存到后端
  const autoSave = useCallback(async (config: ModelsConfigView) => {
    onSaveStatusChange('saving');
    try {
      await api.setModelsConfig(config);
      onSaveStatusChange('saved');
      setTimeout(() => onSaveStatusChange('idle'), 2000);
    } catch (error) {
      console.error('自动保存失败:', error);
      onSaveStatusChange('error');
      showError('保存失败', '无法保存模型配置');
    }
  }, [showError, onSaveStatusChange]);

  const handleChange = useCallback((newConfig: ModelsConfigView) => {
    setModelsConfig(newConfig);
    // debounce 自动保存
    if (saveTimerRef.current) {
      clearTimeout(saveTimerRef.current);
    }
    saveTimerRef.current = setTimeout(() => {
      autoSave(newConfig);
    }, 500);
  }, [autoSave]);

  // 清理 timer
  useEffect(() => {
    return () => {
      if (saveTimerRef.current) {
        clearTimeout(saveTimerRef.current);
      }
    };
  }, []);

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-primary mr-2" />
        <span className="text-sm text-muted-foreground">加载配置中...</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full">
      {/* 子标签栏 — 固定不动 */}
      <div className="flex gap-1 shrink-0 p-4 pb-0">
        {(['providers', 'routing', 'memory'] as const).map((tab) => (
          <button
            key={tab}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              subTab === tab
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setSubTab(tab)}
          >
            {tab === 'providers' ? '模型' : tab === 'routing' ? '路由' : '记忆设置'}
          </button>
        ))}
      </div>

      {/* 内容区域 */}
      <div className={`flex-1 min-h-0 ${subTab === 'providers' ? 'overflow-hidden' : 'overflow-y-auto p-4'}`}>
        {subTab === 'providers' && (
          <ProviderModelsView config={modelsConfig} onChange={handleChange} capabilities={capabilities} />
        )}
        {subTab === 'routing' && (
          <RoutingSection config={modelsConfig} onChange={handleChange} />
        )}
        {subTab === 'memory' && (
          <MemorySettings
            modelsConfig={modelsConfig}
            onSaveStatusChange={onSaveStatusChange}
          />
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Memory 设置组件（独立模型配置）
// ============================================================================

function MemorySettings({
  modelsConfig,
  onSaveStatusChange,
}: {
  modelsConfig: ModelsConfigView;
  onSaveStatusChange: (status: SaveStatus) => void;
}) {
  const [config, setConfig] = useState<MemoryConfigView>({ vector_mode: 'auto' });
  const [isLoading, setIsLoading] = useState(false);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const { showError } = useToast();

  const loadConfig = async () => {
    setIsLoading(true);
    try {
      const cfg = await api.getMemoryConfig();
      setConfig({ ...cfg, vector_mode: cfg.vector_mode || 'auto' });
    } catch (error) {
      console.error('加载 Memory 配置失败:', error);
      showError('加载失败', '无法加载 Memory 配置');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  const autoSave = useCallback(async (nextConfig: MemoryConfigView) => {
    onSaveStatusChange('saving');
    try {
      await api.setMemoryConfig(nextConfig);
      onSaveStatusChange('saved');
      setTimeout(() => onSaveStatusChange('idle'), 2000);
    } catch (error) {
      console.error('保存 Memory 配置失败:', error);
      onSaveStatusChange('error');
      showError('保存失败', '无法保存 Memory 配置');
    }
  }, [onSaveStatusChange, showError]);

  const handleChange = useCallback((nextConfig: MemoryConfigView) => {
    setConfig(nextConfig);
    if (saveTimerRef.current) {
      clearTimeout(saveTimerRef.current);
    }
    saveTimerRef.current = setTimeout(() => autoSave(nextConfig), 500);
  }, [autoSave]);

  useEffect(() => {
    return () => {
      if (saveTimerRef.current) {
        clearTimeout(saveTimerRef.current);
      }
    };
  }, []);

  const modelKeysFor = (acceptedCapabilities: string[]) =>
    Object.entries(modelsConfig.models)
      .filter(([, model]) => {
        if (model.capabilities.length === 0) return true;
        return acceptedCapabilities.some((capability) => model.capabilities.includes(capability));
      })
      .map(([key]) => key);

  const modelLabel = (modelKey: string) => {
    const model = modelsConfig.models[modelKey];
    if (!model) return modelKey;
    return `${model.provider} / ${model.model}`;
  };

  const setModelKey = (
    key: 'model_key' | 'embedding_key' | 'rerank_key',
    modelKey: string | undefined,
  ) => {
    handleChange({ ...config, [key]: modelKey });
  };

  const embeddingDimension = config.embedding_key
    ? Number(modelsConfig.models[config.embedding_key]?.options?.dimension || 0)
    : 0;

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="w-6 h-6 animate-spin text-primary mr-2" />
        <span className="text-sm text-muted-foreground">加载 Memory 配置中...</span>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <MemoryModelSelectSection
        title="记忆文本模型"
        description="片段提取、回忆规划和结果整理使用的文本模型"
        selectedKey={config.model_key}
        candidates={modelKeysFor(['chat', 'lite'])}
        modelLabel={modelLabel}
        onChange={(modelKey) => setModelKey('model_key', modelKey)}
      />

      <MemoryModelSelectSection
        title="记忆嵌入模型"
        description="语义检索和向量索引使用的嵌入模型"
        selectedKey={config.embedding_key}
        candidates={modelKeysFor(['embedding'])}
        modelLabel={modelLabel}
        onChange={(modelKey) => setModelKey('embedding_key', modelKey)}
        footer={
          config.embedding_key ? (
            <div className={`text-xs ${embeddingDimension > 0 ? 'text-muted-foreground' : 'text-destructive'}`}>
              {embeddingDimension > 0
                ? `当前维度：${embeddingDimension}`
                : '选中的嵌入模型缺少 options.dimension，请先在模型页补齐。'}
            </div>
          ) : null
        }
      />

      <MemoryModelSelectSection
        title="记忆重排模型"
        description="召回结果精排模型，当前保存为独立配置供后续召回链路消费"
        selectedKey={config.rerank_key}
        candidates={modelKeysFor(['rerank'])}
        modelLabel={modelLabel}
        onChange={(modelKey) => setModelKey('rerank_key', modelKey)}
      />

      <Card>
        <CardContent className="p-4 space-y-3">
          <div>
            <h4 className="text-sm font-medium">向量模式</h4>
            <p className="text-xs text-muted-foreground mt-1">
              控制语义检索使用内置向量索引、外部 Qdrant 或完全关闭。
            </p>
          </div>
          <Select
            value={config.vector_mode || 'auto'}
            onValueChange={(value) => handleChange({ ...config, vector_mode: value })}
          >
            <SelectTrigger className="w-60 h-8 text-sm">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">自动</SelectItem>
              <SelectItem value="embedded">内置向量索引</SelectItem>
              <SelectItem value="external_qdrant">外部 Qdrant</SelectItem>
              <SelectItem value="disabled">禁用向量层</SelectItem>
            </SelectContent>
          </Select>
        </CardContent>
      </Card>
    </div>
  );
}

function MemoryModelSelectSection({
  title,
  description,
  selectedKey,
  candidates,
  modelLabel,
  onChange,
  footer,
}: {
  title: string;
  description: string;
  selectedKey?: string;
  candidates: string[];
  modelLabel: (modelKey: string) => string;
  onChange: (modelKey: string | undefined) => void;
  footer?: ReactNode;
}) {
  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <div>
          <h4 className="text-sm font-medium">{title}</h4>
          <p className="text-xs text-muted-foreground mt-1">{description}</p>
        </div>
        {candidates.length > 0 ? (
          <Select
            value={selectedKey || '__none__'}
            onValueChange={(value) => onChange(value === '__none__' ? undefined : value)}
          >
            <SelectTrigger className="h-8 text-sm">
              <SelectValue placeholder="选择模型" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__none__">-- 未配置 --</SelectItem>
              {candidates.map((modelKey) => (
                <SelectItem key={modelKey} value={modelKey}>
                  {modelLabel(modelKey)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <div className="text-xs text-muted-foreground">
            请先在模型页添加对应能力的模型。
          </div>
        )}
        {footer}
      </CardContent>
    </Card>
  );
}


// ---------------------------------------------------------------------------
// 预设供应商
// ---------------------------------------------------------------------------

const DEFAULT_PROVIDERS: Record<string, ProviderConfigView> = {
  'DeepSeek': { base_url: 'https://api.deepseek.com', api_key: '', timeout_ms: 300000, protocol: 'deepseek' },
  '智谱': { base_url: 'https://open.bigmodel.cn/api/paas/v4', api_key: '', timeout_ms: 300000, protocol: 'openai_chatcompletions' },
};

interface UrlPreset {
  label: string;
  url: string;
  protocol: string;
}

const DEFAULT_PROVIDER_URL_PRESETS: Record<string, UrlPreset[]> = {
  '智谱': [
    { label: 'OpenAI 兼容（通用）', url: 'https://open.bigmodel.cn/api/paas/v4', protocol: 'openai_chatcompletions' },
    { label: 'OpenAI 兼容（Coding 套餐）', url: 'https://open.bigmodel.cn/api/coding/paas/v4', protocol: 'openai_chatcompletions' },
    { label: 'Anthropic 兼容（Coding 套餐）', url: 'https://open.bigmodel.cn/api/anthropic', protocol: 'anthropic' },
  ],
};

// 协议对应的默认 URL
const PROTOCOL_DEFAULTS: Record<string, string> = {
  openai: 'https://api.openai.com/v1',
  openai_chatcompletions: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com',
};

// ---------------------------------------------------------------------------
// DeepSeek 余额查询
// ---------------------------------------------------------------------------

function ProviderBalanceSection({ providerName }: { providerName: string }) {
  const [balance, setBalance] = useState<import('@/api/tauri').ProviderBalance | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const fetchBalance = async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await api.getProviderBalance(providerName);
      setBalance(result);
    } catch (e) {
      setError(e instanceof Error ? e.message : '查询失败');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="mt-3 pt-3 border-t">
      <div className="flex items-center gap-3 flex-wrap">
        <Label className="text-xs">账户余额</Label>
        <Button
          size="sm"
          variant="outline"
          className="h-6 text-xs px-2"
          onClick={fetchBalance}
          disabled={loading}
        >
          {loading ? '查询中...' : '查询余额'}
        </Button>
        {error && <span className="text-xs text-destructive">{error}</span>}
        {balance && (
          <div className="flex items-center gap-3 flex-wrap">
            <span className={`text-xs font-medium ${balance.is_available ? 'text-green-500' : 'text-destructive'}`}>
              {balance.is_available ? '可用' : '不可用'}
            </span>
            {balance.balance_infos.map((info, i) => (
              <span key={i} className="text-xs text-muted-foreground">
                {info.currency === 'CNY' ? '¥' : '$'}{info.total_balance}
                <span className="ml-1.5 opacity-70">充值 {info.topped_up_balance}</span>
                <span className="ml-1.5 opacity-70">赠金 {info.granted_balance}</span>
              </span>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 供应商与模型 — 分栏视图
// ---------------------------------------------------------------------------

function ProviderModelsView({
  config,
  onChange,
  capabilities,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
  capabilities: ModelCapabilityInfo[];
}) {
  const providerKeys = Object.keys(config.providers);
  const defaultKeys = Object.keys(DEFAULT_PROVIDERS).filter((k) => providerKeys.includes(k));
  const customKeys = providerKeys.filter((k) => !(k in DEFAULT_PROVIDERS)).sort();
  const sortedProviderKeys = [...defaultKeys, ...customKeys];

  const [selectedProvider, setSelectedProvider] = useState(sortedProviderKeys[0] || '');
  const [showApiKey, setShowApiKey] = useState(false);

  // Add provider modal
  const [showAddProvider, setShowAddProvider] = useState(false);
  const [newProviderKey, setNewProviderKey] = useState('');
  const [newProviderDraft, setNewProviderDraft] = useState<ProviderConfigView>({
    base_url: '', api_key: '', timeout_ms: 300000, protocol: 'openai_chatcompletions',
  });
  const [showNewApiKey, setShowNewApiKey] = useState(false);

  // Model modal
  const [modelModalMode, setModelModalMode] = useState<'add' | 'edit' | null>(null);
  const [editingModelKey, setEditingModelKey] = useState('');
  const [modelDraft, setModelDraft] = useState<ModelEntryView>({
    provider: '', model: '', capabilities: [], options: {}, context_window: undefined,
  });
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);
  const [ttsVoices, setTtsVoices] = useState<{ id: string; name: string; gender?: string }[]>([]);
  const [isFetchingVoices, setIsFetchingVoices] = useState(false);
  const [isProbingEmbeddingDimension, setIsProbingEmbeddingDimension] = useState(false);
  const { showSuccess, showError } = useToast();

  // Effective selected provider
  const activeProvider = (selectedProvider && config.providers[selectedProvider])
    ? selectedProvider
    : (sortedProviderKeys[0] || '');
  const selectedConfig = config.providers[activeProvider] || null;
  const providerModels = activeProvider
    ? Object.entries(config.models).filter(([, m]) => m.provider === activeProvider).sort(([a], [b]) => a.localeCompare(b))
    : [];

  // ---- Provider handlers ----
  const updateProviderField = (field: keyof ProviderConfigView, value: unknown) => {
    if (!activeProvider) return;
    const next = { ...config };
    next.providers = {
      ...next.providers,
      [activeProvider]: { ...next.providers[activeProvider], [field]: value },
    };
    onChange(next);
  };

  const removeProvider = (key: string) => {
    const next = { ...config };
    const { [key]: _, ...rest } = next.providers;
    next.providers = rest;
    // 清理引用该 provider 的模型
    const newModels = { ...next.models };
    for (const [mk, mv] of Object.entries(newModels)) {
      if (mv.provider === key) delete newModels[mk];
    }
    next.models = newModels;
    // 清理引用该 provider 的路由
    const newRouting = { ...next.routing };
    for (const [slot, entry] of Object.entries(newRouting)) {
      if (entry.provider === key) delete newRouting[slot];
    }
    next.routing = newRouting;
    onChange(next);
    if (activeProvider === key) {
      const remaining = sortedProviderKeys.filter((k) => k !== key);
      setSelectedProvider(remaining[0] || '');
    }
  };

  const addCustomProvider = () => {
    if (!newProviderKey.trim()) return;
    const key = newProviderKey.trim();
    const next = { ...config };
    next.providers = { ...next.providers, [key]: { ...newProviderDraft } };
    onChange(next);
    setSelectedProvider(key);
    setShowAddProvider(false);
    setNewProviderKey('');
    setNewProviderDraft({ base_url: '', api_key: '', timeout_ms: 300000, protocol: 'openai_chatcompletions' });
  };

  // ---- Model handlers ----
  const openAddModel = () => {
    setModelModalMode('add');
    setModelDraft({ provider: activeProvider, model: '', capabilities: [], options: {}, context_window: undefined });
    setAvailableModels([]);
    setTtsVoices([]);
  };

  const openEditModel = (key: string) => {
    setModelModalMode('edit');
    setEditingModelKey(key);
    setModelDraft({ ...config.models[key] });
    setAvailableModels([]);
  };

  const saveModelEdit = () => {
    if (!editingModelKey) return;
    const next = { ...config };
    next.models = { ...next.models, [editingModelKey]: { ...modelDraft } };
    onChange(next);
    setModelModalMode(null);
  };

  const addModel = () => {
    if (!modelDraft.model.trim()) return;
    let key = modelDraft.model.trim();
    if (config.models[key]) key = `${modelDraft.provider}-${key}`;
    const next = { ...config };
    next.models = { ...next.models, [key]: { ...modelDraft } };
    onChange(next);
    setModelModalMode(null);
  };

  const removeModel = (key: string) => {
    const next = { ...config };
    const { [key]: _, ...rest } = next.models;
    next.models = rest;
    // 同时清理路由中引用该模型的条目
    const newRouting = { ...next.routing };
    for (const [slot, entry] of Object.entries(newRouting)) {
      if (entry.provider === config.models[key]?.provider && entry.model === config.models[key]?.model) {
        delete newRouting[slot];
      }
    }
    next.routing = newRouting;
    onChange(next);
  };

  // 模型名变化时，若 context_window 未设且有 chat/multimodal 能力，查映射默认值填入
  useEffect(() => {
    const model = modelDraft.model.trim();
    const hasCtxCapability = modelDraft.capabilities.includes('chat') || modelDraft.capabilities.includes('multimodal');
    if (!model || !hasCtxCapability || modelDraft.context_window !== undefined) return;
    let cancelled = false;
    api.resolveModelContextWindow(model).then((defaultCtx) => {
      if (!cancelled && defaultCtx > 0) {
        setModelDraft((prev) => prev.context_window === undefined ? { ...prev, context_window: defaultCtx } : prev);
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [modelDraft.model, modelDraft.capabilities, modelDraft.context_window]);

  const toggleCapability = (cap: string) => {
    if (modelDraft.capabilities.includes(cap)) {
      setModelDraft({ ...modelDraft, capabilities: modelDraft.capabilities.filter((c) => c !== cap) });
    } else {
      setModelDraft({ ...modelDraft, capabilities: [...modelDraft.capabilities, cap] });
    }
  };

  const fetchModelsForProvider = async (providerKey: string) => {
    const provider = config.providers[providerKey];
    if (!provider?.base_url || !provider?.api_key) {
      showError('配置不完整', '请先配置 Base URL 和 API Key');
      return;
    }
    setIsFetchingModels(true);
    try {
      const models = await api.fetchProviderModels(provider.base_url, provider.api_key, provider.timeout_ms, provider.protocol);
      if (models.length === 0) showError('无可用模型', '该供应商未返回任何模型');
      setAvailableModels(models);
    } catch (error) {
      showError('获取失败', `无法获取模型列表：${error}`);
      setAvailableModels([]);
    } finally {
      setIsFetchingModels(false);
    }
  };

  const fetchTtsVoices = async () => {
    setIsFetchingVoices(true);
    try { setTtsVoices(await api.listTtsVoices()); } catch { setTtsVoices([]); } finally { setIsFetchingVoices(false); }
  };

  // DeepSeek 自动获取模型：选中 DeepSeek 且有 api-key 时自动拉取并填充
  useEffect(() => {
    if (activeProvider !== 'DeepSeek' || !selectedConfig?.api_key?.trim()) return;
    if (isFetchingModels) return;

    let cancelled = false;
    const autoFetch = async () => {
      const provider = config.providers[activeProvider];
      if (!provider?.base_url || !provider?.api_key) return;
      setIsFetchingModels(true);
      try {
        const models = await api.fetchProviderModels(provider.base_url, provider.api_key, provider.timeout_ms, provider.protocol);
        if (cancelled || models.length === 0) return;
        const next = { ...config };
        for (const modelId of models) {
          if (!next.models[modelId]) {
            next.models = { ...next.models, [modelId]: { provider: activeProvider, model: modelId, capabilities: ['chat'], options: {} } };
          }
        }
        onChange(next);
      } catch {
        // 静默失败，不影响 UI
      } finally {
        if (!cancelled) setIsFetchingModels(false);
      }
    };
    autoFetch();
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProvider, selectedConfig?.api_key]);

  const probeEmbeddingDimension = async () => {
    const provider = config.providers[modelDraft.provider];
    if (!provider?.base_url || !modelDraft.model.trim()) {
      showError('配置不完整', '请先选择供应商并填写嵌入模型名称');
      return;
    }
    setIsProbingEmbeddingDimension(true);
    try {
      const dimension = await api.probeEmbeddingDimension(provider.base_url, provider.api_key, modelDraft.model.trim(), provider.timeout_ms, provider.protocol);
      setModelDraft((c) => ({ ...c, options: { ...c.options, dimension } }));
      showSuccess('获取成功', `Embedding 维度：${dimension}`);
    } catch (error) {
      showError('获取失败', `无法获取 Embedding 维度：${error}`);
    } finally {
      setIsProbingEmbeddingDimension(false);
    }
  };

  return (
    <div className="flex h-full">
      {/* 左侧：供应商列表 */}
      <div className="w-56 shrink-0 border-r flex flex-col">
        <div className="px-3 pt-4 pb-2 text-xs text-muted-foreground font-medium">供应商</div>
        <div className="flex-1 overflow-y-auto px-2 pt-1">
          {/* 默认供应商 — Card 风格 */}
          {defaultKeys.map((key) => (
            <button
              key={key}
              className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors flex items-center justify-between border mb-1.5 ${
                activeProvider === key
                  ? 'bg-primary/10 text-primary font-medium border-primary/30'
                  : 'border-border hover:bg-muted text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setSelectedProvider(key)}
            >
              <span>{key}</span>
            </button>
          ))}
          {/* 自定义供应商 */}
          {customKeys.length > 0 && defaultKeys.length > 0 && <div className="border-t my-1.5 mx-1" />}
          {customKeys.map((key) => (
            <button
              key={key}
              className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
                activeProvider === key
                  ? 'bg-primary/10 text-primary font-medium'
                  : 'hover:bg-muted text-muted-foreground hover:text-foreground'
              }`}
              onClick={() => setSelectedProvider(key)}
            >
              {key}
            </button>
          ))}
        </div>
        <div className="border-t p-2">
          <Button size="sm" className="w-full" onClick={() => { setShowAddProvider(true); setNewProviderKey(''); setNewProviderDraft({ base_url: '', api_key: '', timeout_ms: 300000, protocol: 'openai_chatcompletions' }); setShowNewApiKey(false); }}>
            <Plus className="w-3 h-3 mr-1" />自定义供应商
          </Button>
        </div>
      </div>

      {/* 右侧：供应商设置 + 模型列表 */}
      <div className="flex-1 min-w-0 overflow-y-auto p-4">
        {activeProvider && selectedConfig ? (
          <>
            {/* 供应商设置 */}
            <div className="mb-6">
              <div className="flex items-center justify-between mb-3">
                <h4 className="text-sm font-medium">{activeProvider} 设置</h4>
                {!(activeProvider in DEFAULT_PROVIDERS) && (
                <Button variant="ghost" size="sm" className="h-7 text-destructive hover:text-destructive hover:bg-destructive/10" onClick={() => removeProvider(activeProvider)}>
                  <Trash2 className="w-3 h-3 mr-1" />移除供应商
                </Button>
                )}
              </div>
              <div className="space-y-3">
                <div>
                  <Label className="text-xs">API Key</Label>
                  <div className="relative">
                    <Input
                      type={showApiKey ? 'text' : 'password'}
                      value={selectedConfig.api_key}
                      onChange={(e) => updateProviderField('api_key', e.target.value)}
                      className="text-sm h-8 pr-8"
                      placeholder="sk-... 或 ${ENV_VAR}"
                    />
                    <button type="button" onClick={() => setShowApiKey(!showApiKey)} className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground">
                      {showApiKey ? <EyeOff className="w-3.5 h-3.5" /> : <Eye className="w-3.5 h-3.5" />}
                    </button>
                  </div>
                  <p className="text-xs text-muted-foreground mt-1">支持 {'${ENV_VAR}'} 引用环境变量</p>
                </div>
                {activeProvider !== 'DeepSeek' && (
                <div>
                  <Label className="text-xs">Base URL</Label>
                  {(() => {
                    const urlPresets = DEFAULT_PROVIDER_URL_PRESETS[activeProvider];
                    if (urlPresets) {
                      const matchIdx = urlPresets.findIndex((p) => p.url === selectedConfig.base_url);
                      const isCustom = matchIdx === -1;
                      return (
                        <>
                          <Select
                            value={isCustom ? '__custom__' : String(matchIdx)}
                            onValueChange={(v) => {
                              if (v === '__custom__') {
                                updateProviderField('base_url', '');
                              } else {
                                const preset = urlPresets[parseInt(v)];
                                const next = { ...config };
                                next.providers = { ...next.providers, [activeProvider]: { ...selectedConfig, base_url: preset.url, protocol: preset.protocol } };
                                onChange(next);
                              }
                            }}
                          >
                            <SelectTrigger className="h-8 text-sm"><SelectValue /></SelectTrigger>
                            <SelectContent>
                              {urlPresets.map((p, i) => (
                                <SelectItem key={i} value={String(i)}>{p.label}</SelectItem>
                              ))}
                              <SelectItem value="__custom__">自定义 URL</SelectItem>
                            </SelectContent>
                          </Select>
                          {!isCustom && (
                            <div className="text-xs text-muted-foreground mt-1.5 font-mono break-all">{selectedConfig.base_url}</div>
                          )}
                          {isCustom && (
                            <Input
                              value={selectedConfig.base_url}
                              onChange={(e) => updateProviderField('base_url', e.target.value)}
                              className="text-sm h-8 mt-2"
                              placeholder="https://..."
                            />
                          )}
                        </>
                      );
                    }
                    return (
                      <Input
                        value={selectedConfig.base_url}
                        onChange={(e) => updateProviderField('base_url', e.target.value)}
                        className="text-sm h-8"
                        placeholder="https://api.openai.com/v1"
                      />
                    );
                  })()}
                </div>
                )}
                <div className="flex gap-3">
                  {activeProvider !== 'DeepSeek' && (
                  <div className="flex-1">
                    <Label className="text-xs">协议</Label>
                    <Select
                      value={selectedConfig.protocol || 'openai_chatcompletions'}
                      onValueChange={(v) => {
                        const next = { ...config };
                        next.providers = { ...next.providers, [activeProvider]: { ...selectedConfig, protocol: v } };
                        onChange(next);
                      }}
                    >
                      <SelectTrigger className="h-8 text-sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="openai_chatcompletions">OpenAI Chat Completions</SelectItem>
                        <SelectItem value="anthropic">Anthropic</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  )}
                  <div className="w-32">
                    <Label className="text-xs">超时 (ms)</Label>
                    <Input
                      type="number"
                      value={selectedConfig.timeout_ms}
                      onChange={(e) => updateProviderField('timeout_ms', parseInt(e.target.value) || 300000)}
                      className="text-sm h-8"
                    />
                  </div>
                </div>
                {/* DeepSeek 余额查询 */}
                {activeProvider === 'DeepSeek' && selectedConfig.api_key.trim() && (
                  <ProviderBalanceSection providerName={activeProvider} />
                )}
              </div>
            </div>

            <div className="border-t my-4" />

            {/* 模型列表 */}
            <div>
              <div className="flex items-center justify-between mb-3">
                <h4 className="text-sm font-medium">模型列表</h4>
                <Button size="sm" onClick={openAddModel}>
                  <Plus className="w-3 h-3 mr-1" />添加模型
                </Button>
              </div>
              {providerModels.length === 0 ? (
                <div className="text-center text-muted-foreground py-6 text-sm">暂无模型，点击上方按钮添加</div>
              ) : (
                <div className="space-y-2">
                  {providerModels.map(([key, m]) => (
                    <Card key={key}>
                      <CardContent className="p-3">
                        <div className="flex items-center justify-between">
                          <div>
                            <span className="font-medium text-sm">{key}</span>
                            <div className="text-xs text-muted-foreground mt-1">{m.model}</div>
                            <div className="flex gap-1 mt-1">
                              {m.capabilities.map((cap) => {
                                const capInfo = capabilities.find((c) => c.key === cap);
                                return <Badge key={cap} variant="secondary" className="text-[10px] px-1.5 py-0 h-5">{capInfo?.display_name || cap}</Badge>;
                              })}
                            </div>
                          </div>
                          <div className="flex items-center gap-1">
                            <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => openEditModel(key)} title="编辑"><Edit2 className="w-3.5 h-3.5" /></Button>
                            <Button variant="ghost" size="icon" className="h-7 w-7 hover:bg-destructive/20 hover:text-destructive" onClick={() => removeModel(key)} title="删除"><Trash2 className="w-3.5 h-3.5" /></Button>
                          </div>
                        </div>
                      </CardContent>
                    </Card>
                  ))}
                </div>
              )}
            </div>
          </>
        ) : (
          <div className="flex items-center justify-center h-full text-muted-foreground text-sm">请从左侧选择或添加一个供应商</div>
        )}
      </div>

      {/* 添加供应商 Modal */}
      <Dialog open={showAddProvider} onOpenChange={setShowAddProvider}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>添加自定义供应商</DialogTitle></DialogHeader>
          <div className="space-y-3 pt-2">
            <div>
              <Label className="text-xs">供应商名称</Label>
              <Input value={newProviderKey} onChange={(e) => setNewProviderKey(e.target.value)} className="text-sm h-8" placeholder="例如: MyProvider" />
            </div>
            <ProviderForm draft={newProviderDraft} setDraft={setNewProviderDraft} showApiKey={showNewApiKey} setShowApiKey={setShowNewApiKey} onSave={addCustomProvider} onCancel={() => setShowAddProvider(false)} saveLabel="添加" />
          </div>
        </DialogContent>
      </Dialog>

      {/* 添加/编辑模型 Modal */}
      <Dialog open={modelModalMode !== null} onOpenChange={(v) => { if (!v) { setModelModalMode(null); setAvailableModels([]); } }}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>{modelModalMode === 'add' ? '添加模型' : `编辑模型: ${editingModelKey}`}</DialogTitle></DialogHeader>
          <div className="pt-2 space-y-3">
            <div>
              <Label className="text-xs">供应商</Label>
              <div className="text-sm text-muted-foreground mt-0.5">{modelDraft.provider}</div>
            </div>
            <div>
              <div className="flex items-center justify-between">
                <Label className="text-xs">模型名称</Label>
                <Button variant="ghost" size="sm" className="h-5 text-xs px-2" onClick={() => fetchModelsForProvider(modelDraft.provider)} disabled={isFetchingModels}>
                  {isFetchingModels ? <><Loader2 className="w-3 h-3 mr-1 animate-spin" />获取中...</> : '获取模型列表'}
                </Button>
              </div>
              {availableModels.length > 0 ? (
                <Select value={modelDraft.model} onValueChange={(v) => setModelDraft({ ...modelDraft, model: v })}>
                  <SelectTrigger className="h-8 text-sm"><SelectValue placeholder="-- 选择模型 --" /></SelectTrigger>
                  <SelectContent>
                    {availableModels.map((m) => <SelectItem key={m} value={m}>{m}</SelectItem>)}
                  </SelectContent>
                </Select>
              ) : (
                <Input value={modelDraft.model} onChange={(e) => setModelDraft({ ...modelDraft, model: e.target.value })} className="text-sm h-8" placeholder="gpt-4o, deepseek-chat, ..." />
              )}
            </div>
            <div>
              <Label className="text-xs">能力</Label>
              <div className="flex flex-wrap gap-1.5 mt-1">
                {capabilities.filter((cap) => cap.key !== 'lite').map((cap) => (
                  <button
                    key={cap.key}
                    className={`px-2 py-0.5 text-xs rounded border transition-colors ${modelDraft.capabilities.includes(cap.key) ? 'bg-primary/20 text-primary border-primary/40' : 'bg-secondary text-muted-foreground border-border hover:text-foreground'}`}
                    onClick={() => toggleCapability(cap.key)}
                  >
                    {cap.display_name}
                  </button>
                ))}
              </div>
            </div>
            {(modelDraft.capabilities.includes('chat') || modelDraft.capabilities.includes('multimodal')) && (
              <div>
                <Label className="text-xs">上下文窗口 (context_window)</Label>
                <Input
                  type="number"
                  className="h-8 text-sm mt-1"
                  placeholder="留空使用模型默认值"
                  value={modelDraft.context_window ?? ''}
                  onChange={(e) => {
                    const v = e.target.value.trim();
                    setModelDraft({ ...modelDraft, context_window: v === '' ? undefined : Math.max(0, parseInt(v, 10) || 0) });
                  }}
                />
                <p className="text-xs text-muted-foreground mt-0.5">单位：token。留空时从 context_windows.json 映射表取默认值。</p>
              </div>
            )}
            {modelDraft.capabilities.includes('tts') && (
              <div>
                <div className="flex items-center justify-between">
                  <Label className="text-xs">TTS 音色 (voice)</Label>
                  {ttsVoices.length === 0 && (
                    <Button variant="ghost" size="sm" className="h-5 text-xs px-2" onClick={fetchTtsVoices} disabled={isFetchingVoices}>
                      {isFetchingVoices ? <><Loader2 className="w-3 h-3 mr-1 animate-spin" />获取中...</> : '获取可用音色'}
                    </Button>
                  )}
                </div>
                {ttsVoices.length > 0 ? (
                  <Select value={(modelDraft.options?.voice as string) || '__default__'} onValueChange={(v) => setModelDraft({ ...modelDraft, options: { ...modelDraft.options, voice: v === '__default__' ? undefined : v } })}>
                    <SelectTrigger className="h-8 text-sm"><SelectValue placeholder="-- 使用默认音色 --" /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="__default__">-- 使用默认音色 --</SelectItem>
                      {ttsVoices.map((v) => <SelectItem key={v.id} value={v.id}>{v.name}{v.gender ? ` (${v.gender})` : ''}</SelectItem>)}
                    </SelectContent>
                  </Select>
                ) : (
                  <Input value={(modelDraft.options?.voice as string) || ''} onChange={(e) => setModelDraft({ ...modelDraft, options: { ...modelDraft.options, voice: e.target.value || undefined } })} className="text-sm h-8" placeholder="输入音色名称" />
                )}
              </div>
            )}
            {modelDraft.capabilities.includes('embedding') && (
              <div>
                <div className="flex items-center justify-between">
                  <Label className="text-xs">Embedding 维度</Label>
                  <Button variant="ghost" size="sm" className="h-5 text-xs px-2" onClick={probeEmbeddingDimension} disabled={isProbingEmbeddingDimension || !modelDraft.model.trim()}>
                    {isProbingEmbeddingDimension ? <><Loader2 className="w-3 h-3 mr-1 animate-spin" />获取中...</> : '获取维度'}
                  </Button>
                </div>
                <Input type="number" min={1} value={(modelDraft.options?.dimension as number | undefined) || ''} onChange={(e) => setModelDraft({ ...modelDraft, options: { ...modelDraft.options, dimension: e.target.value ? Number(e.target.value) : undefined } })} className="text-sm h-8" placeholder="例如 1536、1024、768" />
                <p className="text-xs text-muted-foreground mt-1">不同 embedding 模型需要填写对应维度。</p>
              </div>
            )}
            {modelDraft.capabilities.includes('rerank') && (
              <div className="rounded-md border border-dashed p-2 text-xs text-muted-foreground">Rerank 模型用于通用召回结果精排。</div>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <Button variant="ghost" size="sm" onClick={() => { setModelModalMode(null); setAvailableModels([]); }}>取消</Button>
              <Button size="sm" onClick={modelModalMode === 'add' ? addModel : saveModelEdit} disabled={!modelDraft.model}>{modelModalMode === 'add' ? '添加' : '保存'}</Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function ProviderForm({
  draft,
  setDraft,
  showApiKey,
  setShowApiKey,
  onSave,
  onCancel,
  saveLabel = '保存',
}: {
  draft: ProviderConfigView;
  setDraft: (d: ProviderConfigView) => void;
  showApiKey: boolean;
  setShowApiKey: (v: boolean) => void;
  onSave: () => void;
  onCancel: () => void;
  saveLabel?: string;
}) {
  const handleProtocolChange = (protocol: string) => {
    const defaultUrl = PROTOCOL_DEFAULTS[protocol] || '';
    setDraft({ ...draft, protocol, base_url: defaultUrl });
  };

  return (
    <div className="space-y-2">
      <div>
        <Label className="text-xs">Base URL</Label>
        <Input
          value={draft.base_url}
          onChange={(e) => setDraft({ ...draft, base_url: e.target.value })}
          className="text-sm h-8"
          placeholder="https://api.openai.com/v1"
        />
      </div>
      <div>
        <Label className="text-xs">API Key</Label>
        <div className="relative">
          <Input
            type={showApiKey ? 'text' : 'password'}
            value={draft.api_key}
            onChange={(e) => setDraft({ ...draft, api_key: e.target.value })}
            className="text-sm h-8 pr-8"
            placeholder="sk-... 或 ${ENV_VAR}"
          />
          <button
            type="button"
            onClick={() => setShowApiKey(!showApiKey)}
            className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
          >
            {showApiKey ? <EyeOff className="w-3.5 h-3.5" /> : <Eye className="w-3.5 h-3.5" />}
          </button>
        </div>
        <p className="text-xs text-muted-foreground mt-1">支持 {'${ENV_VAR}'} 引用环境变量</p>
      </div>
      <div>
        <Label className="text-xs">超时 (毫秒)</Label>
        <Input
          type="number"
          value={draft.timeout_ms}
          onChange={(e) => setDraft({ ...draft, timeout_ms: parseInt(e.target.value) || 300000 })}
          className="text-sm h-8"
          placeholder="300000"
        />
      </div>
      <div>
        <Label className="text-xs">请求格式（协议类型）</Label>
        <Select
          value={draft.protocol || 'openai_chatcompletions'}
          onValueChange={handleProtocolChange}
        >
          <SelectTrigger className="text-sm h-8">
            <SelectValue placeholder="选择协议" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="openai_chatcompletions">OpenAI Chat Completions</SelectItem>
            <SelectItem value="anthropic">Anthropic</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <div className="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          取消
        </Button>
        <Button size="sm" onClick={onSave}>
          {saveLabel}
        </Button>
      </div>
    </div>
  );
}


// ---------------------------------------------------------------------------
// Routing 子区域
// ---------------------------------------------------------------------------

function RoutingSection({
  config,
  onChange,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
}) {
  const modelKeys = Object.keys(config.models).sort();
  const [routeSearch, setRouteSearch] = useState<Record<string, string>>({});
  const modelLabel = (modelKey: string) => {
    const model = config.models[modelKey];
    if (!model) return modelKey;
    return `${model.provider} / ${model.model}`;
  };

  // 路由槽位定义（不依赖 capabilities 列表，直接枚举）
  const routingSlots = [
    { key: 'chat', display_name: '对话' },
    { key: 'lite', display_name: '轻量文本' },
    { key: 'multimodal', display_name: '多模态' },
    { key: 'image_generation', display_name: '图片生成' },
    { key: 'video_generation', display_name: '视频生成' },
    { key: 'stt', display_name: '语音识别' },
    { key: 'tts', display_name: '语音合成' },
  ];

  // 根据 routing entry 找到对应的 models key
  const findModelKeyForRoute = (slotKey: string): string | null => {
    const entry = config.routing[slotKey];
    if (!entry) return null;
    return modelKeys.find((mk) => {
      const m = config.models[mk];
      return m && m.provider === entry.provider && m.model === entry.model;
    }) || null;
  };

  const setRoute = (slotKey: string, modelKey: string) => {
    const next = { ...config };
    const newRouting = { ...next.routing };
    if (modelKey === '__none__') {
      delete newRouting[slotKey];
    } else {
      const entry = next.models[modelKey];
      if (entry) {
        newRouting[slotKey] = { ...entry };
      }
    }
    next.routing = newRouting;
    onChange(next);
    setRouteSearch((prev) => ({ ...prev, [slotKey]: '' }));
  };

  return (
    <div className="flex flex-col h-full">
      <div className="mb-3 shrink-0">
        <h4 className="text-sm font-medium text-muted-foreground">能力路由</h4>
        <p className="text-xs text-muted-foreground mt-1">
          为对话和多媒体能力选择默认模型；Embedding 和 Rerank 在 Memory 子页中选择。
        </p>
      </div>

      <div className="space-y-2 flex-1 min-h-0 overflow-y-auto">
        {routingSlots.map((slot) => {
          const currentModelKey = findModelKeyForRoute(slot.key);
          const search = routeSearch[slot.key] || '';
          const filtered = modelKeys
            .filter((mk) => {
              const m = config.models[mk];
              if (m.capabilities.length === 0) return true;
              if (m.capabilities.includes(slot.key)) return true;
              if (slot.key === 'lite' && m.capabilities.includes('chat')) return true;
              if (slot.key === 'chat' && m.capabilities.includes('multimodal')) return true;
              return false;
            })
            .filter((mk) => {
              if (!search) return true;
              const q = search.toLowerCase();
              return mk.toLowerCase().includes(q) || modelLabel(mk).toLowerCase().includes(q);
            });
          return (
            <Card key={slot.key}>
              <CardContent className="p-3">
                <div className="flex items-center gap-4">
                  <div className="w-28 shrink-0">
                    <div className="text-sm font-medium leading-tight">{slot.display_name}</div>
                    <div className="text-xs text-muted-foreground">({slot.key})</div>
                  </div>
                  <Select
                    value={currentModelKey || '__none__'}
                    onValueChange={(v) => setRoute(slot.key, v)}
                  >
                    <SelectTrigger className="h-8 text-sm flex-1">
                      <SelectValue placeholder="-- 未配置 --" />
                    </SelectTrigger>
                    <SelectContent>
                      <div className="p-1.5 border-b" onPointerDown={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
                        <Input
                          value={search}
                          onChange={(e) => setRouteSearch((prev) => ({ ...prev, [slot.key]: e.target.value }))}
                          className="h-7 text-xs"
                          placeholder="搜索模型..."
                        />
                      </div>
                      <SelectItem value="__none__">-- 未配置 --</SelectItem>
                      {filtered.map((mk) => (
                        <SelectItem key={mk} value={mk}>{modelLabel(mk)}</SelectItem>
                      ))}
                      {filtered.length === 0 && (
                        <div className="px-2 py-3 text-xs text-muted-foreground text-center">无匹配模型</div>
                      )}
                    </SelectContent>
                  </Select>
                </div>
              </CardContent>
            </Card>
          );
        })}
      </div>

      {modelKeys.length === 0 && (
        <p className="text-xs text-muted-foreground mt-3">
          请先在模型页中添加模型定义，然后回来配置路由
        </p>
      )}
    </div>
  );
}

// ============================================================================
// MCP 设置组件
// ============================================================================

function McpSettings() {
  const [servers, setServers] = useState<McpServer[]>([]);
  const [healthMap, setHealthMap] = useState<Record<string, { healthy: boolean; tool_count: number; last_error?: string; server_version?: string }>>({});
  const [isLoading, setIsLoading] = useState(false);
  const [serverModalMode, setServerModalMode] = useState<'add' | 'edit' | null>(null);
  const [editingServerName, setEditingServerName] = useState<string | null>(null);
  const [newServer, setNewServer] = useState({
    name: '',
    transport: 'stdio' as McpTransportDraft,
    command: '',
    args: '',
    endpoint: '',
    authHeader: '',
    headers: '',
    env: '',
  });
  const { showSuccess, showError } = useToast();

  // 立即拉取 server 列表（config 读，瞬时），不等待探测。健康状态单独异步更新。
  const loadServers = async () => {
    setIsLoading(true);
    try {
      const data = await api.getMcpServers();
      setServers(data);
    } catch (error) {
      console.error('加载 MCP 服务器失败:', error);
      showError('加载失败', '无法加载 MCP 服务器列表');
    } finally {
      setIsLoading(false);
    }
    // 健康状态异步刷新，不阻塞列表渲染
    refreshHealth();
  };

  const refreshHealth = async () => {
    try {
      const health = await api.getMcpHealth();
      const map: typeof healthMap = {};
      for (const s of health) {
        map[s.name] = { healthy: s.healthy, tool_count: s.tool_count, last_error: s.last_error, server_version: s.server_version };
      }
      setHealthMap(map);
    } catch (error) {
      console.error('加载 MCP 健康状态失败:', error);
    }
  };

  // 探测单个 server 后刷新健康状态（用于添加/编辑/行内重试）
  const probeServer = async (name: string) => {
    try {
      await api.probeMcpServer(name);
    } catch (error) {
      console.error('探测 MCP 服务器失败:', error);
    } finally {
      refreshHealth();
    }
  };

  useEffect(() => {
    loadServers();
  }, []);

  const resetDraft = () => {
    setNewServer({
      name: '',
      transport: 'stdio',
      command: '',
      args: '',
      endpoint: '',
      authHeader: '',
      headers: '',
      env: '',
    });
  };

  const closeServerModal = () => {
    setServerModalMode(null);
    setEditingServerName(null);
    resetDraft();
  };

  const openEditServer = (server: McpServer) => {
    setEditingServerName(server.name);
    setNewServer({
      name: server.name,
      transport: (server.transport === 'http' ? 'http' : 'stdio') as McpTransportDraft,
      command: server.command,
      args: server.args.join(' '),
      endpoint: server.endpoint,
      authHeader: server.auth_header,
      headers: formatKeyValue(server.headers),
      env: formatKeyValue(server.env),
    });
    setServerModalMode('edit');
  };

  const handleAddServer = async () => {
    try {
      const isStdio = newServer.transport === 'stdio';
      const request = isStdio
        ? {
            name: newServer.name.trim(),
            transport: newServer.transport,
            command: newServer.command.trim(),
            args: parseListArgs(newServer.args),
            env: parseKeyValueText(newServer.env, '环境变量'),
          }
        : {
            name: newServer.name.trim(),
            transport: newServer.transport,
            command: '',
            args: [],
            endpoint: newServer.endpoint.trim(),
            authHeader: newServer.authHeader.trim() || undefined,
            headers: parseKeyValueText(newServer.headers, 'Header'),
          };

      await api.registerMcpServer(request);
      showSuccess('添加成功', `MCP 服务器 "${newServer.name}" 已添加`);
      const addedName = newServer.name.trim();
      closeServerModal();
      await loadServers();
      // 异步探测新 server，完成后刷新该行健康状态（不阻塞列表渲染）
      probeServer(addedName);
    } catch (error) {
      console.error('添加 MCP 服务器失败:', error);
      showError('添加失败', errorMessage(error));
    }
  };

  const handleUpdateServer = async () => {
    if (!editingServerName) return;
    try {
      const isStdio = newServer.transport === 'stdio';
      const request = isStdio
        ? {
            name: editingServerName,
            transport: newServer.transport,
            command: newServer.command.trim(),
            args: parseListArgs(newServer.args),
            env: parseKeyValueText(newServer.env, '环境变量'),
          }
        : {
            name: editingServerName,
            transport: newServer.transport,
            command: '',
            args: [],
            endpoint: newServer.endpoint.trim(),
            authHeader: newServer.authHeader.trim() || undefined,
            headers: parseKeyValueText(newServer.headers, 'Header'),
          };

      await api.updateMcpServer(editingServerName, request);
      showSuccess('保存成功', `MCP 服务器 "${editingServerName}" 已更新`);
      const updatedName = editingServerName;
      closeServerModal();
      await loadServers();
      // 编辑后重新探测该 server（配置可能影响握手）
      probeServer(updatedName);
    } catch (error) {
      console.error('更新 MCP 服务器失败:', error);
      showError('保存失败', errorMessage(error));
    }
  };

  const handleRemoveServer = async (name: string) => {
    try {
      await api.removeMcpServer(name);
      showSuccess('删除成功', `MCP 服务器 "${name}" 已删除`);
      loadServers();
    } catch (error) {
      console.error('删除 MCP 服务器失败:', error);
      showError('删除失败', '无法删除 MCP 服务器');
    }
  };

  const handleToggleEnabled = async (name: string, enabled: boolean) => {
    try {
      await api.setMcpServerEnabled(name, enabled);
      showSuccess('状态更新', `MCP 服务器 "${name}" 已${enabled ? '启用' : '禁用'}`);
      loadServers();
    } catch (error) {
      console.error('切换 MCP 服务器状态失败:', error);
      showError('操作失败', '无法更新 MCP 服务器状态');
    }
  };

  // 刷新：对全部 server 并发重探，全部完成后统一读一次健康状态。真正重置探测状态。
  const handleRefresh = async () => {
    setIsLoading(true);
    try {
      const data = await api.getMcpServers();
      setServers(data);
      // 并发探测所有 server（后端单 server 探测互不阻塞）
      await Promise.allSettled(data.map((s) => api.probeMcpServer(s.name)));
      await refreshHealth();
    } catch (error) {
      console.error('刷新 MCP 服务器失败:', error);
      showError('刷新失败', '无法刷新 MCP 服务器状态');
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">MCP 服务器</h3>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={handleRefresh}>
            <RefreshCw className="w-4 h-4 mr-2" />
            刷新
          </Button>
          <Button size="sm" onClick={() => { resetDraft(); setServerModalMode('add'); }}>
            <Plus className="w-4 h-4 mr-2" />
            添加服务器
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : servers.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">暂无 MCP 服务器</div>
      ) : (
        <div className="space-y-2">
          {servers.map((server) => {
            const health = healthMap[server.name];
            const isHealthy = health?.healthy ?? true;
            const hasHealth = health !== undefined;
            const isRemote = server.transport === 'http';
            const serverTarget = isRemote
              ? server.endpoint || server.command || '(未配置 endpoint)'
              : `${server.command} ${server.args.join(' ')}`.trim();
            return (
            <Card key={server.name}>
              <CardContent className="p-4 flex items-center justify-between">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <span className="font-medium">{server.name}</span>
                    <Badge variant="outline" className="text-xs">
                      {isRemote ? 'HTTP/SSE' : server.transport}
                    </Badge>
                    <Badge variant={server.enabled ? 'default' : 'secondary'}>
                      {server.enabled ? '已启用' : '已禁用'}
                    </Badge>
                    {server.enabled && hasHealth && (
                      <Badge variant={isHealthy ? 'outline' : 'destructive'} className="text-xs">
                        {isHealthy ? `健康 (${health.tool_count} 工具)` : '不可达'}
                      </Badge>
                    )}
                    {health?.server_version && (
                      <Badge variant="outline" className="text-xs">v{health.server_version}</Badge>
                    )}
                  </div>
                  <div className="text-sm text-muted-foreground mt-1 truncate">
                    {serverTarget}
                  </div>
                  {isRemote && (server.auth_header || server.headers) && (
                    <div className="text-xs text-muted-foreground mt-1">
                      {server.auth_header ? '已设置认证头' : ''}
                      {server.auth_header && server.headers ? ' · ' : ''}
                      {server.headers ? `${Object.keys(server.headers).length} 个自定义 Header` : ''}
                    </div>
                  )}
                  {server.enabled && hasHealth && !isHealthy && health.last_error && (
                    <div className="text-xs text-destructive mt-1 truncate" title={health.last_error}>
                      {health.last_error}
                    </div>
                  )}
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  {server.enabled && hasHealth && !isHealthy && (
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-8 w-8"
                      onClick={() => probeServer(server.name)}
                      title="重新探测"
                    >
                      <RefreshCw className="w-4 h-4" />
                    </Button>
                  )}
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => openEditServer(server)}
                    title="编辑配置"
                  >
                    <Edit2 className="w-4 h-4" />
                  </Button>
                  <Switch
                    checked={server.enabled}
                    onCheckedChange={(checked) => handleToggleEnabled(server.name, checked)}
                  />
                  <Button
                    variant="ghost"
                    size="icon"
                    className="hover:bg-destructive/20 hover:text-destructive"
                    onClick={() => handleRemoveServer(server.name)}
                    title="删除"
                  >
                    <Trash2 className="w-4 h-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
            );
          })}
        </div>
      )}

      {/* 添加/编辑服务器对话框 */}
      {serverModalMode !== null && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="max-w-md w-full mx-4">
            <CardContent className="p-6">
              <h3 className="text-lg font-medium mb-4">
                {serverModalMode === 'add'
                  ? '添加 MCP 服务器'
                  : `编辑 MCP 服务器: ${editingServerName}`}
              </h3>
              <div className="space-y-4">
                <div>
                  <Label htmlFor="serverName">名称</Label>
                  <Input
                    id="serverName"
                    value={newServer.name}
                    onChange={(e) => setNewServer({ ...newServer, name: e.target.value })}
                    placeholder="my-mcp-server"
                    disabled={serverModalMode === 'edit'}
                  />
                </div>
                <div>
                  <Label htmlFor="serverTransport">连接方式</Label>
                  <Select
                    value={newServer.transport}
                    onValueChange={(value) => setNewServer({ ...newServer, transport: value as McpTransportDraft })}
                  >
                    <SelectTrigger id="serverTransport">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="stdio">stdio / npx</SelectItem>
                      <SelectItem value="http">HTTP/SSE 端点</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                {newServer.transport === 'stdio' ? (
                  <>
                    <div>
                      <Label htmlFor="serverCommand">命令</Label>
                      <Input
                        id="serverCommand"
                        value={newServer.command}
                        onChange={(e) => setNewServer({ ...newServer, command: e.target.value })}
                        placeholder="npx"
                      />
                    </div>
                    <div>
                      <Label htmlFor="serverArgs">参数（空格分隔）</Label>
                      <Input
                        id="serverArgs"
                        value={newServer.args}
                        onChange={(e) => setNewServer({ ...newServer, args: e.target.value })}
                        placeholder="-y @modelcontextprotocol/server-filesystem"
                      />
                    </div>
                    <div>
                      <Label htmlFor="serverEnv">环境变量（逗号或换行分隔，格式：KEY=VALUE）</Label>
                      <Textarea
                        id="serverEnv"
                        value={newServer.env}
                        onChange={(e) => setNewServer({ ...newServer, env: e.target.value })}
                        placeholder="PATH=/usr/bin&#10;NODE_ENV=production"
                        rows={3}
                      />
                    </div>
                  </>
                ) : (
                  <>
                    <div>
                      <Label htmlFor="serverEndpoint">Endpoint</Label>
                      <Input
                        id="serverEndpoint"
                        value={newServer.endpoint}
                        onChange={(e) => setNewServer({ ...newServer, endpoint: e.target.value })}
                        placeholder="https://example.com/mcp"
                      />
                    </div>
                    <div>
                      <Label htmlFor="serverAuthHeader">认证 Header</Label>
                      <Input
                        id="serverAuthHeader"
                        value={newServer.authHeader}
                        onChange={(e) => setNewServer({ ...newServer, authHeader: e.target.value })}
                        placeholder="Bearer sk-..."
                      />
                    </div>
                    <div>
                      <Label htmlFor="serverHeaders">自定义 Headers（逗号或换行分隔）</Label>
                      <Textarea
                        id="serverHeaders"
                        value={newServer.headers}
                        onChange={(e) => setNewServer({ ...newServer, headers: e.target.value })}
                        placeholder="X-API-Key=xxx&#10;X-Client: tiangong"
                        rows={3}
                      />
                    </div>
                  </>
                )}
              </div>
              <div className="flex justify-end gap-2 mt-6">
                <Button variant="ghost" onClick={closeServerModal}>
                  取消
                </Button>
                <Button
                  onClick={serverModalMode === 'add' ? handleAddServer : handleUpdateServer}
                  disabled={
                    (serverModalMode === 'add' && !newServer.name.trim())
                    || (newServer.transport === 'stdio'
                      ? !newServer.command.trim()
                      : !newServer.endpoint.trim())
                  }
                >
                  {serverModalMode === 'add' ? '添加' : '保存'}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Skill 设置组件
// ============================================================================

function SkillSettings() {
  const [skills, setSkills] = useState<Skill[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  // skill env 编辑
  const [editSkillEnvId, setEditSkillEnvId] = useState<string | null>(null);
  const [editSkillEnvValues, setEditSkillEnvValues] = useState<Record<string, string>>({});
  const [skillDetail, setSkillDetail] = useState<SkillDetail | null>(null);
  const { showSuccess, showError } = useToast();

  const loadSkills = async () => {
    setIsLoading(true);
    try {
      const data = await api.getSkills();
      setSkills(data);
    } catch (error) {
      console.error('加载 Skills 失败:', error);
      showError('加载失败', '无法加载 Skills 列表');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadSkills();
  }, []);


  const handleRemoveSkill = async (id: string) => {
    try {
      await api.removeSkill(id);
      showSuccess('删除成功', 'Skill 已删除');
      loadSkills();
    } catch (error) {
      console.error('删除 Skill 失败:', error);
      showError('删除失败', '无法删除 Skill');
    }
  };

  const handleToggleEnabled = async (id: string, enabled: boolean) => {
    try {
      await api.setSkillEnabled(id, enabled);
      showSuccess('状态更新', `Skill 已${enabled ? '启用' : '禁用'}`);
      loadSkills();
    } catch (error) {
      console.error('切换 Skill 状态失败:', error);
      showError('操作失败', '无法更新 Skill 状态');
    }
  };

  const handleRefreshSkills = async () => {
    try {
      const msg = await api.refreshSkills();
      showSuccess('已刷新', msg);
      loadSkills();
    } catch (error) {
      console.error('刷新 Skill 失败:', error);
      showError('刷新失败', `${error}`);
    }
  };

  const handleShowSkillDetail = async (id: string) => {
    try {
      const detail = await api.getSkillDetail(id);
      setSkillDetail(detail);
    } catch (error) {
      console.error('读取 Skill 详情失败:', error);
      showError('读取失败', `${error}`);
    }
  };

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">Skills</h3>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={handleRefreshSkills}>
            <RefreshCw className="w-4 h-4 mr-2" />
            刷新
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : skills.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">暂无已安装的 Skills</div>
      ) : (
        <div className="space-y-2">
          {skills.map((skill) => (
            <Card key={skill.id}>
              <CardContent className="p-4 flex items-center justify-between">
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{skill.name}</span>
                    <Badge variant={skill.enabled ? 'default' : 'secondary'}>
                      {skill.enabled ? '已启用' : '已禁用'}
                    </Badge>
                    <Badge variant="outline">v{skill.version}</Badge>
                  </div>
                  {skill.description && (
                    <div className="text-sm text-muted-foreground mt-1">{skill.description}</div>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => handleShowSkillDetail(skill.id)}
                    title="查看详情"
                  >
                    <Info className="w-4 h-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={async () => {
                      try {
                        const env = await api.getSkillEnv(skill.id);
                        setEditSkillEnvId(skill.id);
                        setEditSkillEnvValues(env);
                      } catch (e) {
                        setEditSkillEnvId(skill.id);
                        setEditSkillEnvValues({});
                      }
                    }}
                    title="编辑环境变量"
                  >
                    <KeyRound className="w-4 h-4" />
                  </Button>
                  <Switch
                    checked={skill.enabled}
                    onCheckedChange={(checked) => handleToggleEnabled(skill.id, checked)}
                  />
                  <Button
                    variant="ghost"
                    size="icon"
                    className="hover:bg-destructive/20 hover:text-destructive"
                    onClick={() => handleRemoveSkill(skill.id)}
                    title="删除"
                  >
                    <Trash2 className="w-4 h-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* Skill 环境变量编辑 */}
      <EnvEditDialog
        open={editSkillEnvId !== null}
        title={`编辑环境变量: ${editSkillEnvId}`}
        values={editSkillEnvValues}
        onChange={setEditSkillEnvValues}
        onSave={async () => {
          if (!editSkillEnvId) return;
          try {
            await api.setSkillEnv(editSkillEnvId, editSkillEnvValues);
            showSuccess('已保存', '环境变量已更新');
            setEditSkillEnvId(null);
          } catch (error) {
            showError('保存失败', `${error}`);
          }
        }}
        onCancel={() => setEditSkillEnvId(null)}
      />

      <Dialog open={skillDetail !== null} onOpenChange={(open) => !open && setSkillDetail(null)}>
        <DialogContent className="max-w-2xl max-h-[80vh] overflow-hidden flex flex-col">
          <DialogHeader>
            <DialogTitle>{skillDetail?.name || 'Skill 详情'}</DialogTitle>
          </DialogHeader>
          {skillDetail && (
            <div className="space-y-3 overflow-auto pr-1">
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <Badge variant="outline">{skillDetail.id}</Badge>
                <Badge variant={skillDetail.enabled ? 'default' : 'secondary'}>
                  {skillDetail.enabled ? '已启用' : '已禁用'}
                </Badge>
                <Badge variant="outline">v{skillDetail.version}</Badge>
                <Badge variant="outline">{skillDetail.entry}</Badge>
              </div>
              {skillDetail.description && (
                <p className="text-sm text-muted-foreground">{skillDetail.description}</p>
              )}
              <pre className="whitespace-pre-wrap rounded-md bg-muted/50 p-3 text-xs leading-relaxed">
                {skillDetail.readme}
              </pre>
            </div>
          )}
        </DialogContent>
      </Dialog>

    </div>
  );
}

// ============================================================================
// Server 设置组件
// ============================================================================

function ServerSettings() {
  const [subTab, setSubTab] = useState<'config' | 'webhooks'>('config');

  return (
    <div className="flex flex-col h-full">
      <div className="flex gap-1 shrink-0 p-4 pb-0">
        {(['config', 'webhooks'] as const).map((tab) => (
          <button
            key={tab}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              subTab === tab
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setSubTab(tab)}
          >
            {tab === 'config' ? 'Server 配置' : 'Webhook'}
          </button>
        ))}
      </div>
      <div className="flex-1 min-h-0 overflow-y-auto">
        {subTab === 'config' && <ServerConfigPanel />}
        {subTab === 'webhooks' && <ServerWebhookPanel />}
      </div>
    </div>
  );
}

function ServerWebhookPanel() {
  const [serverRunning, setServerRunning] = useState(false);

  useEffect(() => {
    const check = async () => {
      try {
        const cfg = await api.getServerConfig();
        setServerRunning(cfg.running);
      } catch { /* ignore */ }
    };
    check();
    const timer = window.setInterval(check, 5000);
    return () => window.clearInterval(timer);
  }, []);

  return <WebhookPanel serverRunning={serverRunning} />;
}

function ServerConfigPanel() {
  const [config, setConfig] = useState<ServerConfig>({
    host: '127.0.0.1',
    port: 8080,
    auth_token_masked: '',
    running: false,
  });
  const [editHost, setEditHost] = useState('');
  const [editPort, setEditPort] = useState('8080');
  const [editAuthToken, setEditAuthToken] = useState('');
  const [isLoading, setIsLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isToggling, setIsToggling] = useState(false);
  const { showSuccess, showError } = useToast();

  const loadConfig = async (showLoading = true) => {
    if (showLoading) setIsLoading(true);
    try {
      const cfg = await api.getServerConfig();
      setConfig(cfg);
      if (showLoading) {
        setEditHost(cfg.host);
        setEditPort(String(cfg.port));
        setEditAuthToken('');
      }
    } catch (error) {
      console.error('加载 Server 配置失败:', error);
      if (showLoading) {
        showError('加载失败', '无法加载 Server 配置');
      }
    } finally {
      if (showLoading) setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
    const timer = window.setInterval(() => {
      loadConfig(false);
    }, 3000);
    return () => window.clearInterval(timer);
  }, []);

  const handleSave = async () => {
    setIsSaving(true);
    try {
      const port = parseInt(editPort, 10);
      if (isNaN(port) || port < 1 || port > 65535) {
        showError('端口无效', '请输入 1-65535 之间的端口号');
        return;
      }
      const authToken = editAuthToken.trim() || undefined;
      await api.setServerConfig(editHost, port, authToken);
      showSuccess('保存成功', 'Server 配置已更新');
      loadConfig();
    } catch (error) {
      console.error('保存 Server 配置失败:', error);
      showError('保存失败', '无法保存 Server 配置');
    } finally {
      setIsSaving(false);
    }
  };

  const saveCurrentConfig = async () => {
    const port = parseInt(editPort, 10);
    if (isNaN(port) || port < 1 || port > 65535) {
      showError('端口无效', '请输入 1-65535 之间的端口号');
      return false;
    }
    const authToken = editAuthToken.trim() || undefined;
    await api.setServerConfig(editHost, port, authToken);
    return true;
  };

  const handleToggleServer = async (enabled: boolean) => {
    setIsToggling(true);
    try {
      if (enabled) {
        const saved = await saveCurrentConfig();
        if (!saved) return;
        const message = await api.startServer();
        showSuccess('启动成功', message);
      } else {
        const message = await api.stopServer();
        showSuccess('已停止', message);
      }
      await loadConfig();
    } catch (error) {
      console.error('切换 Server 状态失败:', error);
      showError(
        enabled ? '启动失败' : '停止失败',
        error instanceof Error ? error.message : String(error || '无法切换 Server 运行状态')
      );
    } finally {
      setIsToggling(false);
    }
  };

  return (
    <div className="space-y-4 p-4">
      {isLoading ? (
        <div className="flex items-center justify-center py-8">
          <Loader2 className="w-6 h-6 animate-spin text-primary mr-2" />
          <span className="text-sm text-muted-foreground">加载配置中...</span>
        </div>
      ) : (
        <>
          {/* 运行状态 */}
          <div className="flex items-center justify-between gap-3 mb-4">
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">状态：</span>
              <Badge variant={config.running ? 'default' : 'secondary'}>
                {config.running ? '运行中' : '未运行'}
              </Badge>
            </div>
            <div className="flex items-center gap-2">
              {isToggling && <Loader2 className="w-4 h-4 animate-spin text-muted-foreground" />}
              <Switch
                checked={config.running}
                disabled={isSaving || isToggling}
                onCheckedChange={handleToggleServer}
              />
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="serverHost">监听地址</Label>
            <Input
              id="serverHost"
              value={editHost}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditHost(e.target.value)}
              placeholder="127.0.0.1"
              disabled={isSaving || isToggling || config.running}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="serverPort">端口</Label>
            <Input
              id="serverPort"
              type="number"
              value={editPort}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditPort(e.target.value)}
              placeholder="8080"
              disabled={isSaving || isToggling || config.running}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="serverAuthToken">认证 Token</Label>
            <Input
              id="serverAuthToken"
              type="password"
              value={editAuthToken}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditAuthToken(e.target.value)}
              placeholder={config.auth_token_masked || '留空表示不鉴权'}
              disabled={isSaving || isToggling || config.running}
            />
            <p className="text-xs text-muted-foreground">
              当前: {config.auth_token_masked}（留空则保持不变）
            </p>
          </div>

          <div className="flex justify-end gap-2 pt-4">
            <Button onClick={handleSave} disabled={isSaving || isToggling || config.running}>
              {isSaving ? (
                <>
                  <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                  保存中...
                </>
              ) : (
                '保存'
              )}
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

// ============================================================================
// 关于与更新组件
// ============================================================================

function formatBytes(value: number) {
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let size = value;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }
  return `${size.toFixed(unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

function AppUpdateSettings() {
  const storeUpdate = useStore((s) => s.updateAvailable);
  const [currentVersion, setCurrentVersion] = useState('');
  const [availableUpdate, setAvailableUpdate] = useState<{
    version: string;
    currentVersion: string;
    date?: string;
    body?: string;
  } | null>(null);
  const [isChecking, setIsChecking] = useState(false);
  const [isInstalling, setIsInstalling] = useState(false);
  const [downloadedBytes, setDownloadedBytes] = useState(0);
  const [contentLength, setContentLength] = useState<number | undefined>();
  const updateRef = useRef<Update | null>(null);
  const { showSuccess, showError, showInfo } = useToast();

  useEffect(() => {
    let mounted = true;
    import('@tauri-apps/api/app')
      .then(({ getVersion }) => getVersion())
      .then((version) => {
        if (mounted) setCurrentVersion(version);
      })
      .catch((error) => {
        console.error('读取应用版本失败:', error);
        if (mounted) setCurrentVersion('未知');
      });
    return () => {
      mounted = false;
      updateRef.current?.close().catch(() => undefined);
      updateRef.current = null;
    };
  }, []);

  // 从全局 store 预填充更新信息并静默获取可用的 Update 对象
  useEffect(() => {
    if (!storeUpdate || updateRef.current) return;
    let mounted = true;
    setAvailableUpdate({
      version: storeUpdate.version,
      currentVersion: '',
      date: storeUpdate.date,
      body: storeUpdate.body,
    });
    (async () => {
      try {
        const { check } = await import('@tauri-apps/plugin-updater');
        const update = await check({ timeout: 30000 });
        if (!mounted) return;
        await updateRef.current?.close().catch(() => {});
        updateRef.current = update;
        if (update) {
          setAvailableUpdate({
            version: update.version,
            currentVersion: update.currentVersion,
            date: update.date,
            body: update.body,
          });
        }
      } catch {
        // 静默失败，用户可手动重试
      }
    })();
    return () => { mounted = false; };
  }, [storeUpdate]);

  const handleCheckUpdate = async () => {
    setIsChecking(true);
    setAvailableUpdate(null);
    setDownloadedBytes(0);
    setContentLength(undefined);
    try {
      const { check } = await import('@tauri-apps/plugin-updater');
      const update = await check({ timeout: 30000 });
      await updateRef.current?.close().catch(() => undefined);
      updateRef.current = update;

      if (!update) {
        showInfo('已是最新版本', '当前没有可用更新');
        return;
      }

      setAvailableUpdate({
        version: update.version,
        currentVersion: update.currentVersion,
        date: update.date,
        body: update.body,
      });
      showSuccess('发现新版本', `可更新到 ${update.version}`);
    } catch (error) {
      console.error('检查更新失败:', error);
      showError('检查失败', `${error}`);
    } finally {
      setIsChecking(false);
    }
  };

  const handleInstallUpdate = async () => {
    const update = updateRef.current;
    if (!update) {
      showError('没有可安装更新', '请先检查更新');
      return;
    }

    setIsInstalling(true);
    setDownloadedBytes(0);
    setContentLength(undefined);
    try {
      let downloaded = 0;
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === 'Started') {
          downloaded = 0;
          setDownloadedBytes(0);
          setContentLength(event.data.contentLength);
        }
        if (event.event === 'Progress') {
          downloaded += event.data.chunkLength;
          setDownloadedBytes(downloaded);
        }
      });
      showSuccess('更新已安装', '应用将重新启动');
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    } catch (error) {
      console.error('安装更新失败:', error);
      showError('安装失败', `${error}`);
    } finally {
      setIsInstalling(false);
    }
  };

  const progressText = contentLength
    ? `${formatBytes(downloadedBytes)} / ${formatBytes(contentLength)}`
    : downloadedBytes > 0
      ? formatBytes(downloadedBytes)
      : '';

  return (
    <div className="space-y-4 p-4">
      <div className="flex items-center justify-between gap-4">
        <h3 className="text-lg font-medium">关于与更新</h3>
        <a
          href="https://github.com/silent-rs/silent-Tiangong"
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          title="在 GitHub 上查看本项目"
        >
          <Github className="h-4 w-4" />
          <span>GitHub</span>
        </a>
      </div>

      <Card>
        <CardContent className="space-y-4 p-4">
          <div className="flex items-center justify-between gap-4">
            <div>
              <div className="text-sm text-muted-foreground">当前版本</div>
              <div className="mt-1 text-2xl font-semibold">{currentVersion || '读取中...'}</div>
            </div>
            <Badge variant="outline">GitHub / OSS</Badge>
          </div>

          {availableUpdate ? (
            <div className="rounded-md border p-3 text-sm">
              <div className="font-medium">发现新版本 {availableUpdate.version}</div>
              <div className="mt-1 text-muted-foreground">
                当前版本 {availableUpdate.currentVersion}
                {availableUpdate.date ? ` · ${availableUpdate.date}` : ''}
              </div>
              {availableUpdate.body && (
                <div className="mt-3 whitespace-pre-wrap text-muted-foreground">
                  {availableUpdate.body}
                </div>
              )}
            </div>
          ) : (
            <div className="rounded-md border p-3 text-sm text-muted-foreground">
              更新检查会依次从 GitHub Release 和阿里云 OSS 获取最新版本信息。
            </div>
          )}

          {isInstalling && progressText && (
            <div className="text-sm text-muted-foreground">正在下载：{progressText}</div>
          )}

          <div className="flex flex-wrap justify-end gap-2">
            <Button variant="outline" onClick={handleCheckUpdate} disabled={isChecking || isInstalling}>
              {isChecking ? (
                <>
                  <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                  检查中...
                </>
              ) : (
                <>
                  <RefreshCw className="w-4 h-4 mr-2" />
                  检查更新
                </>
              )}
            </Button>
            <Button onClick={handleInstallUpdate} disabled={!updateRef.current || isChecking || isInstalling}>
              {isInstalling ? (
                <>
                  <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                  安装中...
                </>
              ) : (
                '下载并安装'
              )}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

// ============================================================================
// 通用环境变量编辑对话框
// ============================================================================

function EnvEditDialog({
  open,
  title,
  values,
  onChange,
  onSave,
  onCancel,
}: {
  open: boolean;
  title: string;
  values: Record<string, string>;
  onChange: (v: Record<string, string>) => void;
  onSave: () => Promise<void>;
  onCancel: () => void;
}) {
  const [newKey, setNewKey] = useState('');
  const [isSaving, setIsSaving] = useState(false);

  const addKey = () => {
    const key = newKey.trim();
    if (key && !(key in values)) {
      onChange({ ...values, [key]: '' });
      setNewKey('');
    }
  };

  const removeKey = (key: string) => {
    const next = { ...values };
    delete next[key];
    onChange(next);
  };

  const handleSave = async () => {
    setIsSaving(true);
    try { await onSave(); } finally { setIsSaving(false); }
  };

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <Card className="max-w-md w-full mx-4 max-h-[70vh] flex flex-col">
        <CardContent className="p-6 flex flex-col overflow-hidden">
          <h3 className="text-lg font-medium mb-4 shrink-0">{title}</h3>
          <div className="flex-1 overflow-y-auto space-y-3">
            {Object.entries(values).map(([key, value]) => (
              <div key={key} className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <Label className="text-xs font-mono">{key}</Label>
                  <Input
                    type="password"
                    value={value}
                    onChange={(e) => onChange({ ...values, [key]: e.target.value })}
                    placeholder="输入值"
                    className="text-sm h-8 mt-1"
                  />
                </div>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 shrink-0 mt-5 hover:bg-destructive/20 hover:text-destructive"
                  onClick={() => removeKey(key)}
                  title="移除"
                >
                  <Trash2 className="w-3 h-3" />
                </Button>
              </div>
            ))}
            {Object.keys(values).length === 0 && (
              <div className="text-xs text-muted-foreground text-center py-2">暂无环境变量</div>
            )}
          </div>
          <div className="flex items-center gap-2 mt-4 shrink-0">
            <Input
              value={newKey}
              onChange={(e) => setNewKey(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && addKey()}
              placeholder="添加新变量名"
              className="text-sm h-8 flex-1"
            />
            <Button size="sm" variant="outline" onClick={addKey} disabled={!newKey.trim()}>
              <Plus className="w-3 h-3 mr-1" />添加
            </Button>
          </div>
          <div className="flex justify-end gap-2 mt-4 shrink-0">
            <Button variant="ghost" onClick={onCancel}>取消</Button>
            <Button onClick={handleSave} disabled={isSaving}>
              {isSaving ? '保存中...' : '保存'}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
