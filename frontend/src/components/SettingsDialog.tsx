import { useState, useEffect } from 'react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Settings, Eye, EyeOff, Server, Puzzle, Plus, Trash2, Power, Loader2, Globe, Link, Image } from 'lucide-react';
import { api } from '@/api/tauri';
import type { ModelConfig, McpServer, Skill, ServerConfig, ConnectorInfo, MediaConfig } from '@/api/tauri';
import { useToast } from './Toast';

type TabType = 'llm' | 'mcp' | 'skill' | 'server' | 'connector' | 'media';

export function SettingsDialog() {
  const [open, setOpen] = useState(false);
  const [activeTab, setActiveTab] = useState<TabType>('llm');

  return (
    <>
      <Button
        variant="ghost"
        className="w-full justify-start text-[#CCCCCC] hover:bg-[#2A2D2E] hover:text-white"
        onClick={() => setOpen(true)}
      >
        <Settings className="w-4 h-4 mr-2" />
        设置
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="bg-[#252526] border-[#3C3C3C] text-white max-w-4xl max-h-[80vh] overflow-hidden flex flex-col">
          <DialogHeader>
            <DialogTitle>系统设置</DialogTitle>
          </DialogHeader>

          {/* 标签页导航 */}
          <div className="flex gap-1 border-b border-[#3C3C3C]">
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'llm'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('llm')}
            >
              <Settings className="w-4 h-4 inline mr-2" />
              LLM 配置
            </button>
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'mcp'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('mcp')}
            >
              <Server className="w-4 h-4 inline mr-2" />
              MCP 服务器
            </button>
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'skill'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('skill')}
            >
              <Puzzle className="w-4 h-4 inline mr-2" />
              Skills
            </button>
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'server'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('server')}
            >
              <Globe className="w-4 h-4 inline mr-2" />
              Server
            </button>
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'connector'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('connector')}
            >
              <Link className="w-4 h-4 inline mr-2" />
              Connectors
            </button>
            <button
              className={`px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === 'media'
                  ? 'text-[#10A37F] border-b-2 border-[#10A37F]'
                  : 'text-[#CCCCCC] hover:text-white'
              }`}
              onClick={() => setActiveTab('media')}
            >
              <Image className="w-4 h-4 inline mr-2" />
              多媒体
            </button>
          </div>

          {/* 标签页内容 */}
          <div className="flex-1 overflow-y-auto">
            {activeTab === 'llm' && <LLMSettings onClose={() => setOpen(false)} />}
            {activeTab === 'mcp' && <McpSettings />}
            {activeTab === 'skill' && <SkillSettings />}
            {activeTab === 'server' && <ServerSettings />}
            {activeTab === 'connector' && <ConnectorSettings />}
            {activeTab === 'media' && <MediaSettings />}
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

// ============================================================================
// LLM 设置组件
// ============================================================================

function LLMSettings({ onClose }: { onClose: () => void }) {
  const [showToken, setShowToken] = useState(false);
  const [config, setConfig] = useState<ModelConfig>({
    api_auth_token: '',
    api_base_url: '',
    api_timeout_ms: '',
    api_model: '',
  });
  const [originalConfig, setOriginalConfig] = useState<ModelConfig>({ ...config });
  const [isSaving, setIsSaving] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const { showSuccess, showError } = useToast();

  const loadConfig = async () => {
    setIsLoading(true);
    try {
      const cfg = await api.get_model_config();
      setConfig(cfg);
      setOriginalConfig(cfg);
    } catch (error) {
      console.error('加载配置失败:', error);
      showError('加载失败', '无法加载 LLM 配置，请重试');
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
      await api.set_model_config(
        config.api_auth_token || undefined,
        config.api_base_url || undefined,
        config.api_timeout_ms || undefined,
        config.api_model || undefined,
      );
      setOriginalConfig({ ...config });
      showSuccess('保存成功', 'LLM 配置已更新');
      onClose();
    } catch (error) {
      console.error('保存配置失败:', error);
      showError('保存失败', '无法保存 LLM 配置，请重试');
    } finally {
      setIsSaving(false);
    }
  };

  const handleCancel = () => {
    setConfig({ ...originalConfig });
    onClose();
  };

  const hasChanges = Object.keys(config).some(
    key => config[key as keyof typeof config] !== originalConfig[key as keyof typeof originalConfig]
  );

  return (
    <div className="space-y-4 p-4">
      {isLoading && (
        <div className="flex items-center justify-center py-8">
          <Loader2 className="w-6 h-6 animate-spin text-[#10A37F] mr-2" />
          <span className="text-sm text-[#858585]">加载配置中...</span>
        </div>
      )}

      {!isLoading && (
        <>
          {/* API Auth Token */}
          <div className="space-y-2">
            <Label htmlFor="token">API Token</Label>
            <div className="relative">
              <Input
                id="token"
                type={showToken ? 'text' : 'password'}
                value={config.api_auth_token}
                onChange={(e: React.ChangeEvent<HTMLInputElement>) => setConfig({ ...config, api_auth_token: e.target.value })}
                className="bg-[#1E1E1E] border-[#3C3C3C] text-white pr-10"
                placeholder="sk-..."
                disabled={isSaving}
              />
              <button
                type="button"
                onClick={() => setShowToken(!showToken)}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-[#858585] hover:text-white"
              >
                {showToken ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
              </button>
            </div>
          </div>

          {/* API Base URL */}
          <div className="space-y-2">
            <Label htmlFor="baseUrl">Base URL</Label>
            <Input
              id="baseUrl"
              value={config.api_base_url}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setConfig({ ...config, api_base_url: e.target.value })}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder="https://api.openai.com/v1"
              disabled={isSaving}
            />
          </div>

          {/* API Timeout */}
          <div className="space-y-2">
            <Label htmlFor="timeout">超时时间 (毫秒)</Label>
            <Input
              id="timeout"
              type="number"
              value={config.api_timeout_ms}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setConfig({ ...config, api_timeout_ms: e.target.value })}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder="60000"
              disabled={isSaving}
            />
          </div>

          {/* API Model */}
          <div className="space-y-2">
            <Label htmlFor="model">模型名称</Label>
            <Input
              id="model"
              value={config.api_model}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setConfig({ ...config, api_model: e.target.value })}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder="gpt-4"
              disabled={isSaving}
            />
          </div>

          {/* 按钮 */}
          <div className="flex justify-end gap-2 pt-4">
            <Button
              variant="ghost"
              className="text-[#CCCCCC] hover:text-white hover:bg-[#2A2D2E]"
              onClick={handleCancel}
              disabled={isSaving}
            >
              取消
            </Button>
            <Button
              className="bg-[#10A37F] hover:bg-[#0D8A6A]"
              onClick={handleSave}
              disabled={isSaving || !hasChanges}
            >
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
        <Button
          size="sm"
          className="bg-[#10A37F] hover:bg-[#0D8A6A]"
          onClick={() => setShowAddDialog(true)}
        >
          <Plus className="w-4 h-4 mr-2" />
          添加服务器
        </Button>
      </div>

      {isLoading ? (
        <div className="text-center text-[#858585] py-8">加载中...</div>
      ) : servers.length === 0 ? (
        <div className="text-center text-[#858585] py-8">暂无 MCP 服务器</div>
      ) : (
        <div className="space-y-2">
          {servers.map((server) => (
            <div
              key={server.name}
              className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between"
            >
              <div className="flex-1">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{server.name}</span>
                  <span className={`px-2 py-0.5 text-xs rounded ${
                    server.enabled
                      ? 'bg-[#10A37F]/20 text-[#10A37F]'
                      : 'bg-[#3C3C3C] text-[#858585]'
                  }`}>
                    {server.enabled ? '已启用' : '已禁用'}
                  </span>
                </div>
                <div className="text-sm text-[#858585] mt-1">
                  {server.command} {server.args.join(' ')}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="icon"
                  className="hover:bg-[#2A2D2E]"
                  onClick={() => handleToggleEnabled(server.name, !server.enabled)}
                  title={server.enabled ? '禁用' : '启用'}
                >
                  <Power className={`w-4 h-4 ${server.enabled ? 'text-[#10A37F]' : 'text-[#858585]'}`} />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="hover:bg-red-500/20 hover:text-red-400"
                  onClick={() => handleRemoveServer(server.name)}
                  title="删除"
                >
                  <Trash2 className="w-4 h-4" />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* 添加服务器对话框 */}
      {showAddDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-[#252526] border border-[#3C3C3C] rounded-lg p-6 max-w-md w-full mx-4">
            <h3 className="text-lg font-medium mb-4">添加 MCP 服务器</h3>
            <div className="space-y-4">
              <div>
                <Label htmlFor="serverName">名称</Label>
                <Input
                  id="serverName"
                  value={newServer.name}
                  onChange={(e) => setNewServer({ ...newServer, name: e.target.value })}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                  placeholder="my-mcp-server"
                />
              </div>
              <div>
                <Label htmlFor="serverCommand">命令</Label>
                <Input
                  id="serverCommand"
                  value={newServer.command}
                  onChange={(e) => setNewServer({ ...newServer, command: e.target.value })}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                  placeholder="npx"
                />
              </div>
              <div>
                <Label htmlFor="serverArgs">参数（空格分隔）</Label>
                <Input
                  id="serverArgs"
                  value={newServer.args}
                  onChange={(e) => setNewServer({ ...newServer, args: e.target.value })}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                  placeholder="-y @modelcontextprotocol/server-filesystem"
                />
              </div>
              <div>
                <Label htmlFor="serverEnv">环境变量（逗号分隔，格式：KEY=VALUE）</Label>
                <Input
                  id="serverEnv"
                  value={newServer.env}
                  onChange={(e) => setNewServer({ ...newServer, env: e.target.value })}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                  placeholder="PATH=/usr/bin,NODE_ENV=production"
                />
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <Button
                variant="ghost"
                className="text-[#CCCCCC] hover:text-white hover:bg-[#2A2D2E]"
                onClick={() => setShowAddDialog(false)}
              >
                取消
              </Button>
              <Button
                className="bg-[#10A37F] hover:bg-[#0D8A6A]"
                onClick={handleAddServer}
                disabled={!newServer.name || !newServer.command}
              >
                添加
              </Button>
            </div>
          </div>
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
        <Button
          size="sm"
          className="bg-[#10A37F] hover:bg-[#0D8A6A]"
          onClick={() => setShowInstallDialog(true)}
        >
          <Plus className="w-4 h-4 mr-2" />
          安装 Skill
        </Button>
      </div>

      {isLoading ? (
        <div className="text-center text-[#858585] py-8">加载中...</div>
      ) : skills.length === 0 ? (
        <div className="text-center text-[#858585] py-8">暂无已安装的 Skills</div>
      ) : (
        <div className="space-y-2">
          {skills.map((skill) => (
            <div
              key={skill.id}
              className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between"
            >
              <div className="flex-1">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{skill.name}</span>
                  <span className={`px-2 py-0.5 text-xs rounded ${
                    skill.enabled
                      ? 'bg-[#10A37F]/20 text-[#10A37F]'
                      : 'bg-[#3C3C3C] text-[#858585]'
                  }`}>
                    {skill.enabled ? '已启用' : '已禁用'}
                  </span>
                  <span className="px-2 py-0.5 text-xs rounded bg-[#3C3C3C] text-[#858585]">
                    {skill.source_type}
                  </span>
                </div>
                {skill.description && (
                  <div className="text-sm text-[#858585] mt-1">{skill.description}</div>
                )}
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="icon"
                  className="hover:bg-[#2A2D2E]"
                  onClick={() => handleToggleEnabled(skill.id, !skill.enabled)}
                  title={skill.enabled ? '禁用' : '启用'}
                >
                  <Power className={`w-4 h-4 ${skill.enabled ? 'text-[#10A37F]' : 'text-[#858585]'}`} />
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="hover:bg-red-500/20 hover:text-red-400"
                  onClick={() => handleRemoveSkill(skill.id)}
                  title="删除"
                >
                  <Trash2 className="w-4 h-4" />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* 安装 Skill 对话框 */}
      {showInstallDialog && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
          <div className="bg-[#252526] border border-[#3C3C3C] rounded-lg p-6 max-w-md w-full mx-4">
            <h3 className="text-lg font-medium mb-4">安装 Skill</h3>
            <div className="space-y-4">
              <div>
                <Label htmlFor="skillPath">Skill 路径</Label>
                <Input
                  id="skillPath"
                  value={installPath}
                  onChange={(e) => setInstallPath(e.target.value)}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                  placeholder="/path/to/skill"
                />
                <p className="text-xs text-[#858585] mt-2">
                  请输入包含 SKILL.md 的目录路径
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-2 mt-6">
              <Button
                variant="ghost"
                className="text-[#CCCCCC] hover:text-white hover:bg-[#2A2D2E]"
                onClick={() => setShowInstallDialog(false)}
              >
                取消
              </Button>
              <Button
                className="bg-[#10A37F] hover:bg-[#0D8A6A]"
                onClick={handleInstallSkill}
                disabled={!installPath}
              >
                安装
              </Button>
            </div>
          </div>
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
          <Loader2 className="w-6 h-6 animate-spin text-[#10A37F] mr-2" />
          <span className="text-sm text-[#858585]">加载配置中...</span>
        </div>
      ) : (
        <>
          {/* 运行状态 */}
          <div className="flex items-center gap-2 mb-4">
            <span className="text-sm text-[#858585]">状态：</span>
            <span className={`px-2 py-0.5 text-xs rounded ${
              config.running
                ? 'bg-[#10A37F]/20 text-[#10A37F]'
                : 'bg-[#3C3C3C] text-[#858585]'
            }`}>
              {config.running ? '运行中' : '未运行'}
            </span>
          </div>

          {/* Host */}
          <div className="space-y-2">
            <Label htmlFor="serverHost">监听地址</Label>
            <Input
              id="serverHost"
              value={editHost}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditHost(e.target.value)}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder="127.0.0.1"
              disabled={isSaving}
            />
          </div>

          {/* Port */}
          <div className="space-y-2">
            <Label htmlFor="serverPort">端口</Label>
            <Input
              id="serverPort"
              type="number"
              value={editPort}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditPort(e.target.value)}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder="8080"
              disabled={isSaving}
            />
          </div>

          {/* Auth Token */}
          <div className="space-y-2">
            <Label htmlFor="serverAuthToken">认证 Token</Label>
            <Input
              id="serverAuthToken"
              type="password"
              value={editAuthToken}
              onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEditAuthToken(e.target.value)}
              className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
              placeholder={config.auth_token_masked || '留空表示不鉴权'}
              disabled={isSaving}
            />
            <p className="text-xs text-[#858585]">
              当前: {config.auth_token_masked}（留空则保持不变）
            </p>
          </div>

          {/* 保存按钮 */}
          <div className="flex justify-end gap-2 pt-4">
            <Button
              className="bg-[#10A37F] hover:bg-[#0D8A6A]"
              onClick={handleSave}
              disabled={isSaving}
            >
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
        <div className="text-center text-[#858585] py-8">加载中...</div>
      ) : connectors.length === 0 ? (
        <div className="text-center text-[#858585] py-8">
          <p>暂无已配置的 Connector</p>
          <p className="text-xs mt-2">请在 ~/.tiangong/connectors.json 中添加配置</p>
        </div>
      ) : (
        <div className="space-y-2">
          {connectors.map((connector) => (
            <div
              key={connector.name}
              className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between"
            >
              <div className="flex-1">
                <div className="flex items-center gap-2">
                  <span className="font-medium">{connector.name}</span>
                  <span className="px-2 py-0.5 text-xs rounded bg-[#3C3C3C] text-[#858585]">
                    {connector.connector_type}
                  </span>
                  <span className={`px-2 py-0.5 text-xs rounded ${
                    connector.enabled
                      ? 'bg-[#10A37F]/20 text-[#10A37F]'
                      : 'bg-[#3C3C3C] text-[#858585]'
                  }`}>
                    {connector.enabled ? '已启用' : '已禁用'}
                  </span>
                </div>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="icon"
                  className="hover:bg-[#2A2D2E]"
                  onClick={() => handleToggleEnabled(connector.name, !connector.enabled)}
                  title={connector.enabled ? '禁用' : '启用'}
                >
                  <Power className={`w-4 h-4 ${connector.enabled ? 'text-[#10A37F]' : 'text-[#858585]'}`} />
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// 多媒体设置组件
// ============================================================================

function MediaSettings() {
  const [config, setConfig] = useState<MediaConfig>({
    image_api_configured: false,
    stt_api_configured: false,
    tts_api_configured: false,
  });
  const [isLoading, setIsLoading] = useState(false);
  const { showError } = useToast();

  const loadConfig = async () => {
    setIsLoading(true);
    try {
      const cfg = await api.getMediaConfig();
      setConfig(cfg);
    } catch (error) {
      console.error('加载多媒体配置失败:', error);
      showError('加载失败', '无法加载多媒体配置');
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => {
    loadConfig();
  }, []);

  const StatusBadge = ({ configured }: { configured: boolean }) => (
    <span className={`px-2 py-0.5 text-xs rounded ${
      configured
        ? 'bg-[#10A37F]/20 text-[#10A37F]'
        : 'bg-[#3C3C3C] text-[#858585]'
    }`}>
      {configured ? '已配置' : '未配置'}
    </span>
  );

  return (
    <div className="p-4">
      <div className="flex items-center justify-between mb-4">
        <h3 className="text-lg font-medium">多媒体能力</h3>
      </div>

      {isLoading ? (
        <div className="text-center text-[#858585] py-8">加载中...</div>
      ) : (
        <div className="space-y-3">
          <div className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between">
            <div>
              <div className="font-medium">图片生成</div>
              <div className="text-sm text-[#858585] mt-1">
                支持 DALL-E / GPT-Image 等图片生成后端
              </div>
            </div>
            <StatusBadge configured={config.image_api_configured} />
          </div>

          <div className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between">
            <div>
              <div className="font-medium">语音识别 (STT)</div>
              <div className="text-sm text-[#858585] mt-1">
                支持 OpenAI Whisper 等语音识别后端
              </div>
            </div>
            <StatusBadge configured={config.stt_api_configured} />
          </div>

          <div className="bg-[#1E1E1E] border border-[#3C3C3C] rounded-lg p-4 flex items-center justify-between">
            <div>
              <div className="font-medium">语音合成 (TTS)</div>
              <div className="text-sm text-[#858585] mt-1">
                支持 OpenAI TTS 等语音合成后端
              </div>
            </div>
            <StatusBadge configured={config.tts_api_configured} />
          </div>

          <p className="text-xs text-[#858585] mt-4">
            多媒体 API Key 通过环境变量（TIANGONG_IMAGE_API_KEY / TIANGONG_STT_API_KEY / TIANGONG_TTS_API_KEY）或 ~/.tiangong/media.json 配置文件设置。
          </p>
        </div>
      )}
    </div>
  );
}
