import { useState, useEffect } from 'react';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from './ui/dialog';
import { Button } from './ui/button';
import { Input } from './ui/input';
import { Label } from './ui/label';
import { Settings, Eye, EyeOff } from 'lucide-react';
import { api } from '@/api/tauri';
import type { ModelConfig } from '@/api/tauri';

export function SettingsButton() {
  const [open, setOpen] = useState(false);
  const [showToken, setShowToken] = useState(false);
  const [config, setConfig] = useState<ModelConfig>({
    api_auth_token: '',
    api_base_url: '',
    api_timeout_ms: '',
    api_model: '',
  });
  const [originalConfig, setOriginalConfig] = useState<ModelConfig>({ ...config });
  const [isSaving, setIsSaving] = useState(false);

  const loadConfig = async () => {
    try {
      const cfg = await api.get_model_config();
      setConfig(cfg);
      setOriginalConfig(cfg);
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
      await api.set_model_config(
        config.api_auth_token || undefined,
        config.api_base_url || undefined,
        config.api_timeout_ms || undefined,
        config.api_model || undefined,
      );
      setOriginalConfig({ ...config });
      setOpen(false);
    } catch (error) {
      console.error('保存配置失败:', error);
    } finally {
      setIsSaving(false);
    }
  };

  const handleCancel = () => {
    setConfig({ ...originalConfig });
    setOpen(false);
  };

  const hasChanges = Object.keys(config).some(
    key => config[key as keyof typeof config] !== originalConfig[key as keyof typeof originalConfig]
  );

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
            <DialogTitle>LLM 配置</DialogTitle>
          </DialogHeader>

          <div className="space-y-4">
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
                {isSaving ? '保存中...' : '保存'}
              </Button>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
