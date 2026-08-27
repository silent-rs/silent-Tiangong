// add：为工程模板项目添加依赖（yarn add --exact）。
// 面向 Agent/命令通道：与 build 一致的项目定位、包名校验、进程管理、
// 超时与日志；版本默认锁定精确版本（可复现构建，与 ts-npx 的精确版本先例一致）。
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fail, requireProject } from './common.mjs';
import { runYarn } from './build.mjs';

const ADD_TIMEOUT_MS = 240_000;

// npm 包规格 [@scope/]name[@range]。range 仅允许语义版本字符；
// file:/git/http 等本地或源码依赖一律拒绝（供应链只走 registry）。
const PACKAGE_SPEC =
  /^(@[a-z0-9-~][a-z0-9-._~]*\/)?[a-z0-9-~][a-z0-9-._~]*(@[\w.^~*>=<| -]+)?$/i;

export async function add(argv, ctx) {
  const [id, ...rest] = argv;
  if (!id || rest.length === 0) {
    return fail('用法：add <id> <包名...> [--dev]');
  }
  const dev = rest.includes('--dev');
  const packages = rest.filter((item) => item !== '--dev');
  const invalid = packages.filter((item) => !PACKAGE_SPEC.test(item));
  if (invalid.length > 0) {
    return fail(
      `包名不合法: ${invalid.join('、')}（仅接受 npm registry 包规格 [@scope/]name[@range]）`,
    );
  }
  let projectDir;
  try {
    projectDir = requireProject(ctx, id);
  } catch (error) {
    return fail(error.message);
  }
  if (!existsSync(join(projectDir, 'package.json'))) {
    return fail('项目缺少 package.json（add 仅适用于工程模板，如 ts-tool / node-sidecar）');
  }

  const args = ['add', ...packages, '--exact'];
  if (dev) {
    args.push('--dev');
  }
  const started = Date.now();
  const { code, output, spawnError } = await runYarn(projectDir, args, ADD_TIMEOUT_MS);
  if (spawnError) {
    return { ok: false, error: `启动 yarn 失败：${spawnError}` };
  }
  if (code !== 0) {
    return {
      ok: false,
      error: `yarn add 失败（退出码 ${code}）`,
      log_tail: output.slice(-4096),
      hint: '完整日志见项目 logs/build.log',
    };
  }
  return { ok: true, added: packages, dev, duration_ms: Date.now() - started };
}
