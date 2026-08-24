// 开发期打包：构建 UI 并组装可直接「导入本地插件」的插件包目录（release/）。
// 产物 = plugin.json + dist/ + templates/（随包资源，plugin-dev.init 的模板源）
// + 内容树清单（路径 + sha256 逐条，供信任哈希锁定直接消费）。
import { cpSync, mkdirSync, rmSync, readFileSync, writeFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

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
cpSync(join(pluginRoot, 'templates'), join(release, 'templates'), { recursive: true });

// 打包即校验：清单、entry 与 resources 就位，避免导入时才发现问题
const manifest = JSON.parse(readFileSync(join(release, 'plugin.json'), 'utf8'));
const entry = manifest.ui?.contributions?.[0]?.entry;
if (!entry) throw new Error('plugin.json 缺少 ui.contributions[].entry');
readFileSync(join(release, entry));
for (const dir of manifest.resources ?? []) {
  statSync(join(release, dir));
}

// 内容树清单：release/ 内除清单自身外全部文件的路径 + sha256
function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...walk(path));
    else out.push(path);
  }
  return out;
}
const files = walk(release)
  .filter((path) => !path.endsWith('content-manifest.json'))
  .sort()
  .map((path) => ({
    path: relative(release, path).split('\\').join('/'),
    sha256: createHash('sha256').update(readFileSync(path)).digest('hex'),
  }));
writeFileSync(
  join(release, 'content-manifest.json'),
  `${JSON.stringify({ algorithm: 'sha256', files }, null, 2)}\n`,
);

const fingerprint = `id=${manifest.id} version=${manifest.version} entry=${entry} files=${files.length}`;
writeFileSync(join(release, '.package-info'), `${fingerprint}\n`);
console.log(`[package] 完成: release/（${fingerprint}）`);
console.log('[package] 天工「设置 → 插件管理 → 导入本地插件」选择 release 目录');
