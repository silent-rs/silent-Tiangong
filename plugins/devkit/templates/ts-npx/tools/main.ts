// ts-npx 模板：npx 命令行脚本（经命令执行通道运行，无 sidecar、无构建）。
//
// 形态：CLI——命令行参数输入、stdout 输出 JSON 结果、非零退出码表示失败。
// Agent 经命令通道运行：npx -y tsx@4.19.2 <本文件> [--name 名字]
// （完整命令见 plugin.json 的能力说明段）。
//
// 改造指南：按需求解析参数并实现功能，保持「stdout 只输出一个 JSON 结果」
// 的约定（日志信息写 stderr），下游（Agent/页面）按 JSON 消费。

interface HelloResult {
  ok: boolean;
  message: string;
  args: string[];
  node: string;
}

function main(): void {
  const args = process.argv.slice(2);
  let name = "天工";
  const nameIndex = args.indexOf("--name");
  if (nameIndex !== -1 && args[nameIndex + 1]) {
    name = args[nameIndex + 1];
  }
  const result: HelloResult = {
    ok: true,
    message: `你好，${name}！这是 {{PLUGIN_ID}} 的 npx 脚本能力。`,
    args,
    node: process.version,
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

main();
