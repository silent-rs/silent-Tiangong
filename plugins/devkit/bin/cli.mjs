#!/usr/bin/env node
// @silent-ai/plugin-creator（devkit CLI）——天工插件创作工具链。
//
// 经命令通道执行：npx -y @silent-ai/plugin-creator@1.0.0 <command> ...
// 约定：stdout 输出单个 JSON 结果（{ok, ...}）；人读信息写 stderr；
// 非零退出码表示失败。开发目录固定 ~/.tiangong/plugins-dev/<id>/
// （TIANGONG_PLUGINS_DEV 或 --root 覆盖）。
import { init } from '../src/init.mjs';
import { validate } from '../src/validate.mjs';
import { build } from '../src/build.mjs';
import { run } from '../src/run.mjs';
import { logs } from '../src/logs.mjs';

const VERSION = '1.0.0';

function usage() {
  return [
    `@silent-ai/plugin-creator devkit v${VERSION}`,
    '',
    '用法（stdout 输出 JSON 结果）：',
    '  plugin-creator init <template> <id> [--name 显示名]    按模板生成项目骨架',
    '  plugin-creator validate <id>                            清单与结构校验',
    '  plugin-creator build <id>                               构建（yarn 或零构建打包）',
    '  plugin-creator run <id> [-- 脚本参数...]                按 run.json 试运行',
    '  plugin-creator logs <dev:id|plugin:id> [--lines N]      读日志尾部',
    '',
    '全局：--root <path> 覆盖开发根（默认 ~/.tiangong/plugins-dev）。',
    '模板：ui-app（纯 UI，零构建）、ts-tool（TS 工具插件）、ts-npx（npx 脚本插件）。',
  ].join('\n');
}

function parseGlobalArgs(argv) {
  const rootIndex = argv.indexOf('--root');
  if (rootIndex !== -1 && argv[rootIndex + 1]) {
    const root = argv[rootIndex + 1];
    argv.splice(rootIndex, 2);
    return root;
  }
  return undefined;
}

async function main() {
  const argv = process.argv.slice(2);
  const rootOverride = parseGlobalArgs(argv);
  const [command, ...rest] = argv;
  const ctx = { rootOverride };
  const table = { init, validate, build, run, logs };
  const handler = table[command];
  if (!handler) {
    process.stderr.write(usage());
    process.stderr.write(`\n未知命令：${command ?? '(空)'}\n`);
    process.exit(2);
  }
  try {
    const result = await handler(rest, ctx);
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    if (!result.ok) process.exit(1);
  } catch (error) {
    process.stderr.write(`devkit 内部错误：${error?.stack ?? error}\n`);
    process.exit(1);
  }
}

void main();
