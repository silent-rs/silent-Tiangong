// 打包步骤：组装 release/ —— plugin.json + 零构建页面 + 打包后的 sidecar
// 单文件，并生成内容哈希清单（本地信任锚）。产物不包含源码、vendor 与
// node_modules；安装后的运行时零依赖。

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
const uiEntries = (manifest.ui?.contributions ?? []).map((item) => item.entry);
for (const entry of uiEntries) {
  if (!(await exists(entry))) {
    throw new Error(`UI 入口不存在: ${entry}`);
  }
}

await rm('release', { recursive: true, force: true });
await mkdir('release/sidecar', { recursive: true });
await cp('plugin.json', 'release/plugin.json');
for (const entry of uiEntries) {
  const info = await stat(entry);
  if (info.isDirectory()) {
    await cp(entry, join('release', entry), { recursive: true });
  } else {
    await cp(entry, join('release', entry));
  }
}
await cp(bundled, `release/${sidecar.entry}`);

// 内容清单：release 全树（排除清单自身），路径 + sha256——本地信任锚。
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
