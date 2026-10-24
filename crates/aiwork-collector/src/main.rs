//! AI Work 助手 Windows 采集器。
//!
//! 职责：读取本机 Trae CN / WorkBuddy / Qoder IDE 的登录态，生成一个口令加密的
//! `.aiworkpack`，供 Web 版服务端一次导入。不做账号池、签到、网关等运行期功能。

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod dpapi;
#[cfg(windows)]
mod jwt;
#[cfg(windows)]
mod scanner;

#[cfg(windows)]
fn main() -> eframe::Result<()> {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("aiwork-collector 仅支持 Windows：本机登录态解密依赖 DPAPI 与 Chromium 本地数据。");
}
