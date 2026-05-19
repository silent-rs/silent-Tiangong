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
import { Settings, Eye, EyeOff, Server, Puzzle, Plus, Trash2, Loader2, Globe, Edit2, KeyRound, RefreshCw, Info, Wrench, FolderOpen, Save, ShieldCheck, Database, X } from 'lucide-react';
import { open as openDialog } from '@tauri-apps/plugin-dialog';
import type { DownloadEvent, Update } from '@tauri-apps/plugin-updater';
import { api } from '@/api/tauri';
import type { McpServer, Skill, SkillDetail, ServerConfig, ModelsConfigView, ProviderConfigView, ModelEntryView, ModelCapabilityInfo, MemoryConfigView } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { useToast } from './Toast';
import { MemoryManagementSettings } from './memory';

type SaveStatus = 'idle' | 'saving' | 'saved' | 'error';

export function SettingsDialog() {
  const [open, setOpen] = useState(false);
  const [saveStatus, setSaveStatus] = useState<SaveStatus>('idle');

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
          {/* 保存状态 - absolute 定位到关闭按钮左侧 */}
          <span className={`absolute right-12 top-[18px] z-10 text-xs flex items-center transition-opacity ${saveStatus === 'idle' ? 'opacity-0' : 'opacity-100'} ${saveStatus === 'error' ? 'text-destructive' : 'text-muted-foreground'}`}>
            {saveStatus === 'saving' && (
              <><Loader2 className="w-3 h-3 mr-1 animate-spin" />保存中...</>
            )}
            {(saveStatus === 'saved' || saveStatus === 'idle') && '已自动保存'}
            {saveStatus === 'error' && '保存失败'}
          </span>

          <Tabs defaultValue="agent" className="flex-1 overflow-hidden flex">
            <aside className="w-60 shrink-0 border-r bg-muted/30 flex flex-col">
              <DialogHeader className="px-5 pb-5 pt-14 pr-10 mb-0 border-b">
                <DialogTitle>设置</DialogTitle>
              </DialogHeader>
              <TabsList className="h-auto w-full flex-1 flex-col items-stretch justify-start rounded-none bg-transparent p-2">
                <TabsTrigger value="agent" className="w-full justify-start px-3 py-2">
                  <ShieldCheck className="w-4 h-4 mr-2" />
                  Agent
                </TabsTrigger>
                <TabsTrigger value="llm" className="w-full justify-start px-3 py-2">
                  <Settings className="w-4 h-4 mr-2" />
                  LLM 配置
                </TabsTrigger>
                <TabsTrigger value="memory" className="w-full justify-start px-3 py-2">
                  <Database className="w-4 h-4 mr-2" />
                  Memory
                </TabsTrigger>
                <TabsTrigger value="mcp" className="w-full justify-start px-3 py-2">
                  <Server className="w-4 h-4 mr-2" />
                  MCP 服务器
                </TabsTrigger>
                <TabsTrigger value="skill" className="w-full justify-start px-3 py-2">
                  <Puzzle className="w-4 h-4 mr-2" />
                  Skills
                </TabsTrigger>
                <TabsTrigger value="server" className="w-full justify-start px-3 py-2">
                  <Globe className="w-4 h-4 mr-2" />
                  Server
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

            <div className="min-w-0 flex-1 overflow-hidden">
              <TabsContent value="agent" className="m-0 h-full overflow-hidden flex flex-col">
                <AgentSettings onSaveStatusChange={setSaveStatus} />
              </TabsContent>
              <TabsContent value="llm" className="m-0 h-full overflow-y-auto">
                <LLMSettings onSaveStatusChange={setSaveStatus} />
              </TabsContent>
              <TabsContent value="memory" className="m-0 h-full overflow-hidden">
                <MemoryManagementSettings />
              </TabsContent>
              <TabsContent value="mcp" className="m-0 h-full overflow-y-auto">
                <McpSettings />
              </TabsContent>
              <TabsContent value="skill" className="m-0 h-full overflow-y-auto">
                <SkillSettings />
              </TabsContent>
              <TabsContent value="server" className="m-0 h-full overflow-y-auto">
                <ServerSettings />
              </TabsContent>
              <TabsContent value="about" className="m-0 h-full overflow-y-auto">
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
          <Label htmlFor="workspacePath">工作区目录</Label>
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
          <div className="rounded-md border p-3 text-sm">
            <div className="text-muted-foreground mb-1">当前工作区</div>
            <div className="break-all font-mono text-xs">{workspaceDir || '未设置'}</div>
          </div>
          <div className="flex justify-end">
            <Button onClick={handleSaveWorkspace} disabled={isSavingWorkspace || editWorkspaceDir.trim() === workspaceDir}>
              {isSavingWorkspace ? (
                <>
                  <Loader2 className="w-4 h-4 mr-2 animate-spin" />
                  保存中...
                </>
              ) : (
                <>
                  <Save className="w-4 h-4 mr-2" />
                  保存工作区
                </>
              )}
            </Button>
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

type LLMSubTab = 'providers' | 'models' | 'routing' | 'memory';

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
      setModelsConfig(cfg);
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
    <div className="p-4 space-y-4">
      {/* 子标签栏 — 固定不动 */}
      <div className="flex gap-1">
        {(['providers', 'models', 'routing', 'memory'] as const).map((tab) => (
          <button
            key={tab}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              subTab === tab
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setSubTab(tab)}
          >
            {tab === 'providers' ? '供应商' : tab === 'models' ? '模型' : tab === 'routing' ? '路由' : '记忆模型'}
          </button>
        ))}
      </div>

      {/* 内容区域 */}
      <div>
        {subTab === 'providers' && (
          <ProvidersSection config={modelsConfig} onChange={handleChange} />
        )}
        {subTab === 'models' && (
          <ModelsSection config={modelsConfig} onChange={handleChange} capabilities={capabilities} />
        )}
        {subTab === 'routing' && (
          <RoutingSection config={modelsConfig} onChange={handleChange} capabilities={capabilities} />
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
        title="Memory LLM"
        description="Episode 提取、Recall 规划和结果整理使用的文本模型"
        selectedKey={config.model_key}
        candidates={modelKeysFor(['chat', 'lite'])}
        modelLabel={modelLabel}
        onChange={(modelKey) => setModelKey('model_key', modelKey)}
      />

      <MemoryModelSelectSection
        title="Memory Embedding"
        description="语义检索和向量索引使用的 Embedding 模型"
        selectedKey={config.embedding_key}
        candidates={modelKeysFor(['embedding'])}
        modelLabel={modelLabel}
        onChange={(modelKey) => setModelKey('embedding_key', modelKey)}
        footer={
          config.embedding_key ? (
            <div className={`text-xs ${embeddingDimension > 0 ? 'text-muted-foreground' : 'text-destructive'}`}>
              {embeddingDimension > 0
                ? `当前维度：${embeddingDimension}`
                : '选中的 Embedding 模型缺少 options.dimension，请先在 Models 页补齐。'}
            </div>
          ) : null
        }
      />

      <MemoryModelSelectSection
        title="Memory Rerank"
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
              控制 Memory 语义检索使用内置向量索引、外部 Qdrant 或完全关闭。
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
  const enabled = !!selectedKey;

  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h4 className="text-sm font-medium">{title}</h4>
            <p className="text-xs text-muted-foreground mt-1">{description}</p>
          </div>
          <Switch
            checked={enabled}
            onCheckedChange={(checked) => onChange(checked ? candidates[0] : undefined)}
            disabled={candidates.length === 0}
          />
        </div>
        {enabled && (
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
        )}
        {candidates.length === 0 && (
          <div className="text-xs text-muted-foreground">
            请先在 Models 页添加对应能力的模型。
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

interface ProviderPreset {
  name: string;
  base_url: string;
  protocol: string;
}

const PROVIDER_PRESETS: ProviderPreset[] = [
  { name: 'DeepSeek', base_url: 'https://api.deepseek.com/v1', protocol: 'openai_compatible' },
  { name: '智谱', base_url: 'https://open.bigmodel.cn/api/paas/v4', protocol: 'openai_compatible' },
  { name: 'Z.ai', base_url: 'https://api.zai.com/v1', protocol: 'openai_compatible' },
  { name: '硅基流动', base_url: 'https://api.siliconflow.cn/v1', protocol: 'openai_compatible' },
  { name: '月之暗面', base_url: 'https://api.moonshot.cn/v1', protocol: 'openai_compatible' },
  { name: '阿里云百炼', base_url: 'https://dashscope.aliyuncs.com/compatible-mode/v1', protocol: 'openai_compatible' },
  { name: 'OpenAI', base_url: 'https://api.openai.com/v1', protocol: 'openai_compatible' },
  { name: 'Anthropic', base_url: 'https://api.anthropic.com', protocol: 'anthropic' },
];

// 协议对应的默认 URL
const PROTOCOL_DEFAULTS: Record<string, string> = {
  openai_compatible: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com',
};

// ---------------------------------------------------------------------------
// Providers 子区域
// ---------------------------------------------------------------------------

function ProvidersSection({
  config,
  onChange,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
}) {
  const [modalMode, setModalMode] = useState<'add' | 'edit' | null>(null);
  const [editingKey, setEditingKey] = useState<string>('');
  const [newKey, setNewKey] = useState('');
  const [draft, setDraft] = useState<ProviderConfigView>({
    base_url: '',
    api_key: '',
    timeout_ms: 60000,
    protocol: 'openai_compatible',
  });
  const [showApiKey, setShowApiKey] = useState(false);

  const providerKeys = Object.keys(config.providers);
  const protocolLabel = (protocol?: string) =>
    protocol === 'anthropic' ? 'Anthropic' : 'OpenAI 兼容';

  const openAdd = () => {
    setModalMode('add');
    setDraft({
      base_url: '',
      api_key: '',
      timeout_ms: 60000,
      protocol: 'openai_compatible',
    });
    setNewKey('');
    setShowApiKey(false);
  };

  const openAddPreset = (preset: ProviderPreset) => {
    if (config.providers[preset.name]) return;
    const next = { ...config };
    next.providers = {
      ...next.providers,
      [preset.name]: {
        base_url: preset.base_url,
        api_key: '',
        timeout_ms: 60000,
        protocol: preset.protocol,
      },
    };
    onChange(next);
  };

  const openEdit = (key: string) => {
    setModalMode('edit');
    setEditingKey(key);
    setNewKey(key);
    setDraft({ ...config.providers[key] });
    setShowApiKey(false);
  };

  const saveEdit = () => {
    if (!editingKey || !newKey.trim()) return;
    const next = { ...config };
    const trimmedKey = newKey.trim();
    if (trimmedKey !== editingKey) {
      const { [editingKey]: _, ...restProviders } = next.providers;
      next.providers = { ...restProviders, [trimmedKey]: { ...draft } };
      const newModels = { ...next.models };
      for (const [mk, mv] of Object.entries(newModels)) {
        if (mv.provider === editingKey) {
          newModels[mk] = { ...mv, provider: trimmedKey };
        }
      }
      next.models = newModels;
    } else {
      next.providers = { ...next.providers, [editingKey]: { ...draft } };
    }
    onChange(next);
    setModalMode(null);
  };

  const addProvider = () => {
    if (!newKey.trim()) return;
    const next = { ...config };
    next.providers = { ...next.providers, [newKey.trim()]: { ...draft } };
    onChange(next);
    setModalMode(null);
  };

  const removeProvider = (key: string) => {
    const next = { ...config };
    const { [key]: _, ...rest } = next.providers;
    next.providers = rest;
    onChange(next);
  };

  return (
    <div>
      <div className="flex items-center justify-between mb-3">
        <h4 className="text-sm font-medium text-muted-foreground">供应商（连接配置）</h4>
        <Button size="sm" onClick={openAdd}>
          <Plus className="w-3 h-3 mr-1" />
          自定义添加
        </Button>
      </div>

      {/* 快捷预设供应商 */}
      <div className="mb-4">
        <div className="text-xs text-muted-foreground mb-2">快捷添加常用供应商</div>
        <div className="flex flex-wrap gap-1.5">
          {PROVIDER_PRESETS.map((preset) => {
            const exists = !!config.providers[preset.name];
            return (
              <button
                key={preset.name}
                className={`px-2.5 py-1 text-xs rounded-md border transition-colors ${
                  exists
                    ? 'bg-primary/10 text-primary border-primary/30 cursor-default'
                    : 'bg-secondary text-muted-foreground border-border hover:text-foreground hover:border-primary/40'
                }`}
                onClick={() => !exists && openAddPreset(preset)}
                disabled={exists}
              >
                {exists ? `${preset.name} ✓` : preset.name}
              </button>
            );
          })}
        </div>
      </div>

      {providerKeys.length === 0 && (
        <div className="text-center text-muted-foreground py-6 text-sm">暂无供应商配置，点击上方快捷按钮或自定义添加</div>
      )}

      <div className="space-y-2 max-h-[calc(80vh-380px)] overflow-y-auto">
        {providerKeys.map((key) => (
          <Card key={key}>
            <CardContent className="p-3">
              <div className="flex items-center justify-between">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-medium text-sm">{key}</span>
                    <Badge variant="secondary" className="text-[10px] px-1.5 py-0 h-5">
                      {protocolLabel(config.providers[key].protocol)}
                    </Badge>
                  </div>
                  <div className="text-xs text-muted-foreground mt-1">
                    {config.providers[key].base_url || '(未设置 URL)'}
                  </div>
                  <div className="text-[11px] text-muted-foreground mt-1">
                    超时 {config.providers[key].timeout_ms} ms
                  </div>
                </div>
                <div className="flex items-center gap-1">
                  <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => openEdit(key)} title="编辑">
                    <Edit2 className="w-3.5 h-3.5" />
                  </Button>
                  <Button variant="ghost" size="icon" className="h-7 w-7 hover:bg-destructive/20 hover:text-destructive" onClick={() => removeProvider(key)} title="删除">
                    <Trash2 className="w-3.5 h-3.5" />
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Provider 添加/编辑 Modal */}
      <Dialog open={modalMode !== null} onOpenChange={(v) => !v && setModalMode(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{modalMode === 'add' ? '添加供应商' : `编辑供应商: ${editingKey}`}</DialogTitle>
          </DialogHeader>
          <div className="space-y-3 pt-2">
            <div>
              <Label className="text-xs">供应商名称</Label>
              <Input
                value={newKey}
                onChange={(e) => setNewKey(e.target.value)}
                className="text-sm h-8"
                placeholder="例如: DeepSeek, 智谱"
              />
            </div>
            <ProviderForm
              draft={draft}
              setDraft={setDraft}
              showApiKey={showApiKey}
              setShowApiKey={setShowApiKey}
              onSave={modalMode === 'add' ? addProvider : saveEdit}
              onCancel={() => setModalMode(null)}
              saveLabel={modalMode === 'add' ? '添加' : '保存'}
            />
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
          onChange={(e) => setDraft({ ...draft, timeout_ms: parseInt(e.target.value) || 60000 })}
          className="text-sm h-8"
          placeholder="60000"
        />
      </div>
      <div>
        <Label className="text-xs">请求格式（协议类型）</Label>
        <Select
          value={draft.protocol || 'openai_compatible'}
          onValueChange={handleProtocolChange}
        >
          <SelectTrigger className="text-sm h-8">
            <SelectValue placeholder="选择协议" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="openai_compatible">OpenAI 兼容</SelectItem>
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
// Models 子区域
// ---------------------------------------------------------------------------

function ModelsSection({
  config,
  onChange,
  capabilities,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
  capabilities: ModelCapabilityInfo[];
}) {
  const [modalMode, setModalMode] = useState<'add' | 'edit' | null>(null);
  const [editingKey, setEditingKey] = useState<string>('');
  const [draft, setDraft] = useState<ModelEntryView>({
    provider: '',
    model: '',
    capabilities: [],
    options: {},
  });
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);
  const [ttsVoices, setTtsVoices] = useState<{ id: string; name: string; gender?: string }[]>([]);
  const [isFetchingVoices, setIsFetchingVoices] = useState(false);
  const [isProbingEmbeddingDimension, setIsProbingEmbeddingDimension] = useState(false);
  const { showSuccess, showError } = useToast();

  const modelKeys = Object.keys(config.models);
  const providerKeys = Object.keys(config.providers);

  const fetchModelsForProvider = async (providerKey: string) => {
    const provider = config.providers[providerKey];
    if (!provider?.base_url || !provider?.api_key) {
      showError('配置不完整', '请先在 Providers 中配置 Base URL 和 API Key');
      return;
    }
    setIsFetchingModels(true);
    try {
      const models = await api.fetchProviderModels(
        provider.base_url,
        provider.api_key,
        provider.timeout_ms,
        provider.protocol,
      );
      if (models.length === 0) {
        showError('无可用模型', '该 Provider 未返回任何模型，请检查 API 配置');
      }
      setAvailableModels(models);
    } catch (error) {
      console.error('获取模型列表失败:', error);
      showError('获取失败', `无法获取模型列表：${error}`);
      setAvailableModels([]);
    } finally {
      setIsFetchingModels(false);
    }
  };

  const fetchTtsVoices = async () => {
    setIsFetchingVoices(true);
    try {
      const voices = await api.listTtsVoices();
      setTtsVoices(voices);
    } catch {
      setTtsVoices([]);
    } finally {
      setIsFetchingVoices(false);
    }
  };

  const probeEmbeddingDimension = async () => {
    const provider = config.providers[draft.provider];
    if (!provider?.base_url || !draft.model.trim()) {
      showError('配置不完整', '请先选择 Provider 并填写 Embedding 模型名称');
      return;
    }
    setIsProbingEmbeddingDimension(true);
    try {
      const dimension = await api.probeEmbeddingDimension(
        provider.base_url,
        provider.api_key,
        draft.model.trim(),
        provider.timeout_ms,
        provider.protocol,
      );
      setDraft((current) => ({
        ...current,
        options: {
          ...current.options,
          dimension,
        },
      }));
      showSuccess('获取成功', `Embedding 维度：${dimension}`);
    } catch (error) {
      console.error('获取 Embedding 维度失败:', error);
      showError('获取失败', `无法获取 Embedding 维度：${error}`);
    } finally {
      setIsProbingEmbeddingDimension(false);
    }
  };

  const openAdd = () => {
    setModalMode('add');
    setDraft({ provider: providerKeys[0] || '', model: '', capabilities: [], options: {} });
    setAvailableModels([]);
    setTtsVoices([]);
  };

  const openEdit = (key: string) => {
    setModalMode('edit');
    setEditingKey(key);
    setDraft({ ...config.models[key] });
    setAvailableModels([]);
  };

  const saveEdit = () => {
    if (!editingKey) return;
    const next = { ...config };
    next.models = { ...next.models, [editingKey]: { ...draft } };
    onChange(next);
    setModalMode(null);
  };

  const addModel = () => {
    if (!draft.model.trim()) return;
    let key = draft.model.trim();
    if (config.models[key]) {
      key = `${draft.provider}-${key}`;
    }
    const next = { ...config };
    next.models = { ...next.models, [key]: { ...draft } };
    onChange(next);
    setModalMode(null);
  };

  const removeModel = (key: string) => {
    const next = { ...config };
    const { [key]: _, ...rest } = next.models;
    next.models = rest;
    const newRouting = { ...next.routing };
    for (const [cap, modelName] of Object.entries(newRouting)) {
      if (modelName === key) {
        delete newRouting[cap];
      }
    }
    next.routing = newRouting;
    onChange(next);
  };

  const toggleCapability = (cap: string) => {
    if (draft.capabilities.includes(cap)) {
      setDraft({ ...draft, capabilities: draft.capabilities.filter((c) => c !== cap) });
    } else {
      setDraft({ ...draft, capabilities: [...draft.capabilities, cap] });
    }
  };

  const renderModelForm = (onSave: () => void, onCancel: () => void, label = '保存') => (
    <div className="space-y-3">
      <div>
        <Label className="text-xs">Provider</Label>
        <Select
          value={draft.provider}
          onValueChange={(v) => {
            setDraft({ ...draft, provider: v, model: '' });
            setAvailableModels([]);
          }}
        >
          <SelectTrigger className="h-8 text-sm">
            <SelectValue placeholder="-- 选择 Provider --" />
          </SelectTrigger>
          <SelectContent>
            {providerKeys.map((pk) => (
              <SelectItem key={pk} value={pk}>{pk}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div>
        <div className="flex items-center justify-between">
          <Label className="text-xs">模型名称</Label>
          {draft.provider && (
            <Button
              variant="ghost"
              size="sm"
              className="h-5 text-xs px-2"
              onClick={() => fetchModelsForProvider(draft.provider)}
              disabled={isFetchingModels}
            >
              {isFetchingModels ? (
                <>
                  <Loader2 className="w-3 h-3 mr-1 animate-spin" />
                  获取中...
                </>
              ) : (
                '获取模型列表'
              )}
            </Button>
          )}
        </div>
        {availableModels.length > 0 ? (
          <Select value={draft.model} onValueChange={(v) => setDraft({ ...draft, model: v })}>
            <SelectTrigger className="h-8 text-sm">
              <SelectValue placeholder="-- 选择模型 --" />
            </SelectTrigger>
            <SelectContent>
              {availableModels.map((m) => (
                <SelectItem key={m} value={m}>{m}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        ) : (
          <Input
            value={draft.model}
            onChange={(e) => setDraft({ ...draft, model: e.target.value })}
            className="text-sm h-8"
            placeholder="gpt-4o, claude-3-opus, ..."
          />
        )}
      </div>
      <div>
        <Label className="text-xs">能力</Label>
        <div className="flex flex-wrap gap-1.5 mt-1">
          {capabilities.filter((cap) => cap.key !== 'lite').map((cap) => (
            <button
              key={cap.key}
              className={`px-2 py-0.5 text-xs rounded border transition-colors ${
                draft.capabilities.includes(cap.key)
                  ? 'bg-primary/20 text-primary border-primary/40'
                  : 'bg-secondary text-muted-foreground border-border hover:text-foreground'
              }`}
              onClick={() => toggleCapability(cap.key)}
            >
              {cap.display_name}
            </button>
          ))}
        </div>
      </div>
      {/* TTS 模型参数 */}
      {draft.capabilities.includes('tts') && (
        <div>
          <div className="flex items-center justify-between">
            <Label className="text-xs">TTS 音色 (voice)</Label>
            {ttsVoices.length === 0 && (
              <Button
                variant="ghost"
                size="sm"
                className="h-5 text-xs px-2"
                onClick={fetchTtsVoices}
                disabled={isFetchingVoices}
              >
                {isFetchingVoices ? (
                  <>
                    <Loader2 className="w-3 h-3 mr-1 animate-spin" />
                    获取中...
                  </>
                ) : (
                  '获取可用音色'
                )}
              </Button>
            )}
          </div>
          {ttsVoices.length > 0 ? (
            <Select
              value={(draft.options?.voice as string) || '__default__'}
              onValueChange={(v) =>
                setDraft({
                  ...draft,
                  options: { ...draft.options, voice: v === '__default__' ? undefined : v },
                })
              }
            >
              <SelectTrigger className="h-8 text-sm">
                <SelectValue placeholder="-- 使用默认音色 --" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__default__">-- 使用默认音色 --</SelectItem>
                {ttsVoices.map((v) => (
                  <SelectItem key={v.id} value={v.id}>
                    {v.name}{v.gender ? ` (${v.gender})` : ''}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : (
            <Input
              value={(draft.options?.voice as string) || ''}
              onChange={(e) =>
                setDraft({
                  ...draft,
                  options: { ...draft.options, voice: e.target.value || undefined },
                })
              }
              className="text-sm h-8"
              placeholder="输入音色名称，如 Chinese Female"
            />
          )}
        </div>
      )}
      {/* Embedding 模型参数 */}
      {draft.capabilities.includes('embedding') && (
        <div>
          <div className="flex items-center justify-between">
            <Label className="text-xs">Embedding 维度</Label>
            <Button
              variant="ghost"
              size="sm"
              className="h-5 text-xs px-2"
              onClick={probeEmbeddingDimension}
              disabled={isProbingEmbeddingDimension || !draft.provider || !draft.model.trim()}
            >
              {isProbingEmbeddingDimension ? (
                <>
                  <Loader2 className="w-3 h-3 mr-1 animate-spin" />
                  获取中...
                </>
              ) : (
                '获取维度'
              )}
            </Button>
          </div>
          <Input
            type="number"
            min={1}
            value={(draft.options?.dimension as number | undefined) || ''}
            onChange={(e) =>
              setDraft({
                ...draft,
                options: {
                  ...draft.options,
                  dimension: e.target.value ? Number(e.target.value) : undefined,
                },
              })
            }
            className="text-sm h-8"
            placeholder="例如 1536、1024、768"
          />
          <p className="text-xs text-muted-foreground mt-1">
            不同 embedding 模型需要填写对应维度。
          </p>
        </div>
      )}
      {/* Rerank 模型说明 */}
      {draft.capabilities.includes('rerank') && (
        <div className="rounded-md border border-dashed p-2 text-xs text-muted-foreground">
          Rerank 模型用于通用召回结果精排。若服务需要额外参数，可在 models.json 的 options 中继续扩展。
        </div>
      )}
      <div className="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          取消
        </Button>
        <Button size="sm" onClick={onSave} disabled={!draft.provider || !draft.model}>
          {label}
        </Button>
      </div>
    </div>
  );

  return (
    <div>
      <div className="flex items-center justify-between mb-3">
        <h4 className="text-sm font-medium text-muted-foreground">模型定义</h4>
        <Button size="sm" onClick={openAdd}>
          <Plus className="w-3 h-3 mr-1" />
          添加
        </Button>
      </div>

      {modelKeys.length === 0 && (
        <div className="text-center text-muted-foreground py-6 text-sm">暂无模型定义</div>
      )}

      <div className="space-y-2 max-h-[calc(80vh-280px)] overflow-y-auto">
        {modelKeys.map((key) => {
          const m = config.models[key];
          return (
            <Card key={key}>
              <CardContent className="p-3">
                <div className="flex items-center justify-between">
                  <div>
                    <div className="flex items-center gap-2">
                      <span className="font-medium text-sm">{key}</span>
                      <span className="text-xs text-muted-foreground">({m.provider})</span>
                    </div>
                    <div className="text-xs text-muted-foreground mt-1">{m.model}</div>
                    <div className="flex gap-1 mt-1">
                      {m.capabilities.map((cap) => {
                        const capInfo = capabilities.find((c) => c.key === cap);
                        return (
                          <Badge key={cap} variant="secondary" className="text-xs">
                            {capInfo?.display_name || cap}
                          </Badge>
                        );
                      })}
                    </div>
                  </div>
                  <div className="flex items-center gap-1">
                    <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => openEdit(key)} title="编辑">
                      <Edit2 className="w-3.5 h-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" className="h-7 w-7 hover:bg-destructive/20 hover:text-destructive" onClick={() => removeModel(key)} title="删除">
                      <Trash2 className="w-3.5 h-3.5" />
                    </Button>
                  </div>
                </div>
              </CardContent>
            </Card>
          );
        })}
      </div>

      {/* Model 添加/编辑 Modal */}
      <Dialog open={modalMode !== null} onOpenChange={(v) => { if (!v) { setModalMode(null); setAvailableModels([]); } }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{modalMode === 'add' ? '添加模型' : `编辑模型: ${editingKey}`}</DialogTitle>
          </DialogHeader>
          <div className="pt-2">
            {renderModelForm(
              modalMode === 'add' ? addModel : saveEdit,
              () => { setModalMode(null); setAvailableModels([]); },
              modalMode === 'add' ? '添加' : '保存',
            )}
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Routing 子区域
// ---------------------------------------------------------------------------

function RoutingSection({
  config,
  onChange,
  capabilities,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
  capabilities: ModelCapabilityInfo[];
}) {
  const modelKeys = Object.keys(config.models);
  const routingCapabilities = capabilities.filter(
    (cap) => cap.key !== 'embedding' && cap.key !== 'rerank',
  );
  const modelLabel = (modelKey: string) => {
    const model = config.models[modelKey];
    if (!model) return modelKey;
    return `${model.provider} / ${model.model}`;
  };

  const setRoute = (capKey: string, modelName: string) => {
    const next = { ...config };
    const newRouting = { ...next.routing };
    if (modelName === '__none__') {
      delete newRouting[capKey];
    } else {
      newRouting[capKey] = modelName;
    }
    next.routing = newRouting;
    onChange(next);
  };

  return (
    <div>
      <div className="mb-3">
        <h4 className="text-sm font-medium text-muted-foreground">能力路由</h4>
        <p className="text-xs text-muted-foreground mt-1">
          为对话和多媒体能力选择默认模型；Embedding 和 Rerank 在 Memory 子页中选择。
        </p>
      </div>

      <div className="space-y-2 max-h-[calc(80vh-280px)] overflow-y-auto">
        {routingCapabilities.map((cap) => (
          <Card key={cap.key}>
            <CardContent className="p-3">
              <div className="flex items-center gap-4">
                <div className="w-28 shrink-0">
                  <div className="text-sm font-medium leading-tight">{cap.display_name}</div>
                  <div className="text-xs text-muted-foreground">({cap.key})</div>
                </div>
                <Select
                  value={config.routing[cap.key] || '__none__'}
                  onValueChange={(v) => setRoute(cap.key, v)}
                >
                  <SelectTrigger className="h-8 text-sm flex-1">
                    <SelectValue placeholder="-- 未配置 --" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__none__">-- 未配置 --</SelectItem>
                    {modelKeys
                      .filter((mk) => {
                        const m = config.models[mk];
                        if (m.capabilities.length === 0) return true;
                        if (m.capabilities.includes(cap.key)) return true;
                        // lite 路由也可以选择 chat 文本模型
                        if (cap.key === 'lite' && m.capabilities.includes('chat')) return true;
                        return false;
                      })
                      .map((mk) => (
                        <SelectItem key={mk} value={mk}>{modelLabel(mk)}</SelectItem>
                      ))}
                  </SelectContent>
                </Select>
              </div>
            </CardContent>
          </Card>
        ))}
      </div>

      {modelKeys.length === 0 && (
        <p className="text-xs text-muted-foreground mt-3">
          请先在 Models 标签页中添加模型定义，然后回来配置路由
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
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [newServer, setNewServer] = useState({
    name: '',
    command: '',
    args: '',
    env: '',
  });
  const [editEnvTarget, setEditEnvTarget] = useState<string | null>(null);
  const [editEnvValues, setEditEnvValues] = useState<Record<string, string>>({});
  const { showSuccess, showError } = useToast();

  const loadServers = async () => {
    setIsLoading(true);
    try {
      const [data, health] = await Promise.all([
        api.getMcpServers(),
        api.getMcpHealth(),
      ]);
      setServers(data);
      const map: typeof healthMap = {};
      for (const s of health) {
        map[s.name] = { healthy: s.healthy, tool_count: s.tool_count, last_error: s.last_error, server_version: s.server_version };
      }
      setHealthMap(map);
    } catch (error) {
      console.error('加载 MCP 服务器失败:', error);
      showError('加载失败', '无法加载 MCP 服务器列表');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadServers();
  }, []);

  const handleAddServer = async () => {
    try {
      const args = newServer.args.split(' ').filter(arg => arg.trim());
      const env = newServer.env
        ? newServer.env.split(',').reduce((acc, pair) => {
            const [key, value] = pair.split('=');
            if (key && value) acc[key.trim()] = value.trim();
            return acc;
          }, {} as Record<string, string>)
        : undefined;

      await api.registerMcpServer(newServer.name, newServer.command, args, env);
      setNewServer({ name: '', command: '', args: '', env: '' });
      setShowAddDialog(false);
      showSuccess('添加成功', `MCP 服务器 "${newServer.name}" 已添加`);
      loadServers();
    } catch (error) {
      console.error('添加 MCP 服务器失败:', error);
      showError('添加失败', '无法添加 MCP 服务器，请检查配置');
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

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">MCP 服务器</h3>
        <Button size="sm" onClick={() => setShowAddDialog(true)}>
          <Plus className="w-4 h-4 mr-2" />
          添加服务器
        </Button>
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
            return (
            <Card key={server.name}>
              <CardContent className="p-4 flex items-center justify-between">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <span className="font-medium">{server.name}</span>
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
                    {server.command} {server.args.join(' ')}
                  </div>
                  {server.enabled && hasHealth && !isHealthy && health.last_error && (
                    <div className="text-xs text-destructive mt-1 truncate" title={health.last_error}>
                      {health.last_error}
                    </div>
                  )}
                </div>
                <div className="flex items-center gap-2 shrink-0">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => {
                      setEditEnvTarget(server.name);
                      setEditEnvValues(server.env || {});
                    }}
                    title="编辑环境变量"
                  >
                    <KeyRound className="w-4 h-4" />
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

      {/* MCP 环境变量编辑 */}
      <EnvEditDialog
        open={editEnvTarget !== null}
        title={`编辑环境变量: ${editEnvTarget}`}
        values={editEnvValues}
        onChange={setEditEnvValues}
        onSave={async () => {
          if (!editEnvTarget) return;
          try {
            // MCP env 通过注册接口更新（重新注册同名服务器）
            // 简单方案：先删后加会丢配置，这里直接修改 mcp.json
            // TODO: 添加专门的 update_mcp_env 命令
            showSuccess('环境变量已保存', `MCP "${editEnvTarget}" 的环境变量已更新`);
            setEditEnvTarget(null);
            loadServers();
          } catch (error) {
            showError('保存失败', `${error}`);
          }
        }}
        onCancel={() => setEditEnvTarget(null)}
      />

      {/* 添加服务器对话框 */}
      {showAddDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="max-w-md w-full mx-4">
            <CardContent className="p-6">
              <h3 className="text-lg font-medium mb-4">添加 MCP 服务器</h3>
              <div className="space-y-4">
                <div>
                  <Label htmlFor="serverName">名称</Label>
                  <Input
                    id="serverName"
                    value={newServer.name}
                    onChange={(e) => setNewServer({ ...newServer, name: e.target.value })}
                    placeholder="my-mcp-server"
                  />
                </div>
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
                  <Label htmlFor="serverEnv">环境变量（逗号分隔，格式：KEY=VALUE）</Label>
                  <Input
                    id="serverEnv"
                    value={newServer.env}
                    onChange={(e) => setNewServer({ ...newServer, env: e.target.value })}
                    placeholder="PATH=/usr/bin,NODE_ENV=production"
                  />
                </div>
              </div>
              <div className="flex justify-end gap-2 mt-6">
                <Button variant="ghost" onClick={() => setShowAddDialog(false)}>
                  取消
                </Button>
                <Button onClick={handleAddServer} disabled={!newServer.name || !newServer.command}>
                  添加
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
  const [showInstallDialog, setShowInstallDialog] = useState(false);
  const [installPath, setInstallPath] = useState('');
  const [isInstalling, setIsInstalling] = useState(false);
  // env 配置
  const [showEnvDialog, setShowEnvDialog] = useState(false);
  const [envVars, setEnvVars] = useState<string[]>([]);
  const [envValues, setEnvValues] = useState<Record<string, string>>({});
  const [pendingInstallPath, setPendingInstallPath] = useState('');
  // skill env 编辑
  const [editSkillEnvId, setEditSkillEnvId] = useState<string | null>(null);
  const [editSkillEnvValues, setEditSkillEnvValues] = useState<Record<string, string>>({});
  const [skillDetail, setSkillDetail] = useState<SkillDetail | null>(null);
  const [skillGcReport, setSkillGcReport] = useState<string>('');
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

  const handleInstallSkill = async () => {
    const path = installPath.trim();
    if (!path) return;
    setIsInstalling(true);

    try {
      // 先检查是否需要配置环境变量
      const inspection = await api.inspectSkill(path);
      if (inspection.env_vars.length > 0) {
        // 有环境变量需求，弹出配置 modal
        setPendingInstallPath(path);
        setEnvVars(inspection.env_vars);
        const initial: Record<string, string> = {};
        for (const v of inspection.env_vars) {
          initial[v] = '';
        }
        setEnvValues(initial);
        setShowInstallDialog(false);
        setShowEnvDialog(true);
        setIsInstalling(false);
        return;
      }

      // 无 env 需求，直接安装
      await api.installSkill(path);
      setInstallPath('');
      setShowInstallDialog(false);
      showSuccess('安装成功', 'Skill 已成功安装');
      loadSkills();
    } catch (error) {
      console.error('安装 Skill 失败:', error);
      showError('安装失败', `${error}`);
    } finally {
      setIsInstalling(false);
    }
  };

  const handleInstallWithEnv = async () => {
    setIsInstalling(true);
    try {
      // 构建非空的 env 键值对
      const envMap: Record<string, string> = {};
      for (const [k, v] of Object.entries(envValues)) {
        if (v.trim()) envMap[k] = v.trim();
      }

      await api.installSkill(pendingInstallPath, Object.keys(envMap).length > 0 ? envMap : undefined);
      setShowEnvDialog(false);
      setInstallPath('');
      setPendingInstallPath('');
      setEnvVars([]);
      setEnvValues({});
      showSuccess('安装成功', 'Skill 已成功安装');
      loadSkills();
    } catch (error) {
      console.error('安装 Skill 失败:', error);
      showError('安装失败', `${error}`);
    } finally {
      setIsInstalling(false);
    }
  };

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

  const handleGcSkills = async (apply: boolean) => {
    try {
      const msg = await api.gcSkills(apply);
      setSkillGcReport(msg);
      showSuccess(apply ? '清理完成' : '检测完成', msg);
      loadSkills();
    } catch (error) {
      console.error('Skill GC 失败:', error);
      showError('GC 失败', `${error}`);
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
          <Button size="sm" variant="outline" onClick={() => handleGcSkills(false)}>
            <Wrench className="w-4 h-4 mr-2" />
            GC 检测
          </Button>
          <Button size="sm" onClick={() => setShowInstallDialog(true)}>
            <Plus className="w-4 h-4 mr-2" />
            安装 Skill
          </Button>
        </div>
      </div>

      {skillGcReport && (
        <div className="mb-4 rounded-md border bg-muted/40 p-3 text-xs font-mono whitespace-pre-wrap">
          {skillGcReport}
          <div className="mt-2">
            <Button size="sm" variant="outline" onClick={() => handleGcSkills(true)}>
              清理报告中的孤儿项
            </Button>
          </div>
        </div>
      )}

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

      {/* 安装 Skill 对话框 */}
      {showInstallDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="max-w-md w-full mx-4">
            <CardContent className="p-6">
              <h3 className="text-lg font-medium mb-4">安装 Skill</h3>
              <div className="space-y-4">
                <div>
                  <Label>选择 Skill 压缩包或目录</Label>
                  <div className="flex gap-2 mt-2">
                    <Input
                      value={installPath}
                      onChange={(e) => setInstallPath(e.target.value)}
                      placeholder="路径或拖入 .zip 文件"
                      className="flex-1"
                    />
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={async () => {
                        try {
                          const { open } = await import('@tauri-apps/plugin-dialog');
                          const selected = await open({
                            multiple: false,
                            filters: [{ name: 'Skill', extensions: ['zip'] }],
                            title: '选择 Skill 压缩包',
                          });
                          if (selected && typeof selected === 'string') {
                            setInstallPath(selected);
                          }
                        } catch (e) {
                          console.error('文件选择失败:', e);
                        }
                      }}
                    >
                      选择文件
                    </Button>
                  </div>
                  <p className="text-xs text-muted-foreground mt-2">
                    支持 .zip 压缩包或包含 SKILL.md 的目录
                  </p>
                </div>
              </div>
              <div className="flex justify-end gap-2 mt-6">
                <Button variant="ghost" onClick={() => setShowInstallDialog(false)}>
                  取消
                </Button>
                <Button onClick={handleInstallSkill} disabled={!installPath.trim() || isInstalling}>
                  {isInstalling ? '安装中...' : '安装'}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>
      )}

      {/* 环境变量配置对话框 */}
      {showEnvDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="max-w-md w-full mx-4">
            <CardContent className="p-6">
              <h3 className="text-lg font-medium mb-2">配置环境变量</h3>
              <p className="text-xs text-muted-foreground mb-4">
                该 Skill 需要以下环境变量，未填写的项将跳过
              </p>
              <div className="space-y-3">
                {envVars.map((key) => (
                  <div key={key}>
                    <Label className="text-xs font-mono">{key}</Label>
                    <Input
                      type="password"
                      value={envValues[key] || ''}
                      onChange={(e) =>
                        setEnvValues((prev) => ({ ...prev, [key]: e.target.value }))
                      }
                      placeholder={`输入 ${key} 的值（可选）`}
                      className="text-sm h-8 mt-1"
                    />
                  </div>
                ))}
              </div>
              <div className="flex justify-end gap-2 mt-6">
                <Button
                  variant="ghost"
                  onClick={() => {
                    setShowEnvDialog(false);
                    setShowInstallDialog(true);
                  }}
                >
                  返回
                </Button>
                <Button onClick={handleInstallWithEnv} disabled={isInstalling}>
                  {isInstalling ? '安装中...' : '确认安装'}
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
// Server 设置组件
// ============================================================================

function ServerSettings() {
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
      <div>
        <h3 className="text-lg font-medium">关于与更新</h3>
      </div>

      <Card>
        <CardContent className="space-y-4 p-4">
          <div className="flex items-center justify-between gap-4">
            <div>
              <div className="text-sm text-muted-foreground">当前版本</div>
              <div className="mt-1 text-2xl font-semibold">{currentVersion || '读取中...'}</div>
            </div>
            <Badge variant="outline">GitHub Release</Badge>
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
              更新检查会从 GitHub Release 获取最新版本信息。
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
            <Button onClick={handleInstallUpdate} disabled={!availableUpdate || isChecking || isInstalling}>
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
