// 构建步骤（前端工程 + sidecar 打包）：
// 1. vue-tsc 类型检查页面 TS；
// 2. vite 将页面打成单文件 dist/index.html（Shadow 容器动态注入）；
// 3. esbuild 将 sidecar/main.mjs 连同协议库与全部 dependencies 打成单个
//    自包含 ESM 文件——运行时零依赖、零网络、可哈希锁定。
// （原生 .node 模块暂不支持打包，见模板 README。）
import { spawnSync } from 'node:child_process';
import { rm, mkdir } from 'node:fs/promises';
import { build as viteBuild } from 'vite';
import { build as esbuildBuild } from 'esbuild';

const typecheck = spawnSync('yarn', ['run', 'typecheck'], { stdio: 'inherit' });
if (typecheck.status !== 0) {
  throw new Error(`类型检查失败（退出码 ${typecheck.status}）`);
}

await viteBuild();

await rm('build', { recursive: true, force: true });
await mkdir('build', { recursive: true });

await esbuildBuild({
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
