import { execSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = dirname(fileURLToPath(import.meta.url));
const pluginRoot = join(root, '..');
const release = join(pluginRoot, 'release');

console.log('[package] vite build...');
execSync('yarn build', { cwd: pluginRoot, stdio: 'inherit' });

console.log('[package] 组装插件包 release/...');
rmSync(release, { recursive: true, force: true });
mkdirSync(release, { recursive: true });
cpSync(join(pluginRoot, 'plugin.json'), join(release, 'plugin.json'));
cpSync(join(pluginRoot, 'dist'), join(release, 'dist'), { recursive: true });

const manifest = JSON.parse(readFileSync(join(release, 'plugin.json'), 'utf8'));
const entry = manifest.ui?.contributions?.[0]?.entry;
if (!entry) throw new Error('plugin.json 缺少 ui.contributions[].entry');
readFileSync(join(release, entry));
const fingerprint = `id=${manifest.id} version=${manifest.version} entry=${entry}`;
writeFileSync(join(release, '.package-info'), `${fingerprint}\n`);
console.log(`[package] 完成: release/（${fingerprint}）`);
console.log('[package] 天工「设置 → 插件管理 → 导入本地插件」选择 release 目录');
