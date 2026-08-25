// build：工程模板走 yarn install → build → package；零构建模板走内建打包
//（plugin.json + UI 入口目录 + resources 目录 + 内容树清单）。
import { appendFileSync, cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execSync, spawn } from 'node:child_process';
import { join, relative } from 'node:path';
import { fail, requireProject } from './common.mjs';

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
    result = zeroBuild(projectDir);
  }
  return { ...result, duration_ms: Date.now() - started };
}

async function yarnBuild(projectDir) {
  const steps = [
    ['install', ['install', '--silent']],
    ['build', ['run', 'build']],
    ['package', ['run', 'package']],
  ];
  for (const [step, args] of steps) {
    const { code, stdout, stderr } = await runYarn(projectDir, args);
    if (code !== 0) {
      return {
        ok: false,
        failed_step: step,
        error: `步骤 [${step}] 失败（退出码 ${code}）`,
        log_tail: tail(`${stdout}\n${stderr}`, 4096),
        hint: '完整日志见项目 logs/build.log；可用 logs 命令读取',
      };
    }
  }
  return { ok: true, release_dir: join(projectDir, 'release') };
}

function runYarn(cwd, args) {
  return new Promise((resolve) => {
    const child = spawn('yarn', args, {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, npm_config_cache: join(cwd, 'runtime', 'npm-cache') },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    const timer = setTimeout(() => child.kill('SIGKILL'), BUILD_TIMEOUT_MS);
    child.on('close', (code) => {
      clearTimeout(timer);
      appendLog(cwd, `$ yarn ${args.join(' ')}\n${stdout}\n${stderr}\n# 退出码 ${code}\n`);
      resolve({ code: code ?? -1, stdout, stderr });
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
  cpSync(join(projectDir, 'plugin.json'), join(releaseDir, 'plugin.json'));
  const copied = new Set(['plugin.json']);
  for (const contribution of manifest.ui?.contributions ?? []) {
    const entry = contribution.entry ?? '';
    if (!entry) continue;
    const topSegment = entry.split('/')[0];
    const source = join(projectDir, topSegment);
    if (entry.includes('/')) {
      cpSync(source, join(releaseDir, topSegment), { recursive: true });
    } else if (existsSync(source)) {
      cpSync(source, join(releaseDir, entry));
    }
    copied.add(topSegment);
  }
  for (const directory of manifest.resources ?? []) {
    const source = join(projectDir, directory);
    if (existsSync(source)) {
      cpSync(source, join(releaseDir, directory), { recursive: true });
    }
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

function walk(dir) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) out.push(...walk(path));
    else out.push(path);
  }
  return out;
}

function tail(text, maxBytes) {
  const buffer = Buffer.from(text, 'utf8');
  if (buffer.length <= maxBytes) return text;
  return buffer.subarray(buffer.length - maxBytes).toString('utf8');
}
