mod logging;

fn main() -> anyhow::Result<()> {
    // 初始化日志系统
    // _guard 需要保持存活，否则日志不会刷新
    let _guard = logging::init_logging()?;

    tiangong_entry::run()
}
