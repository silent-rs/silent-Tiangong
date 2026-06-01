import { useState, useEffect } from 'react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from './ui/select';
import { Settings, Eye, EyeOff } from 'lucide-react';
import { api } from '@/api/tauri';
import type { ModelsConfigView, ProviderConfigView } from '@/api/tauri';

export function SettingsButton() {
  const [open, setOpen] = useState(false);
  const [showApiKey, setShowApiKey] = useState(false);
  const [config, setConfig] = useState<ModelsConfigView>({
    providers: {},
    routing: {},
  });
  const [originalConfig, setOriginalConfig] = useState<ModelsConfigView>({ ...config });
  const [isSaving, setIsSaving] = useState(false);

  const loadConfig = async () => {
    try {
      const cfg = await api.getModelsConfig();
      setConfig(cfg);
      setOriginalConfig(JSON.parse(JSON.stringify(cfg)));
    } catch (error) {
      console.error('加载配置失败:', error);
    }
  };

  useEffect(() => {
    if (open) {
      loadConfig();
    }
  }, [open]);

  const handleSave = async () => {
    setIsSaving(true);
    try {
      await api.setModelsConfig(config);
      setOriginalConfig(JSON.parse(JSON.stringify(config)));
      setOpen(false);
    } catch (error) {
      console.error('保存配置失败:', error);
    } finally {
      setIsSaving(false);
    }
  };

  const handleCancel = () => {
    setConfig(JSON.parse(JSON.stringify(originalConfig)));
    setOpen(false);
  };

  const hasChanges = JSON.stringify(config) !== JSON.stringify(originalConfig);

  // 获取第一个 provider 进行快速编辑
  const providerKeys = Object.keys(config.providers);
  const firstKey = providerKeys[0] || 'default';
  const firstProvider: ProviderConfigView = config.providers[firstKey] || {
    base_url: '',
    api_key: '',
    timeout_ms: 60000,
    protocol: 'openai_compatible',
  };

  const updateProvider = (updates: Partial<ProviderConfigView>) => {
    setConfig({
      ...config,
      providers: {
        ...config.providers,
        [firstKey]: { ...firstProvider, ...updates },
      },
    });
  };

  return (
    <>
      <Button
        variant="ghost"
        className="text-[#CCCCCC] hover:bg-[#2A2D2E] hover:text-white"
        onClick={() => setOpen(true)}
      >
        <Settings className="w-4 h-4 mr-2" />
        LLM 设置
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="bg-[#252526] border-[#3C3C3C] text-white max-w-md">
          <DialogHeader>
            <DialogTitle>LLM 快速配置 ({firstKey})</DialogTitle>
          </DialogHeader>

          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="apiKey">API Key</Label>
              <div className="relative">
                <Input
                  id="apiKey"
                  type={showApiKey ? 'text' : 'password'}
                  value={firstProvider.api_key}
                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => updateProvider({ api_key: e.target.value })}
                  className="bg-[#1E1E1E] border-[#3C3C3C] text-white pr-10"
                  placeholder="sk-... 或 ${ENV_VAR}"
                />
                <button
                  type="button"
                  onClick={() => setShowApiKey(!showApiKey)}
                  className="absolute right-2 top-1/2 -translate-y-1/2 text-[#858585] hover:text-white"
                >
                  {showApiKey ? <EyeOff className="w-4 h-4" /> : <Eye className="w-4 h-4" />}
                </button>
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="baseUrl">Base URL</Label>
              <Input
                id="baseUrl"
                value={firstProvider.base_url}
                onChange={(e: React.ChangeEvent<HTMLInputElement>) => updateProvider({ base_url: e.target.value })}
                className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                placeholder="https://api.openai.com/v1"
              />
            </div>

            <div className="space-y-2">
              <Label>协议类型</Label>
              <Select
                value={firstProvider.protocol || 'openai_compatible'}
                onValueChange={(value) => updateProvider({ protocol: value })}
              >
                <SelectTrigger className="bg-[#1E1E1E] border-[#3C3C3C] text-white">
                  <SelectValue placeholder="选择协议" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="openai_compatible">OpenAI 兼容</SelectItem>
                  <SelectItem value="anthropic">Anthropic</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="timeout">超时时间 (毫秒)</Label>
              <Input
                id="timeout"
                type="number"
                value={firstProvider.timeout_ms}
                onChange={(e: React.ChangeEvent<HTMLInputElement>) => updateProvider({ timeout_ms: parseInt(e.target.value) || 60000 })}
                className="bg-[#1E1E1E] border-[#3C3C3C] text-white"
                placeholder="60000"
              />
            </div>

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
                {isSaving ? '保存中...' : '保存'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
