#!/bin/sh
# sh scripts/verify-issue-513.sh
# 实进程验证，不运行单元测试。需要 macOS、Python 3.11+、Rust 和可用的原生沙箱。
# 所有程序从当前源码构建；临时宿主仅调用项目现有接口，不替代退出实现。
# 日志保留在 target/debug/issue-513-*；环境受限返回 77，不计为通过。
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec python3 - "$repo_root" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import re
import select
import signal
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib

root = Path(sys.argv[1])
os.chdir(root)
debug = root / 'target/debug'
debug.mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix='issue-513-', dir=debug))
print(f'验证日志：{work}', flush=True)
children = []
groups = set()
passed = []
failures = []


def deadline(_signum, _frame):
    raise TimeoutError('整套验证超过 15 分钟，停止并清理')


signal.signal(signal.SIGALRM, deadline)
signal.alarm(900)


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_until(predicate, seconds, description):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError(description)


def bounded(command, seconds, **kwargs):
    process = subprocess.Popen(command, start_new_session=True, **kwargs)
    children.append(process)
    try:
        code = process.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
        raise AssertionError(f'命令超时：{command[0]}')
    if code:
        raise AssertionError(f'命令失败（{code}）：{command}')


def success(name):
    passed.append(name)
    print(f'PASS {name}', flush=True)


def environment():
    env = os.environ.copy()
    for key in list(env):
        if key.startswith('TIANGONG_') or key == 'RUST_LOG':
            del env[key]
    return env


def check_environment():
    probe = subprocess.Popen(['/bin/sleep', '2'], start_new_session=True)
    children.append(probe)
    try:
        probe.kill()
        probe.wait(timeout=3)
        bounded(['/usr/bin/sandbox-exec', '-p', '(version 1)(allow default)',
                 '/usr/bin/true'], 5, stdout=subprocess.DEVNULL)
    except (PermissionError, FileNotFoundError, AssertionError) as error:
        print(f'UNSUPPORTED：无法实际验证原生 macOS 沙箱或终止子进程：{error}', flush=True)
        raise SystemExit(77)
    success('环境探测：实际启用沙箱、实际终止子进程')


HOST = r'''
use std::{io::{self, Write}, path::{Path, PathBuf}, sync::{Arc, Barrier}, time::{Duration, Instant}};
use anyhow::{Result, bail};
use serde_json::json;
use sha2::{Digest, Sha256};
use tiangong_plugin_runtime::{registry, manifest::SidecarLifecycle, sidecar::{SidecarConfig, SidecarConnection, StdioSidecarConnection}, trust};

fn announce(message: &str) {
    println!("{message}");
    io::stdout().flush().unwrap();
}
fn input() -> String {
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    line.trim().to_owned()
}
fn fixture(root: &Path, binary: &Path) -> Result<PathBuf> {
    tiangong_plugin_runtime::test_support::ensure_test_launcher_signed(root)
        .map_err(anyhow::Error::msg)?;
    tiangong_config::registry::init_from_dir(&root.join("config"));
    let dir = root.join("plugins/test-stdio");
    std::fs::create_dir_all(&dir)?;
    std::fs::hard_link(binary, dir.join("sidecar"))?;
    let manifest = json!({"schema_version":2,"id":"test-stdio","version":"0.0.0",
        "entrypoints":["desktop"],
        "permissions":["sidecar.invoke","tool.provide"],"capabilities":{"tools":true},
        "tools":[{"name":"echo","description":"echo","input_schema":{"type":"object"}}],
        "sidecar":{"binary":"sidecar","lifecycle":"resident","startup_timeout_ms":2000}});
    std::fs::write(dir.join("plugin.json"), serde_json::to_vec(&manifest)?)?;
    let artifact = |name: &str| -> Result<serde_json::Value> {
        let digest = Sha256::digest(std::fs::read(dir.join(name))?);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(json!({"path":name,"sha256":hex}))
    };
    let release = json!({"schema_version":1,"id":"test-stdio","version":"0.0.0",
        "publisher":"local","permissions":["sidecar.invoke","tool.provide"],
        "manifest":artifact("plugin.json")?,"sidecar":artifact("sidecar")?});
    std::fs::write(dir.join("release.json"), serde_json::to_vec(&release)?)?;
    trust::sign_with_user_key(root, &dir.join("release.json"))?;
    Ok(dir.join("sidecar"))
}
fn connection(root: &Path, binary: &Path, sandbox: bool) -> StdioSidecarConnection {
    let startup = if std::env::var_os("TIANGONG_TEST_STDIO_MUTE").is_some() {
        Duration::from_millis(500)
    } else {
        Duration::from_secs(10)
    };
    StdioSidecarConnection::new(SidecarConfig::new("test-stdio", "0.0.0", binary,
        root.join("endpoint.json"), root.join("sidecar.log"), root.join("data"), root)
        .with_lifecycle(SidecarLifecycle::Resident)
        .with_sandbox(sandbox).with_timeouts(startup, Duration::from_secs(10)))
}
fn main() -> Result<()> {
    tracing_subscriber::fmt().with_ansi(false).with_target(false).with_writer(io::stderr).init();
    let args: Vec<String> = std::env::args().collect();
    let mode = &args[1];
    let root = PathBuf::from(&args[2]);
    let binary = fixture(&root, Path::new(&args[3]))?;
    let sandbox = args[4] == "1";
    if mode == "race" {
        assert_eq!(registry::preload_installed_plugins(&root), 1);
        registry::reverify_plugin_sidecar(&root, "test-stdio")?;
        let result = registry::invoke_sidecar(&root, "test-stdio", "echo", json!({"ready":true}))?;
        assert_eq!(result["ready"], true);
        // 使后台补验证真正需要执行，而不是因已有记录直接返回。
        std::fs::remove_file(root.join("plugins/.verifications/test-stdio.json"))?;
        let barrier = Arc::new(Barrier::new(13));
        let threads: Vec<_> = (0..12).map(|index| {
            let barrier = Arc::clone(&barrier);
            let root = root.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match index % 3 {
                    0 => { registry::preload_installed_plugins(&root); }
                    1 => registry::prewarm_plugin_sidecar(&root, "test-stdio"),
                    _ => tiangong_plugin_runtime::verification::reverify_installed_sidecars(&root),
                }
            })
        }).collect();
        barrier.wait();
        std::thread::sleep(Duration::from_millis(args[5].parse()?));
        registry::begin_sidecar_shutdown();
        registry::shutdown_all_sidecars();
        for thread in threads { thread.join().unwrap(); }
        assert!(registry::sidecars_shutting_down());
        assert_eq!(registry::preload_installed_plugins(&root), 0);
        registry::prewarm_plugin_sidecar(&root, "test-stdio");
        tiangong_plugin_runtime::verification::reverify_installed_sidecars(&root);
        let error = connection(&root, &binary, true).ensure_running().unwrap_err();
        assert!(format!("{error:#}").contains("应用正在退出"), "{error:#}");
        announce("CLOSED");
        input(); // 外部脚本在宿主存活时核对所有子进程确实退出。
        return Ok(());
    }
    let connection = Arc::new(connection(&root, &binary, sandbox));
    if mode == "mute" {
        let started = Instant::now();
        let error = connection.ensure_running().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(format!("{error:#}").contains("就绪握手失败"), "{error:#}");
        connection.stop()?;
        announce("CLOSED");
        input();
        return Ok(());
    }
    let reply = connection.invoke("echo", r#"{"ready":true}"#)?;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&reply)?["ready"], true);
    if mode == "restart" {
        assert!(connection.invoke("crash", "{}").is_err());
        let reply = connection.invoke("echo", r#"{"restarted":true}"#)?;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&reply)?["restarted"], true);
        connection.stop()?;
        announce("CLOSED"); input(); return Ok(());
    }
    if mode == "busy" {
        let connection = Arc::clone(&connection);
        let payload = json!({"sidecar_pid_file":root.join("sidecar.pid"),"child_pid_file":root.join("child.pid")});
        std::thread::spawn(move || { let _ = connection.invoke("hang", &payload.to_string()); });
        let end = Instant::now() + Duration::from_secs(5);
        while !root.join("child.pid").is_file() {
            if Instant::now() >= end { bail!("后台子进程未启动"); }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    announce("READY");
    match input().as_str() {
        "stop" => { connection.stop()?; announce("CLOSED"); input(); }
        // 正常退出也刻意不调用 stop，用于独立验证管道 EOF/宿主监视兜底。
        "exit" => std::process::exit(0),
        other => bail!("未知控制命令：{other}"),
    }
    Ok(())
}
'''


def build():
    log = work / 'build.jsonl'
    with log.open('w') as output:
        bounded(['cargo', 'build', '-p', 'tiangong-plugin-sidecar', '-p',
                 'tiangong-plugin-fs-sidecar', '-p', 'tiangong-sandbox',
                 '--bins', '--message-format=json'], 600, stdout=output)
    source = work / 'src/main.rs'
    source.parent.mkdir()
    source.write_text(HOST)
    dependencies = tomllib.loads((root / 'Cargo.toml').read_text())['workspace']['dependencies']
    helper_name = work.name + '-host'
    manifest = f'[package]\nname="{helper_name}"\nversion="0.0.0"\nedition="2024"\n[workspace]\n'
    for name in ['anyhow', 'serde_json', 'sha2', 'tracing-subscriber',
                 'tiangong-plugin-runtime', 'tiangong-config']:
        spec = dependencies[name]
        if isinstance(spec, str):
            spec = {'version': spec}
        spec = dict(spec)
        if 'path' in spec:
            spec['path'] = str(root / spec['path'])
        manifest += f'\n[dependencies.{name}]\n'
        manifest += ''.join(f'{key}={json.dumps(value)}\n' for key, value in spec.items())
    (work / 'Cargo.toml').write_text(manifest)
    shutil.copy2(root / 'Cargo.lock', work / 'Cargo.lock')
    bounded(['cargo', 'build', '--offline', '--manifest-path', str(work / 'Cargo.toml'),
             '--target-dir', str(root / 'target')], 600)
    os.link(debug / helper_name, work / 'host')
    success('构建当前源码和调用真实 Runtime 的临时宿主')


def launched_pids(log):
    return {int(match.group(1)) for line in log.read_text().splitlines()
            if 'stdio sidecar 已启动' in line
            for match in re.finditer(r'\bpid=(\d+)', line)}


def host_case(mode, sandbox, action='stop', delay=0, number=0):
    name = f'{mode}-sandbox={sandbox}-{action}-delay={delay}-{number}'
    directory = work / name
    directory.mkdir()
    log = directory / 'host.log'
    env = environment()
    if mode == 'mute':
        env['TIANGONG_TEST_STDIO_MUTE'] = '1'
    with log.open('w') as output:
        host = subprocess.Popen([str(work / 'host'), mode, str(directory),
                                 str(debug / 'test-stdio-sidecar'), str(int(sandbox)), str(delay)],
                                stdin=subprocess.PIPE, stdout=output, stderr=output,
                                env=env, start_new_session=True)
        children.append(host)

        def marker(text):
            if host.poll() is not None:
                raise AssertionError(f'{name} 宿主提前结束：\n{log.read_text()}')
            groups.update(launched_pids(log))
            return text in log.read_text().splitlines()

        terminal = mode in ('race', 'mute', 'restart')
        wait_until(lambda: marker('CLOSED' if terminal else 'READY'), 30, f'{name} 未就绪')
        pids = launched_pids(log)
        assert pids, f'{name} 没有真正启动 sidecar'
        groups.update(pids)
        child_file = directory / 'child.pid'
        if child_file.exists():
            child_pid = int(child_file.read_text())
            assert alive(child_pid), f'{name} 后台子进程未存活'
            pids.add(child_pid)
        if not terminal:
            assert any(alive(pid) for pid in pids), f'{name} 退出前无进程存活'
            if action == 'kill':
                host.kill()
            else:
                host.stdin.write((action + '\n').encode())
                host.stdin.flush()
            if action == 'stop':
                wait_until(lambda: marker('CLOSED'), 10, f'{name} stop 未完成')
            else:
                host.wait(timeout=5)
        try:
            wait_until(lambda: all(not alive(pid) for pid in pids), 5, f'{name} 进程仍存活')
        except AssertionError:
            live = [pid for pid in pids if alive(pid)]
            with (directory / 'remaining-processes.log').open('w') as output:
                subprocess.run(['ps', '-o', 'pid,ppid,pgid,stat,command', '-p',
                                ','.join(map(str, live))], stdout=output, timeout=5)
            raise AssertionError(f'{name} 残留 PID：{live}') from None
        if host.poll() is None:
            # 宿主继续活着时观察晚到的预热/补验证，避免靠宿主退出才清理。
            before = launched_pids(log)
            time.sleep(0.5)
            assert launched_pids(log) == before, f'{name} 关闭完成后又启动进程'
            host.stdin.write(b'release\n')
            host.stdin.flush()
            assert host.wait(timeout=5) == 0
        elif action == 'exit':
            assert host.returncode == 0
        elif action == 'kill':
            assert host.returncode == -signal.SIGKILL
        host.stdin.close()
    success(name)


def blocked_io(sandbox, own_group):
    name = f'blocked-io-sandbox={sandbox}-own-group={own_group}'
    directory = work / name
    directory.mkdir()
    fifo = directory / 'blocked.fifo'
    os.mkfifo(fifo)
    reader = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
    binary = debug / 'tiangong-fs-sidecar'
    env = environment()
    env.update(TIANGONG_PLUGIN_TRANSPORT='stdio', TIANGONG_PLUGIN_HOST_PID=str(os.getpid()),
               TIANGONG_PLUGIN_ID='fs', TIANGONG_PLUGIN_DATA_DIR=str(directory), RUST_LOG='info')
    if own_group:
        env['TIANGONG_SIDECAR_OWN_PROCESS_GROUP'] = '1'
    policy = None
    kwargs = {}
    if sandbox:
        request = dict(protocol_version=1, policy_schema=2,
                       policy=dict(mode='workspace_write', workspace=str(directory)),
                       program=str(binary), program_root=str(binary.parent),
                       program_sha256=hashlib.sha256(binary.read_bytes()).hexdigest())
        data = json.dumps(request).encode()
        policy = tempfile.TemporaryFile()
        policy.write(len(data).to_bytes(4, 'big') + data)
        policy.flush()
        policy.seek(0)
        # 将策略描述符固定到 fd3；Python 在 exec 前只保留白名单描述符。
        policy_fd = policy.fileno()

        def install_policy_fd():
            os.dup2(policy_fd, 3, inheritable=True)
            if policy_fd != 3:
                os.close(policy_fd)

        kwargs['pass_fds'] = tuple({3, policy_fd})
        kwargs['preexec_fn'] = install_policy_fd
    with (directory / 'sidecar.log').open('w') as output:
        process = subprocess.Popen([str(debug / 'tiangong-sandbox')] if sandbox else [str(binary)],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=output,
                                   env=env, start_new_session=True, **kwargs)
        children.append(process)
        groups.add(process.pid)
        if policy is not None:
            policy.close()
        try:
            frames = [dict(kind='auth', token=''), dict(kind='request', request_id='blocked',
                payload=dict(protocol_version='0.1.0', request_id='blocked', operation='fs.write_file',
                    payload=dict(workspace=str(directory), full_trust=True, path=str(fifo),
                                 content='x' * (1024 * 1024), append=True)))]
            for frame in frames:
                process.stdin.write((json.dumps(frame) + '\n').encode())
                process.stdin.flush()
            assert select.select([reader], [], [], 5)[0], '未观察到 FIFO 数据，不能证明写入已开始'
            assert not select.select([process.stdout], [], [], 0.1)[0], '写入提前返回，未形成阻塞'
            started = time.monotonic()
            process.stdin.close()
            process.wait(timeout=3)
            assert process.returncode in (0, -signal.SIGKILL), process.returncode
            print(f'  已观察到真实 FIFO 写入；关闭输入后 {time.monotonic() - started:.3f}s 退出', flush=True)
        finally:
            os.close(reader)
            process.stdout.close()
    success(name)


def cleanup(processes, process_groups):
    for process in processes:
        if process.poll() is None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    for group in process_groups:
        try:
            os.killpg(group, signal.SIGKILL)
        except ProcessLookupError:
            pass


def case(operation):
    before = len(children)
    previous_groups = set(groups)
    try:
        operation()
    except Exception as error:
        failures.append(str(error))
        print(f'FAIL {error}', flush=True)
        for log in work.glob('*/host.log'):
            groups.update(launched_pids(log))
        cleanup(children[before:], groups - previous_groups)


try:
    check_environment()
    build()
    for sandbox in (False, True):
        for own_group in (False, True):
            case(lambda: blocked_io(sandbox, own_group))
        for mode in ('idle', 'busy'):
            for action in ('stop', 'exit', 'kill'):
                case(lambda: host_case(mode, sandbox, action))
        case(lambda: host_case('mute', sandbox))
        case(lambda: host_case('restart', sandbox))
    for number in range(3):
        for delay in (0, 5, 25, 100):
            case(lambda: host_case('race', True, delay=delay, number=number))
    print(f'通过：{len(passed)} 项；失败：{len(failures)} 项；环境跳过：0。', flush=True)
    if failures:
        raise SystemExit(1)
finally:
    # 回收失败用例的宿主及其独立进程组，不触碰其他天工实例。
    for log in work.glob('*/host.log'):
        groups.update(launched_pids(log))
    cleanup(children, groups)
PY
