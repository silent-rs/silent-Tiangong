// 打包步骤：组装 release/ —— plugin.json + 打包后的 sidecar 单文件 + 内容
// 哈希清单（本地信任锚）。无 UI、无页面资源。
import { createHash } from 'node:crypto';
import { cp, mkdir, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join, relative } from 'node:path';

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

async function walk(root) {
  const out = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) {
      out.push(...(await walk(path)));
    } else {
      out.push(path);
    }
  }
  return out;
}

const manifest = JSON.parse(await readFile('plugin.json', 'utf8'));
const sidecar = manifest.sidecar;
if (!sidecar || sidecar.runtime !== 'node' || !sidecar.entry) {
  throw new Error('plugin.json 必须声明 sidecar.runtime=node 与 entry');
}
const bundled = 'build/sidecar-main.mjs';
if (!(await exists(bundled))) {
  throw new Error('缺少构建产物 build/sidecar-main.mjs，先执行 yarn run build');
}
if ((manifest.ui?.contributions ?? []).length > 0) {
  throw new Error('node-tool 模板是无界面形态：plugin.json 不应声明 ui.contributions');
}

await rm('release', { recursive: true, force: true });
await mkdir('release/sidecar', { recursive: true });
await cp('plugin.json', 'release/plugin.json');
await cp(bundled, `release/${sidecar.entry}`);

const files = [];
for (const path of await walk('release')) {
  const rel = relative('release', path).replaceAll('\\', '/');
  if (rel === 'content-manifest.json') {
    continue;
  }
  const raw = await readFile(path);
  files.push({ path: rel, sha256: createHash('sha256').update(raw).digest('hex') });
}
await writeFile(
  'release/content-manifest.json',
  `${JSON.stringify({ algorithm: 'sha256', files }, null, 2)}\n`,
);
await writeFile(
  'release/.package-info',
  JSON.stringify({ id: manifest.id, version: manifest.version, entry: sidecar.entry }, null, 2),
);
console.log(JSON.stringify({ ok: true, files: files.length }));
