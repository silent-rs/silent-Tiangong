// 开发期打包：构建 UI 并组装可直接安装的插件包目录（release/）。
// 产物 = plugin.json + dist/ + 内容树清单（路径 + sha256 逐条，供信任哈希锁定消费）。
// plugin creator 的「构建」按钮与本脚本执行同一流程。
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

const manifest = JSON.parse(readFileSync(join(release, 'plugin.json'), 'utf8'));
const entry = manifest.ui?.contributions?.[0]?.entry;
if (!entry) throw new Error('plugin.json 缺少 ui.contributions[].entry');
readFileSync(join(release, entry));

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

writeFileSync(join(release, '.package-info'), `id=${manifest.id} version=${manifest.version} entry=${entry}\n`);
console.log(`[package] 完成: release/（${files.length} 个文件）`);
