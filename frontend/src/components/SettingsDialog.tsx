import { useState, useEffect, useRef, useCallback } from 'react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Badge } from './ui/badge';
import { Card, CardContent } from './ui/card';
import { Switch } from './ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './ui/tabs';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Settings, Eye, EyeOff, Server, Puzzle, Plus, Trash2, Loader2, Globe, Link, Edit2 } from 'lucide-react';
import { api } from '@/api/tauri';
import type { McpServer, Skill, ServerConfig, ConnectorInfo, ModelsConfigView, ProviderConfigView, ModelEntryView, ModelCapabilityInfo } from '@/api/tauri';
import { useToast } from './Toast';

export function SettingsDialog() {
  const [open, setOpen] = useState(false);

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
        <DialogContent className="max-w-4xl max-h-[80vh] overflow-hidden flex flex-col">
          <DialogHeader>
            <DialogTitle>系统设置</DialogTitle>
          </DialogHeader>

          <Tabs defaultValue="llm" className="flex-1 overflow-hidden flex flex-col">
            <TabsList className="w-full justify-start">
              <TabsTrigger value="llm">
                <Settings className="w-4 h-4 mr-2" />
                LLM 配置
              </TabsTrigger>
              <TabsTrigger value="mcp">
                <Server className="w-4 h-4 mr-2" />
                MCP 服务器
              </TabsTrigger>
              <TabsTrigger value="skill">
                <Puzzle className="w-4 h-4 mr-2" />
                Skills
              </TabsTrigger>
              <TabsTrigger value="server">
                <Globe className="w-4 h-4 mr-2" />
                Server
              </TabsTrigger>
              <TabsTrigger value="connector">
                <Link className="w-4 h-4 mr-2" />
                Connectors
              </TabsTrigger>
            </TabsList>

            <div className="flex-1 overflow-y-auto">
              <TabsContent value="llm">
                <LLMSettings />
              </TabsContent>
              <TabsContent value="mcp">
                <McpSettings />
              </TabsContent>
              <TabsContent value="skill">
                <SkillSettings />
              </TabsContent>
              <TabsContent value="server">
                <ServerSettings />
              </TabsContent>
              <TabsContent value="connector">
                <ConnectorSettings />
              </TabsContent>
            </div>
          </Tabs>
        </DialogContent>
      </Dialog>
    </>
  );
}

// ============================================================================
// LLM 设置组件（三层架构：Providers / Models / Routing）
// ============================================================================

type LLMSubTab = 'providers' | 'models' | 'routing';

function LLMSettings() {
  const [subTab, setSubTab] = useState<LLMSubTab>('providers');
  const [modelsConfig, setModelsConfig] = useState<ModelsConfigView>({
    providers: {},
    models: {},
    routing: {},
  });
  const [capabilities, setCapabilities] = useState<ModelCapabilityInfo[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const [saveStatus, setSaveStatus] = useState<'idle' | 'saving' | 'saved' | 'error'>('idle');
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
    setSaveStatus('saving');
    try {
      await api.setModelsConfig(config);
      setSaveStatus('saved');
      // 2 秒后恢复 idle
      setTimeout(() => setSaveStatus('idle'), 2000);
    } catch (error) {
      console.error('自动保存失败:', error);
      setSaveStatus('error');
      showError('保存失败', '无法保存模型配置');
    }
  }, [showError]);

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
      {/* 子标签页 */}
      <Tabs value={subTab} onValueChange={(v) => setSubTab(v as LLMSubTab)}>
        <TabsList>
          <TabsTrigger value="providers">Providers</TabsTrigger>
          <TabsTrigger value="models">Models</TabsTrigger>
          <TabsTrigger value="routing">Routing</TabsTrigger>
        </TabsList>

        <TabsContent value="providers">
          <ProvidersSection config={modelsConfig} onChange={handleChange} />
        </TabsContent>
        <TabsContent value="models">
          <ModelsSection config={modelsConfig} onChange={handleChange} capabilities={capabilities} />
        </TabsContent>
        <TabsContent value="routing">
          <RoutingSection config={modelsConfig} onChange={handleChange} capabilities={capabilities} />
        </TabsContent>
      </Tabs>

      {/* 保存状态指示 */}
      {saveStatus !== 'idle' && (
        <div className="flex items-center justify-end text-xs text-muted-foreground">
          {saveStatus === 'saving' && (
            <><Loader2 className="w-3 h-3 mr-1 animate-spin" />保存中...</>
          )}
          {saveStatus === 'saved' && '已自动保存'}
          {saveStatus === 'error' && <span className="text-destructive">保存失败</span>}
        </div>
      )}
    </div>
  );
}

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
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const [newKey, setNewKey] = useState('');
  const [draft, setDraft] = useState<ProviderConfigView>({
    base_url: '',
    api_key: '',
    timeout_ms: 60000,
  });
  const [showApiKey, setShowApiKey] = useState(false);

  const providerKeys = Object.keys(config.providers);

  const startEdit = (key: string) => {
    setEditingKey(key);
    setDraft({ ...config.providers[key] });
    setShowApiKey(false);
  };

  const saveEdit = () => {
    if (!editingKey) return;
    const next = { ...config };
    next.providers = { ...next.providers, [editingKey]: { ...draft } };
    onChange(next);
    setEditingKey(null);
  };

  const addProvider = () => {
    if (!newKey.trim()) return;
    const next = { ...config };
    next.providers = { ...next.providers, [newKey.trim()]: { ...draft } };
    onChange(next);
    setShowAdd(false);
    setNewKey('');
    setDraft({ base_url: '', api_key: '', timeout_ms: 60000 });
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
        <h4 className="text-sm font-medium text-muted-foreground">Providers (连接配置)</h4>
        <Button
          size="sm"
          onClick={() => {
            setShowAdd(true);
            setDraft({ base_url: '', api_key: '', timeout_ms: 60000 });
            setNewKey('');
            setShowApiKey(false);
          }}
        >
          <Plus className="w-3 h-3 mr-1" />
          添加
        </Button>
      </div>

      {providerKeys.length === 0 && !showAdd && (
        <div className="text-center text-muted-foreground py-6 text-sm">暂无 Provider 配置</div>
      )}

      <div className="space-y-2">
        {providerKeys.map((key) => (
          <Card key={key}>
            <CardContent className="p-3">
              {editingKey === key ? (
                <ProviderForm
                  providerKey={key}
                  draft={draft}
                  setDraft={setDraft}
                  showApiKey={showApiKey}
                  setShowApiKey={setShowApiKey}
                  onSave={saveEdit}
                  onCancel={() => setEditingKey(null)}
                />
              ) : (
                <div className="flex items-center justify-between">
                  <div>
                    <span className="font-medium text-sm">{key}</span>
                    <div className="text-xs text-muted-foreground mt-1">
                      {config.providers[key].base_url || '(未设置 URL)'}
                    </div>
                  </div>
                  <div className="flex items-center gap-1">
                    <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => startEdit(key)} title="编辑">
                      <Edit2 className="w-3.5 h-3.5" />
                    </Button>
                    <Button variant="ghost" size="icon" className="h-7 w-7 hover:bg-destructive/20 hover:text-destructive" onClick={() => removeProvider(key)} title="删除">
                      <Trash2 className="w-3.5 h-3.5" />
                    </Button>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>
        ))}

        {showAdd && (
          <Card className="border-primary/40">
            <CardContent className="p-3 space-y-3">
              <div>
                <Label className="text-xs">Provider 名称</Label>
                <Input
                  value={newKey}
                  onChange={(e) => setNewKey(e.target.value)}
                  className="text-sm h-8"
                  placeholder="例如: openai, anthropic"
                />
              </div>
              <ProviderForm
                draft={draft}
                setDraft={setDraft}
                showApiKey={showApiKey}
                setShowApiKey={setShowApiKey}
                onSave={addProvider}
                onCancel={() => setShowAdd(false)}
                saveLabel="添加"
              />
            </CardContent>
          </Card>
        )}
      </div>
    </div>
  );
}

function ProviderForm({
  providerKey,
  draft,
  setDraft,
  showApiKey,
  setShowApiKey,
  onSave,
  onCancel,
  saveLabel = '保存',
}: {
  providerKey?: string;
  draft: ProviderConfigView;
  setDraft: (d: ProviderConfigView) => void;
  showApiKey: boolean;
  setShowApiKey: (v: boolean) => void;
  onSave: () => void;
  onCancel: () => void;
  saveLabel?: string;
}) {
  return (
    <div className="space-y-2">
      {providerKey && (
        <div className="text-xs text-muted-foreground mb-1">编辑: {providerKey}</div>
      )}
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
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [showAdd, setShowAdd] = useState(false);
  const [draft, setDraft] = useState<ModelEntryView>({
    provider: '',
    model: '',
    capabilities: [],
    options: {},
  });
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);
  const { showError } = useToast();

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

  const startEdit = (key: string) => {
    setEditingKey(key);
    setDraft({ ...config.models[key] });
    setAvailableModels([]);
  };

  const saveEdit = () => {
    if (!editingKey) return;
    const next = { ...config };
    next.models = { ...next.models, [editingKey]: { ...draft } };
    onChange(next);
    setEditingKey(null);
  };

  const addModel = () => {
    if (!draft.model.trim()) return;
    // 用模型名称作为 key，如果已存在则加 provider 前缀
    let key = draft.model.trim();
    if (config.models[key]) {
      key = `${draft.provider}-${key}`;
    }
    const next = { ...config };
    next.models = { ...next.models, [key]: { ...draft } };
    onChange(next);
    setShowAdd(false);
    setDraft({ provider: '', model: '', capabilities: [], options: {} });
    setAvailableModels([]);
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
    <div className="space-y-2">
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
          {capabilities.map((cap) => (
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
        <h4 className="text-sm font-medium text-muted-foreground">Models (模型定义)</h4>
        <Button
          size="sm"
          onClick={() => {
            setShowAdd(true);
            setDraft({ provider: providerKeys[0] || '', model: '', capabilities: [], options: {} });
            setAvailableModels([]);
          }}
        >
          <Plus className="w-3 h-3 mr-1" />
          添加
        </Button>
      </div>

      {modelKeys.length === 0 && !showAdd && (
        <div className="text-center text-muted-foreground py-6 text-sm">暂无模型定义</div>
      )}

      <div className="space-y-2">
        {modelKeys.map((key) => {
          const m = config.models[key];
          return (
            <Card key={key}>
              <CardContent className="p-3">
                {editingKey === key ? (
                  renderModelForm(saveEdit, () => setEditingKey(null))
                ) : (
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
                      <Button variant="ghost" size="icon" className="h-7 w-7" onClick={() => startEdit(key)} title="编辑">
                        <Edit2 className="w-3.5 h-3.5" />
                      </Button>
                      <Button variant="ghost" size="icon" className="h-7 w-7 hover:bg-destructive/20 hover:text-destructive" onClick={() => removeModel(key)} title="删除">
                        <Trash2 className="w-3.5 h-3.5" />
                      </Button>
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          );
        })}

        {showAdd && (
          <Card className="border-primary/40">
            <CardContent className="p-3">
              {renderModelForm(addModel, () => { setShowAdd(false); setAvailableModels([]); }, '添加')}
            </CardContent>
          </Card>
        )}
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
  capabilities,
}: {
  config: ModelsConfigView;
  onChange: (c: ModelsConfigView) => void;
  capabilities: ModelCapabilityInfo[];
}) {
  const modelKeys = Object.keys(config.models);

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
        <h4 className="text-sm font-medium text-muted-foreground">Routing (能力路由)</h4>
        <p className="text-xs text-muted-foreground mt-1">
          为每种能力选择对应的模型，多媒体（图片/视频/STT/TTS）也通过此处配置
        </p>
      </div>

      <div className="space-y-2">
        {capabilities.map((cap) => (
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
                        return m.capabilities.length === 0 || m.capabilities.includes(cap.key);
                      })
                      .map((mk) => (
                        <SelectItem key={mk} value={mk}>{mk}</SelectItem>
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
  const [isLoading, setIsLoading] = useState(false);
  const [showAddDialog, setShowAddDialog] = useState(false);
  const [newServer, setNewServer] = useState({
    name: '',
    command: '',
    args: '',
    env: '',
  });
  const { showSuccess, showError } = useToast();

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
          {servers.map((server) => (
            <Card key={server.name}>
              <CardContent className="p-4 flex items-center justify-between">
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{server.name}</span>
                    <Badge variant={server.enabled ? 'default' : 'secondary'}>
                      {server.enabled ? '已启用' : '已禁用'}
                    </Badge>
                  </div>
                  <div className="text-sm text-muted-foreground mt-1">
                    {server.command} {server.args.join(' ')}
                  </div>
                </div>
                <div className="flex items-center gap-2">
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
          ))}
        </div>
      )}

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
    try {
      await api.installSkill(installPath);
      setInstallPath('');
      setShowInstallDialog(false);
      showSuccess('安装成功', 'Skill 已成功安装');
      loadSkills();
    } catch (error) {
      console.error('安装 Skill 失败:', error);
      showError('安装失败', '无法安装 Skill，请检查路径');
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

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">Skills</h3>
        <Button size="sm" onClick={() => setShowInstallDialog(true)}>
          <Plus className="w-4 h-4 mr-2" />
          安装 Skill
        </Button>
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
                    <Badge variant="outline">{skill.source_type}</Badge>
                  </div>
                  {skill.description && (
                    <div className="text-sm text-muted-foreground mt-1">{skill.description}</div>
                  )}
                </div>
                <div className="flex items-center gap-2">
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

      {/* 安装 Skill 对话框 */}
      {showInstallDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <Card className="max-w-md w-full mx-4">
            <CardContent className="p-6">
              <h3 className="text-lg font-medium mb-4">安装 Skill</h3>
              <div className="space-y-4">
                <div>
                  <Label htmlFor="skillPath">Skill 路径</Label>
                  <Input
                    id="skillPath"
                    value={installPath}
                    onChange={(e) => setInstallPath(e.target.value)}
                    placeholder="/path/to/skill"
                  />
                  <p className="text-xs text-muted-foreground mt-2">
                    请输入包含 SKILL.md 的目录路径
                  </p>
                </div>
              </div>
              <div className="flex justify-end gap-2 mt-6">
                <Button variant="ghost" onClick={() => setShowInstallDialog(false)}>
                  取消
                </Button>
                <Button onClick={handleInstallSkill} disabled={!installPath}>
                  安装
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
  const { showSuccess, showError } = useToast();

  const loadConfig = async () => {
    setIsLoading(true);
    try {
      const cfg = await api.getServerConfig();
      setConfig(cfg);
      setEditHost(cfg.host);
      setEditPort(String(cfg.port));
      setEditAuthToken('');
    } catch (error) {
      console.error('加载 Server 配置失败:', error);
      showError('加载失败', '无法加载 Server 配置');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
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
          <div className="flex items-center gap-2 mb-4">
            <span className="text-sm text-muted-foreground">状态：</span>
            <Badge variant={config.running ? 'default' : 'secondary'}>
              {config.running ? '运行中' : '未运行'}
            </Badge>
          </div>

          <div className="space-y-2">
            <Label htmlFor="serverHost">监听地址</Label>
            <Input
              id="serverHost"
              value={editHost}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditHost(e.target.value)}
              placeholder="127.0.0.1"
              disabled={isSaving}
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
              disabled={isSaving}
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
              disabled={isSaving}
            />
            <p className="text-xs text-muted-foreground">
              当前: {config.auth_token_masked}（留空则保持不变）
            </p>
          </div>

          <div className="flex justify-end gap-2 pt-4">
            <Button onClick={handleSave} disabled={isSaving}>
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
// Connector 设置组件
// ============================================================================

function ConnectorSettings() {
  const [connectors, setConnectors] = useState<ConnectorInfo[]>([]);
  const [isLoading, setIsLoading] = useState(false);
  const { showSuccess, showError } = useToast();

  const loadConnectors = async () => {
    setIsLoading(true);
    try {
      const data = await api.getConnectors();
      setConnectors(data);
    } catch (error) {
      console.error('加载 Connector 列表失败:', error);
      showError('加载失败', '无法加载 Connector 列表');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConnectors();
  }, []);

  const handleToggleEnabled = async (name: string, enabled: boolean) => {
    try {
      await api.setConnectorEnabled(name, enabled);
      showSuccess('状态更新', `Connector "${name}" 已${enabled ? '启用' : '禁用'}`);
      loadConnectors();
    } catch (error) {
      console.error('切换 Connector 状态失败:', error);
      showError('操作失败', '无法更新 Connector 状态');
    }
  };

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">Connectors</h3>
      </div>

      {isLoading ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : connectors.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">
          <p>暂无已配置的 Connector</p>
          <p className="text-xs mt-2">请在 ~/.tiangong/connectors.json 中添加配置</p>
        </div>
      ) : (
        <div className="space-y-2">
          {connectors.map((connector) => (
            <Card key={connector.name}>
              <CardContent className="p-4 flex items-center justify-between">
                <div className="flex-1">
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{connector.name}</span>
                    <Badge variant="outline">{connector.connector_type}</Badge>
                    <Badge variant={connector.enabled ? 'default' : 'secondary'}>
                      {connector.enabled ? '已启用' : '已禁用'}
                    </Badge>
                  </div>
                </div>
                <Switch
                  checked={connector.enabled}
                  onCheckedChange={(checked) => handleToggleEnabled(connector.name, checked)}
                />
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </div>
  );
}
