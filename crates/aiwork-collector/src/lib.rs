//! Windows 采集器核心库：GUI 与 CLI 两个入口共享扫描/导出逻辑。

#[cfg(windows)]
pub mod app;
#[cfg(windows)]
pub mod dpapi;
#[cfg(windows)]
pub mod headless;
#[cfg(windows)]
pub mod jwt;
#[cfg(windows)]
pub mod scanner;
#[cfg(windows)]
pub mod trae_oauth;
