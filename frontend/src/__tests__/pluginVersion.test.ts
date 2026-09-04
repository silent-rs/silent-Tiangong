import { describe, expect, it } from 'vitest';

import { isRemotePluginVersionNewer } from '@/lib/pluginVersion';

describe('插件升级版本判断', () => {
  it('版本一致时不提示升级', () => {
    expect(isRemotePluginVersionNewer('0.1.2', '0.1.2')).toBe(false);
  });

  it('线上版本更高时提示升级', () => {
    expect(isRemotePluginVersionNewer('0.1.2', '0.1.3')).toBe(true);
  });

  it('本地版本高于线上版本时不提示升级', () => {
    expect(isRemotePluginVersionNewer('0.1.3', '0.1.2')).toBe(false);
  });

  it('按语义版本比较多位数字', () => {
    expect(isRemotePluginVersionNewer('0.1.9', '0.1.10')).toBe(true);
    expect(isRemotePluginVersionNewer('0.1.10', '0.1.9')).toBe(false);
  });

  it('正式版高于预发布版，构建信息不改变优先级', () => {
    expect(isRemotePluginVersionNewer('1.0.0-beta.2', '1.0.0')).toBe(true);
    expect(isRemotePluginVersionNewer('1.0.0+local', '1.0.0+remote')).toBe(false);
  });

  it('版本无效时不误提示升级', () => {
    expect(isRemotePluginVersionNewer('dev', '0.1.2')).toBe(false);
    expect(isRemotePluginVersionNewer('0.1.2', 'latest')).toBe(false);
  });
});
