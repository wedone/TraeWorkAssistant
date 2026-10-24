//! 采集器 CLI 入口（控制台程序，便于自动化与管理员权限下测试）。
//!
//! 用法：
//! ```text
//! aiwork-collector-cli --export <路径> [--password <口令>] [--apps trae,workbuddy,qoder]
//! ```
//! 口令也可通过环境变量 `AIWORK_BUNDLE_PASSWORD` 提供，避免出现在进程参数中。

#[cfg(windows)]
fn main() {
    std::process::exit(aiwork_collector::headless::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("aiwork-collector-cli 仅支持 Windows");
}
