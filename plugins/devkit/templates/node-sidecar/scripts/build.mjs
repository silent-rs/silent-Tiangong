// 构建步骤：esbuild 将 sidecar/main.mjs 连同其 import 的协议库与全部
// dependencies 打成单个自包含 ESM 文件——运行时零依赖、零网络、可哈希锁定。
// （原生 .node 模块暂不支持打包，见模板 README。）

import { build } from 'esbuild';
import { rm, mkdir } from 'node:fs/promises';

await rm('build', { recursive: true, force: true });
await mkdir('build', { recursive: true });

await build({
  entryPoints: ['sidecar/main.mjs'],
  bundle: true,
  platform: 'node',
  format: 'esm',
  target: 'node20',
  outfile: 'build/sidecar-main.mjs',
  // 相对导入（含 vendor 协议库）与 dependencies 全部内联；node 内置模块保持外置。
  packages: 'bundle',
  external: [],
  // ESM 输出下 CJS 依赖的 require('node:内置') 需要 shim（esbuild 官方推荐）。
  banner: {
    js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);",
  },
  minify: false,
  sourcemap: false,
  logLevel: 'info',
});
