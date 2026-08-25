// sidecar 打包：esbuild 将 sidecar/main.mjs 连同 devkit 源与协议库打进
// 单个自包含 ESM 文件（运行时零依赖、零网络；devkit 的 npx CLI 形态保留）。
import { rm, mkdir } from 'node:fs/promises';
import { build } from 'esbuild';

await rm('build', { recursive: true, force: true });
await mkdir('build', { recursive: true });

await build({
  entryPoints: ['sidecar/main.mjs'],
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  outfile: 'build/sidecar-main.mjs',
  packages: 'bundle',
  external: [],
  banner: {
    js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);",
  },
  minify: false,
  sourcemap: false,
  logLevel: 'info',
});
