import { execSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const pluginRoot = join(root, '..');
const workspaceRoot = join(pluginRoot, '..', '..');
const release = join(pluginRoot, 'release');
const installed = join(homedir(), '.tiangong', 'plugins', 'screenshot-input');

if (!process.env.TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH) {
  throw new Error('完整 sidecar 插件包需要 TIANGONG_PLUGIN_SIGNING_PRIVATE_KEY_PATH');
}

console.log('[package] vite build...');
execSync('yarn build', { cwd: pluginRoot, stdio: 'inherit', timeout: 10 * 60 * 1000 });

console.log('[package] 构建并签名 WASM 与当前平台 sidecar...');
execSync('cargo run -p xtask -- build-plugin screenshot-input', {
  cwd: workspaceRoot,
  stdio: 'inherit',
  timeout: 30 * 60 * 1000,
  env: {
    ...process.env,
    TIANGONG_PLUGIN_PREBUILT_UI: join(pluginRoot, 'dist', 'index.html'),
  },
});

console.log('[package] 组装插件包 release/...');
rmSync(release, { recursive: true, force: true });
mkdirSync(release, { recursive: true });
for (const file of [
  'plugin.json',
  'tiangong_plugin_screenshot_input_wasm.wasm',
  `tiangong-screenshot-input-sidecar${process.platform === 'win32' ? '.exe' : ''}`,
  'release.json',
  'release.json.sig',
]) {
  readFileSync(join(installed, file));
  cpSync(join(installed, file), join(release, file));
}
cpSync(join(installed, 'dist'), join(release, 'dist'), { recursive: true });

const manifest = JSON.parse(readFileSync(join(release, 'plugin.json'), 'utf8'));
const entry = manifest.ui?.contributions?.[0]?.entry;
if (!entry) throw new Error('plugin.json 缺少 ui.contributions[].entry');
readFileSync(join(release, entry));
const fingerprint = `id=${manifest.id} version=${manifest.version} platform=${process.platform} entry=${entry}`;
writeFileSync(join(release, '.package-info'), `${fingerprint}\n`);
console.log(`[package] 完成: release/（${fingerprint}）`);
console.log('[package] 天工「设置 → 插件管理 → 导入本地插件」选择 release 目录');
