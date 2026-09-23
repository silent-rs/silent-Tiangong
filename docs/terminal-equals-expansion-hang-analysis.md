# 终端命令「rg/grep 执行极慢」现象分析报告

- 报告日期：2026-09-23
- 现象：Agent 通过终端插件执行 `rg`/`grep` 等命令时耗时极长（常达 20~60s 甚至超时），
  用户在同一嵌入终端里手动执行同样的命令却是秒级完成。
- 结论：**与 `rg`/`grep` 本身无关，也与 `target/` 等目录是否被忽略无关。**
  真实原因是终端插件 POSIX 命令包装器对 zsh 的 `EQUALS` 选项不兼容，
  导致脚本在特定内容处中途夭折，结束标记永不出现，调用方一直轮询到超时。

## 1. 先排除「rg 扫了 target 目录」的假设

在目标仓库实测（ripgrep 15.2.0，worktree `computer-use-exception`）：

```
$ rg --files crates plugins src-tauri | wc -l      # 924
$ time rg -n "INJECTED_ASSETS_FIELD" crates plugins src-tauri --glob '*.rs'
rg ... 0.01s user 0.13s system 674% cpu 0.020 total
```

带进程内时间戳的复核同样是 0.05 秒级：

```
T_START 1790079859.614785
T_END   1790079859.662505      # 差值 ≈ 0.048s
```

`rg` 默认遵守 `.gitignore`，`target/`、`node_modules/` 本就被排除，
924 个文件的扫描不存在性能问题。所以慢的不是命令，而是**命令结束的判定**。

## 2. 关键观察：慢的调用有共同特征

回看本次会话中超时/挂起的调用，全部包含一个裸的 `===` 分隔符，例如：

```sh
rg -n "fn raw_output_since" -A 25 service.rs; echo ===; grep -n "..." service.rs | head -40
grep -n "fn raw_output_since" -A 3 service.rs; echo ===; echo done
echo A; echo ===; echo B
```

而不含 `===` 的等价命令全部秒回：

```sh
grep -n "MARKER_PREFIX: " service.rs                 # 立即返回
grep -c TIANGONG service.rs                          # 立即返回
echo X1; echo '==='; echo X2                         # 立即返回（加了引号）
```

最小复现：`echo A; echo ===; echo B` 超时；`echo X1; echo '==='; echo X2` 正常。
变量只有一个——`===` 是否被引号包裹。

## 3. 根因：zsh `EQUALS` 展开击穿了命令包装器

### 3.1 zsh 行为

zsh 默认开启 `EQUALS` 选项：以 `=` 开头的单词被当作 **equals-expansion**，
`=foo` 会被替换为 `foo` 命令的完整路径（类似 `which`）。因此 `echo ===`
中的 `===` 被解析为「`=` + 命令名 `==`」，而 `==` 不是命令：

```
$ printf 'echo START\necho ===\necho AFTER\n' > /tmp/t.sh; zsh /tmp/t.sh
START
/tmp/t.sh:2: == not found          ← 第 2 行直接失败
RC=1                               ← 后续 echo AFTER 根本没执行
```

`setopt noequals` 后立刻恢复正常，确证选项归属：

```
$ printf 'echo L1\nsetopt noequals\necho ===\necho L3\n' > /tmp/t4.sh
$ zsh -c 'source /tmp/t4.sh; echo RC2=$?'
L1
===
L3
RC2=0
```

注意 equals-expansion 失败不是普通的「命令返回非零」，而是**解析期错误**，
在 `source`/`.` 执行脚本时会直接终止整个脚本的后续执行（实测 `SOURCE_RC=126`）。

### 3.2 包装器为什么因此挂死

终端插件把 Agent 的命令包成一个临时 `.sh`，用 marker 划定边界
（`plugins/tiangong-plugin-terminal/sidecar/src/service.rs:2186` `prepare_posix_command`）：

```
echo '__TIANGONG_START_<id>__'
<用户命令>                      ← 用户命令在这里整体内联
__tiangong_rc=$?
printf '\n__TIANGONG_CWD_<id>__'; pwd
echo '__TIANGONG_RC_<id>__'$__tiangong_rc
echo '__TIANGONG_END_<id>__'    ← 完成判定依赖这一行
```

再以 `. <临时文件>` 的方式 source 进当前登录 shell
（`service.rs:2203`）。于是：

1. 用户命令里的 `echo ===` 触发 zsh equals-expansion 解析错误；
2. `source` 的脚本在该行**整体中止**，后面的 `__TIANGONG_RC_`、
   `__TIANGONG_END_` 几行永远不会输出；
3. 调用侧 `exec_non_interactive`（`service.rs:1423`）以 `parsed.completed`
   为完成判据，每 50ms 轮询一次（`COMMAND_POLL_INTERVAL_MS`），
   end marker 不出现就一直转；
4. 没有 `timeout` 时只能靠「静默 5s + 前台 tty 进入非规范模式」这条
   交互程序兜底（`SILENT_INTERACTIVE_HANG_SECS`）；但此处前台已回到 zsh
   提示符、tty 仍是规范模式，兜底条件不成立 → 一直轮询到工具层超时；
5. 有 `timeout` 时就表现为「命令执行超时」，并走 `collect_after_interrupt`，
   还可能把终端标记为 `Unresponsive`（`service.rs:1612`），
   后续调用被迫新建终端，进一步放大「变慢」的体感。

这解释了全部现象：**慢的不是命令，是卡在等一个永远不会来的结束标记。**

### 3.3 为什么手动执行很快

用户在终端里手动敲 `rg ...`，走的是交互式 zsh 正常行数，
既没有被包进 `source` 的临时脚本，也不需要 marker 判定完成，
命令 0.02s 结束、提示符立即回来。两条路径的差别不在命令，在包装。

## 4. 影响面

触发条件是「用户命令中出现以 `=` 开头的裸单词」，`===` 只是最常见的一种。
同类会中招的还有：

- `echo ====` / `echo =====` 之类的分隔符（Agent 输出分段时极常用）；
- `awk`、`sed` 等参数里出现未引用的 `=cmd` 形态；
- 任何 `=` 开头且后续内容不是合法命令名的裸词。

后果分三层：
1. 该次工具调用挂起或超时，Agent 拿不到结果；
2. 终端被标记 `Unresponsive` 退出复用池，后续调用新建终端；
3. Agent 误判为「rg 很慢/仓库太大」，转而做无意义的范围收窄，浪费轮次。

本次会话中该问题至少命中 5 次，直接拖慢了正在进行的 code review 任务。

## 5. 修复建议（按优先级）

### P0：包装脚本关闭 EQUALS
在 `prepare_posix_command` 生成的脚本开头、用户命令之前加入：

```sh
if [ -n "$ZSH_VERSION" ]; then setopt local_options no_equals 2>/dev/null || setopt noequals; fi
```

或在 source 入口处使用 `emulate -L sh`（更彻底，但会改变用户命令的其他 zsh 语义，
需评估对 Agent 既有命令习惯的影响）。建议先采用最小侵入的 `no_equals`。

注意：这类解析期错误只能靠「不让它发生」来根治，
用 `set -e`/trap 之类的手段无法在解析失败后补出 end marker。

### P1：为「脚本夭折」提供兜底完成判据
即使修掉 EQUALS，其他解析期错误（未闭合引号、非法重定向等）同样会吞掉
end marker。建议在 `exec_non_interactive` 增加一条判据：
**start marker 已出现、end marker 未出现，但前台已回到 shell 且 tty 处于规范模式、
输出静默超过阈值** → 按「命令异常结束」收尾，返回已收集的输出并给出明确 stderr，
而不是一直轮询到超时。现有 `foreground_is_interactive` 已能提供前台/tty 判据
（`service.rs:1513`），只需增加「非交互 + 静默」的对称分支。

### P2：把包装失败与用户命令失败区分开
当前解析失败后 `parsed.exit_code` 为 `None`、`completed` 为 false，
调用方只能报「超时」，信息对排障无用。建议返回可识别的错误语义，
例如「命令包装脚本未正常结束（可能存在语法/解析错误）」，附原始输出尾部。

### P3（可选）：Agent 侧规避
在终端工具说明中提示「分隔符用引号包裹」。这只是缓解，不能替代 P0/P1。

## 6. 复现步骤（供验证修复）

```sh
# 触发（修复前挂起/超时）
run_shell: echo A; echo ===; echo B

# 对照（始终正常）
run_shell: echo X1; echo '==='; echo X2

# 机制确证
printf 'echo L1\necho ===\necho L3\n' > /tmp/t.sh && zsh /tmp/t.sh          # 第2行报 == not found
printf 'echo L1\nsetopt noequals\necho ===\necho L3\n' > /tmp/t4.sh && zsh /tmp/t4.sh   # 全部正常
```

修复后判定标准：第一条命令应在 1 秒内返回，stdout 含 `A`、`===`、`B` 三行，
exit_code 为 0，终端保持 `idle` 可复用。

## 7. 相关代码位置

- `plugins/tiangong-plugin-terminal/sidecar/src/service.rs:2186` `prepare_posix_command` — 包装脚本生成
- `plugins/tiangong-plugin-terminal/sidecar/src/service.rs:2203` — `. <tmpfile>` 的 source 入口
- `plugins/tiangong-plugin-terminal/sidecar/src/service.rs:1423` `exec_non_interactive` — 完成判定与轮询
- `plugins/tiangong-plugin-terminal/sidecar/src/service.rs:2273` `parse_command_output` — marker 解析
- `plugins/tiangong-plugin-terminal/sidecar/src/service.rs:1612` `mark_unresponsive_with_probe` — 超时后退出复用池
- 常量：`COMMAND_POLL_INTERVAL_MS=50`、`SILENT_INTERACTIVE_HANG_SECS=5`（`service.rs:42`、`service.rs:53`）

---

## 8. 实际采用的修复（与上文建议的差异）

修复分支 `fix/terminal-equals-expansion-hang`，改动集中在
`plugins/tiangong-plugin-terminal/sidecar/src/service.rs`。

### 8.1 P0 升级为「用户命令隔离到内层脚本」

上文 P0 只建议关掉 `EQUALS`。实际实现在此之上做了更根本的一步：
**用户命令不再内联进 wrapper，而是单独写入内层脚本，由 wrapper `source` 执行。**

```
wrapper.sh                      inner.sh
  echo START_MARKER               <用户命令原文>
  setopt noequals（仅 zsh）
  . inner.sh        ────────────►
  rc=$?
  setopt equals（按原值恢复）
  printf CWD_MARKER; pwd
  echo RC_MARKER$rc
  echo END_MARKER
```

理由：`EQUALS` 只是解析期错误的一种来源，未闭合引号、非法重定向等同样会
在解析阶段终止整份脚本，而 `set -e` / `trap` / `||` 一律无效——余下语句
根本不会被执行。拆成两层后，解析失败只终止内层 `source`，shell 把它折算
成一个普通的非零退出码（实测 zsh 126、bash 1），wrapper 的控制流完好，
照常输出 cwd、退出码和 end marker。

实测对照（同一 wrapper，内层放未闭合引号）：

```
==== zsh ====                    ==== bash ====
MK_START_x                       MK_START_x
A                                A
.../cmd_inner.sh:4: unmatched "  ...: syntax error: unexpected end of file
MK_CWD_x/tmp/tgproto             MK_CWD_x/tmp/tgproto
MK_RC_x126                       MK_RC_x1
MK_END_x                         MK_END_x
```

`EQUALS` 仍然照建议关闭，但目的从「避免夭折」变成「让 `echo ===`、
`awk =x` 这类写法按字面量正常执行」，而不是稳定地报错后收尾。
片段用 `ZSH_VERSION` 守卫并经 `eval` 执行，对 bash/dash/sh 无害；
原值为关闭时不恢复，不改变用户自己的 shell 偏好。

### 8.2 P1 判据改为「就绪探针」，而非 tty 规范模式 + 静默

上文建议复用 `foreground_is_interactive` 取反（前台回到 shell + tty 规范模式
+ 静默）。实现时换成了向 shell 写一条**就绪探针**并看它是否被执行：

- 前台还有命令在跑时，探针只会躺在输入缓冲里不被执行 → 判定未夭折；
- 脚本已夭折时 shell 已回到提示符，探针立即执行并回显 marker → 判定夭折。

换判据的原因：`foreground_is_interactive` 的取反不是充分条件。命令在两条
前台进程之间的空档、或前台恰好是不改 tty 模式的短命令时，「前台是 shell
且 tty 规范」会瞬时成立，静默窗口一到就会把仍在推进的命令误判为夭折。
探针直接检验「shell 是否正在读新命令」，这正是要判定的事实本身。

探针复用 `shell_ready_probe` 的拼接写法，保证 PTY 回显不含完整 marker，
纯回显不会造成误判；marker 带 `__TIANGONG_` 公共前缀，既被 UI 过滤器隐藏，
也被 `parse_command_output` 排除在命令 stdout 之外。
探测按 3 秒静默触发、间隔 5 秒、最多 3 次，避免持续写入污染真正在读
stdin 的前台程序。

### 8.3 P2 已实现

夭折收尾返回 `exit_code = -1`、`timed_out = false`，stderr 为
「命令包装脚本未正常结束（命令可能存在语法/解析错误），已返回其结束前的输出」，
与「命令执行超时」区分开——后者会把终端标记为不可复用，夭折不会。

## 9. 验证

`cargo test -p tiangong-plugin-terminal-sidecar`：42 passed；
`cargo clippy --all-targets -- -D warnings`：零警告。

新增用例：

| 用例 | 覆盖 |
|---|---|
| `zsh_裸等号命令不再挂起并返回完整输出` | issue 验证标准：真实 zsh PTY 下 `echo A; echo ===; echo B` 返回 A/===/B、rc=0、终端可复用 |
| `用户命令语法错误仍按非零退出码正常收尾` | 未闭合引号不再表现为超时，保留报错前输出 |
| `包装脚本夭折时按异常结束收尾而非超时` | 内层 `exec sh -i` 顶掉 shell 使 marker 全丢，兜底 3.96s 收尾并给出可识别 stderr |
| `静默长命令不被夭折兜底误判` | `sleep 6` 跨越多轮探测仍正常完成 |
| `posix_包装器把用户命令隔离在内层脚本` | 结构约束：两个脚本文件、命令不内联、wrapper 自带 end marker |
| `posix_包装器关闭并恢复_zsh_equals` | 关闭在 source 之前、恢复在之后、ZSH_VERSION 守卫 |
| `夭折探针不污染命令输出` | 探针行不进入 stdout |

反向确认：把 `prepare_posix_command` 退回旧的内联写法并清空 equals 片段后，
`zsh_裸等号命令不再挂起并返回完整输出` 与
`用户命令语法错误仍按非零退出码正常收尾` 均以
`stderr: "命令执行超时", exit_code: -1, timed_out: true` 失败，
与 issue 描述的现象一致；恢复修复后通过。

## 10. 回归修复：就绪探针在 zsh 行编辑器下的假阴性（0.3.11）

### 10.1 现象

0.3.10 上线后出现新的异常：`git fetch`、`yarn install`、`gh pr create`、
`cargo test` 等命令**实际执行成功**，却被回报为

```
命令包装脚本未正常结束（命令可能存在语法/解析错误），已返回其结束前的输出
exit_code = -1
```

这是**假阴性**，比 #571 的挂起更危险：挂起是可见的卡死，而假阴性会让调用方
误以为操作失败并重试——对 `git push`、创建 PR、发送消息、消耗额度这类动作
可能造成重复执行。实测一次会话内复现 5 次以上。

### 10.2 根因：在绘制中间态上做子串匹配

第 8 章引入的 P1 兜底用 `shell_ready_probe` 写入一行探针，再由
`shell_ready_probe_completed` 判断 shell 是否已回到提示符。旧实现是：

```rust
fn shell_ready_probe_completed(raw: &str, marker: &str) -> bool {
    raw.contains(marker)          // 裸字节子串匹配
}
```

其安全性依赖 `shell_ready_probe` 的相邻引用串写法——写入的字节里 marker 被
`''` 断开，纯 tty 回显不含完整 marker，必须 shell 真正执行 `echo` 拼接后才
出现。测试 `shell_ready_probe_文本不含完整_marker` 守的就是这条。

**但 zsh 行编辑器插件会打破这个前提。** `zsh-autosuggestions`、
`zsh-syntax-highlighting` 是 ZLE widget，在**行编辑阶段**就对输入行做解析与
重绘；重绘输出的是语义处理后的内容，引号被消化，完整 marker 随之出现在 PTY
原始字节流里——而命令**一个字都还没执行**。就绪判定因此恒真，正在正常运行的
命令被判为夭折。

关键在于：原始字节流里混着 ZLE 重绘序列、光标回退、`\r` 覆写与预测文本擦除，
这些字节**从未作为「终端最终显示的内容」存在过**，只是绘制过程的中间态。在
中间态上 `contains`，等于把「屏幕上曾经闪过的像素」当成命令执行结果。

同一文件里 `parse_command_output`（主判据）早已走的是正确路子：先用
`persist::TerminalLineProcessor`（`vte::Parser` + 光标模拟）把原始字节还原成
终端真实呈现的行，再做**整行相等**匹配。两套判据一严一松，缺陷由此而来。

### 10.3 一条错误的测试固化了缺陷

0.3.10 的 `就绪判定_命中富提示符与_xtrace_噪声中的_marker` 用例里：

```
_zsh_autosuggest_bind_widgets:18> echo __TIANGONG_READY_abc__
```

这是 autosuggest 的 xtrace 回显，**命令尚未执行**，而断言写的是「必须判为
就绪」。当时为了容忍富提示符噪声把判定放得过宽，等于给错误判据发了通行证。

### 10.4 修复

让探针判据与主判据走同一条路——先终端仿真还原，再整行相等：

```rust
fn shell_ready_probe_completed(raw: &str, marker: &str) -> bool {
    let mut processor = persist::TerminalLineProcessor::new();
    let mut lines = processor.process(raw);
    let current = processor.current_line();
    if !current.trim().is_empty() {
        lines.push(current);
    }
    lines.iter().any(|line| line.trim() == marker)
}
```

- 重绘中间态被 vte 正确消化，只留终端最终呈现的行
- 整行相等取代子串包含，回显的 `echo '前缀''后缀'` 不可能等于纯 marker 行
- `shell_ready_probe` 的拼接防护重新生效，且不再依赖「用户没装 shell 插件」
  这一脆弱前提

判据只变严不变松，end marker 主判据完全未动，不会引回 #571 的挂起；也不涉及
前台进程组判断（`foreground_is_interactive` 存在前台空档瞬时误判问题，见
8.3），不引入新的误判面。

### 10.5 验证

`cargo test -p tiangong-plugin-terminal-sidecar -- --test-threads=1`：45 passed。

| 用例 | 覆盖 |
|---|---|
| `就绪判定_zsh行编辑器重绘不算就绪` | xtrace 回显与语法高亮重绘均不得判为就绪（本次回归的直接守护） |
| `就绪判定_命中富提示符噪声中独占一行的_marker` | 真正执行后 marker 独占一行仍必须判为就绪，兜底能力不被削弱 |
| `就绪判定_未执行的纯回显不算就绪` | 原有防护保持有效 |

**反向确认**：把 `shell_ready_probe_completed` 退回 `raw.contains(marker)`
后，`就绪判定_zsh行编辑器重绘不算就绪` 立即 FAILED；恢复修复后通过。证明该
用例确实守得住这个缺陷。

### 10.6 经验

本次定位走了弯路：最初三次尝试都在单元测试里构造「读 stdin 的长命令」，
全部无法复现——因为**测试环境的 shell 不加载用户 `.zshrc`，没有 autosuggest**，
而故障只在装了 ZLE 插件的真实环境出现。复现环境与故障环境不一致时，构造再多
用例也撞不上。应当先确认故障环境的差异特征，再决定复现路径。
