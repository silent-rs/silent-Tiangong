// build：工程模板走 yarn install → build → package；零构建模板走内建打包
//（plugin.json + UI 入口目录 + resources 目录 + 内容树清单）。
// 全部路径经 resolveInside 约束在项目/发布目录内，复制逐项校验拒绝符号链接。
import { appendFileSync, copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { join, relative } from 'node:path';
import { fail, isRealDirectory, killTree, requireProject, resolveInside, spawnOptions } from './common.mjs';

const BUILD_TIMEOUT_MS = 240_000;

export async function build(argv, ctx) {
  const [id] = argv;
  if (!id) return fail('用法：build <id>');
  let projectDir;
  try {
    projectDir = requireProject(ctx, id);
  } catch (error) {
    return fail(error.message);
  }
  const started = Date.now();
  let result;
  if (existsSync(join(projectDir, 'package.json'))) {
    result = await yarnBuild(projectDir);
  } else {
    try {
      result = zeroBuild(projectDir);
    } catch (error) {
      result = { ok: false, error: error.message };
    }
  }
  return { ...result, duration_ms: Date.now() - started };
}

function detectToolchain() {
  const probe = spawnSync('yarn', ['--version'], { encoding: 'utf8' });
  if (probe.error || probe.status !== 0) {
    return '未找到 yarn。工程模板构建需要 Node ≥ 20 与 yarn，请安装后重试：https://yarnpkg.com';
  }
  const nodeMajor = Number(process.versions.node.split('.')[0]);
  if (nodeMajor < 20) {
    return `Node 版本过低（当前 ${process.versions.node}，要求 ≥ 20），请升级后重试：https://nodejs.org`;
  }
  return null;
}

async function yarnBuild(projectDir) {
  const toolchainError = detectToolchain();
  if (toolchainError) {
    return { ok: false, failed_step: 'toolchain', error: toolchainError };
  }
  const steps = [
    ['install', ['install', '--silent']],
    ['build', ['run', 'build']],
    ['package', ['run', 'package']],
  ];
  for (const [step, args] of steps) {
    const { code, output, spawnError } = await runYarn(projectDir, args);
    if (spawnError) {
      return { ok: false, failed_step: step, error: `启动 yarn 失败：${spawnError}` };
    }
    if (code !== 0) {
      return {
        ok: false,
        failed_step: step,
        error: `步骤 [${step}] 失败（退出码 ${code}）`,
        log_tail: tailText(output, 4096),
        hint: '完整日志见项目 logs/build.log；可用 logs 命令读取',
      };
    }
  }
  return { ok: true, release_dir: join(projectDir, 'release') };
}

function runYarn(cwd, args) {
  return new Promise((resolvePromise) => {
    let settled = false;
    const settle = (value) => {
      if (!settled) {
        settled = true;
        resolvePromise(value);
      }
    };
    let child;
    try {
      child = spawn('yarn', args, spawnOptions({ cwd, env: { ...process.env, npm_config_cache: join(cwd, 'runtime', 'npm-cache') } }));
    } catch (error) {
      settle({ code: -1, output: '', spawnError: error.message });
      return;
    }
    let stdout = '';
    let stderr = '';
    child.stdout?.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr?.on('data', (chunk) => {
      stderr += chunk;
    });
    // spawn 失败（如 ENOENT）经 error 事件到达：结构化返回，不让进程崩溃。
    child.on('error', (error) => {
      settle({ code: -1, output: `${stdout}\n${stderr}`, spawnError: error.message });
    });
    const timer = setTimeout(() => {
      killTree(child);
      // 终止进程树后 close 事件仍会到达并完成 Promise（等待真正退出）。
    }, BUILD_TIMEOUT_MS);
    child.on('close', (code) => {
      clearTimeout(timer);
      appendLog(cwd, `$ yarn ${args.join(' ')}\n${stdout}\n${stderr}\n# 退出码 ${code ?? -1}\n`);
      settle({ code: code ?? -1, output: `${stdout}\n${stderr}`, spawnError: null });
    });
  });
}

function appendLog(projectDir, text) {
  mkdirSync(join(projectDir, 'logs'), { recursive: true });
  appendFileSync(join(projectDir, 'logs', 'build.log'), text);
}

function zeroBuild(projectDir) {
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(join(projectDir, 'plugin.json'), 'utf8'));
  } catch (error) {
    return { ok: false, error: `plugin.json 解析失败：${error.message}` };
  }
  const releaseDir = join(projectDir, 'release');
  rmSync(releaseDir, { recursive: true, force: true });
  mkdirSync(releaseDir, { recursive: true });
  copyChecked(join(projectDir, 'plugin.json'), join(releaseDir, 'plugin.json'), 'plugin.json');
  for (const contribution of manifest.ui?.contributions ?? []) {
    const entry = contribution.entry ?? '';
    if (!entry) continue;
    const source = resolveInside(projectDir, entry, `UI 入口 ${entry}`);
    const target = resolveInside(releaseDir, relative(projectDir, source), `发布目标 ${entry}`);
    if (statSync(source).isDirectory()) {
      copyTree(source, target);
    } else {
      copyChecked(source, target, `UI 入口 ${entry}`);
    }
  }
  for (const directory of manifest.resources ?? []) {
    const source = resolveInside(projectDir, directory, `资源目录 ${directory}`);
    if (!isRealDirectory(source)) continue;
    const target = resolveInside(releaseDir, relative(projectDir, source), `发布目标 ${directory}`);
    copyTree(source, target);
  }
  const files = walk(releaseDir)
    .filter((path) => !path.endsWith('content-manifest.json'))
    .sort()
    .map((path) => ({
      path: relative(releaseDir, path).split('\\').join('/'),
      sha256: createHash('sha256').update(readFileSync(path)).digest('hex'),
    }));
  writeFileSync(
    join(releaseDir, 'content-manifest.json'),
    `${JSON.stringify({ algorithm: 'sha256', files }, null, 2)}\n`,
  );
  return { ok: true, release_dir: releaseDir, files: files.length };
}

import { dirname } from 'node:path';
/** 复制前校验源为普通文件（拒绝符号链接与特殊类型），并确保父目录存在。 */
function copyChecked(from, to, label) {
  const stat = statSync(from);
  if (stat.isSymbolicLink() || !stat.isFile()) {
    throw Object.assign(new Error(`${label} 必须是普通文件（拒绝符号链接）：${from}`), { code: 'SYMLINK' });
  }
  mkdirSync(dirname(to), { recursive: true });
  copyFileSync(from, to);
}

/** 逐项校验的递归复制：目录/普通文件之外的实体（符号链接等）拒绝。 */
function copyTree(source, target) {
  mkdirSync(target, { recursive: true });
  for (const name of readdirSync(source)) {
    const from = join(source, name);
    const to = join(target, name);
    const stat = statSync(from);
    if (stat.isSymbolicLink()) {
      throw Object.assign(new Error(`构建产物不能包含符号链接：${from}`), { code: 'SYMLINK' });
    }
    if (stat.isDirectory()) {
      copyTree(from, to);
    } else if (stat.isFile()) {
      copyFileSync(from, to);
    } else {
      throw Object.assign(new Error(`不支持的文件类型：${from}`), { code: 'BAD_FILE' });
    }
  }
}

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...walk(path));
    else out.push(path);
  }
  return out;
}

function tailText(text, maxBytes) {
  const buffer = Buffer.from(text, 'utf8');
  if (buffer.length <= maxBytes) return text;
  return buffer.subarray(buffer.length - maxBytes).toString('utf8');
}
