//! Trae OAuth 驱动：从服务端取登录 URL，捕获本机回调，再交给服务端完成
//! token 交换并入池。
//!
//! 设计取舍：Trae 新版把登录态放在 `storage.json` 的 `iCubeAuthInfo`（`tc` 私有
//! 加密），离线解密不可行；OAuth 是官方链路，也能拿到 refresh_token。这里不复制
//! 整套 PKCE/设备指纹/ExchangeToken 逻辑，而是复用已部署服务端的 OAuth 命令：
//! - `POST /api/cmd/oauth_get_login_url` 生成登录 URL（服务端保存 pending 状态）
//! - 浏览器登录后 302 回 `http://127.0.0.1:17388/authorize`
//! - `POST /api/cmd/oauth_login` 提交回调 URL，服务端交换 token 并落库

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// 与桌面版/服务端一致的 OAuth 回跳地址
const CALLBACK_ADDR: &str = "127.0.0.1:17388";
const CALLBACK_PATH: &str = "/authorize";

#[derive(Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct LoginUrl {
    url: String,
}

#[derive(Deserialize)]
struct LoginResult {
    user_id: String,
    name: String,
    #[serde(default)]
    refresh_token: String,
}

/// 执行一次 Trae OAuth 登录。成功返回面向用户的摘要（不含 token）。
pub fn login(server: &str, token: &str) -> Result<String, String> {
    let server = server.trim().trim_end_matches('/');
    if server.is_empty() {
        return Err("请填写服务器地址（例如 http://10.0.0.9:8383）".into());
    }
    let token = token.trim();
    if token.is_empty() {
        return Err("请填写管理令牌".into());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP 客户端初始化失败: {e}"))?;
    let cookie = format!("aiwork_admin={token}");

    // 先监听回调端口，避免浏览器回跳时端口尚未就绪
    let listener = TcpListener::bind(CALLBACK_ADDR)
        .map_err(|e| format!("无法监听 {CALLBACK_ADDR}（可能被其它程序占用）: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("监听端口初始化失败: {e}"))?;

    let url = fetch_login_url(&client, server, &cookie)?;
    open::that(&url).map_err(|e| {
        format!("打开浏览器失败（{e}）；请手动打开以下链接完成登录：\n{url}")
    })?;

    let callback = wait_callback(&listener)?;
    let result = submit_callback(&client, server, &cookie, &callback)?;
    Ok(format!(
        "Trae 登录成功：{}（uid {}，refresh_token {}）",
        result.name,
        result.user_id,
        if result.refresh_token.is_empty() {
            "缺失"
        } else {
            "已获取"
        }
    ))
}

fn fetch_login_url(
    client: &reqwest::blocking::Client,
    server: &str,
    cookie: &str,
) -> Result<String, String> {
    let resp = client
        .post(format!("{server}/api/cmd/oauth_get_login_url"))
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("{}")
        .send()
        .map_err(|e| format!("请求登录链接失败（服务器是否可达？）: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("读取登录链接响应失败: {e}"))?;
    let env: Envelope<LoginUrl> = serde_json::from_str(&text).map_err(|e| {
        format!("登录链接响应格式异常（HTTP {status}）: {e}")
    })?;
    if !env.ok {
        return Err(env
            .error
            .unwrap_or_else(|| format!("服务端拒绝生成登录链接（HTTP {status}）")));
    }
    env.data
        .map(|d| d.url)
        .ok_or_else(|| "登录链接响应缺少 url".into())
}

fn wait_callback(listener: &TcpListener) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if Instant::now() >= deadline {
            return Err("等待浏览器回调超时（5 分钟），请重试".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let Some(target) = read_target(&mut stream) else {
                    continue;
                };
                if !target.starts_with(CALLBACK_PATH) {
                    respond(&mut stream, 404, "Not Found", "");
                    continue;
                }
                let callback = format!("http://{CALLBACK_ADDR}{target}");
                respond(
                    &mut stream,
                    200,
                    "OK",
                    "<html><meta charset=\"utf-8\"><body style=\"font-family:sans-serif;padding:40px\">\
                     <h2>Trae 登录完成</h2><p>请回到 AI Work 采集器，账号会自动写入服务器。</p>\
                     </body></html>",
                );
                return Ok(callback);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("接收浏览器回调失败: {e}")),
        }
    }
}

fn read_target(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    (method == "GET").then(|| target.to_string())
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
}

fn submit_callback(
    client: &reqwest::blocking::Client,
    server: &str,
    cookie: &str,
    callback: &str,
) -> Result<LoginResult, String> {
    let resp = client
        .post(format!("{server}/api/cmd/oauth_login"))
        .header(reqwest::header::COOKIE, cookie)
        .json(&serde_json::json!({ "callback_url": callback }))
        .send()
        .map_err(|e| format!("提交登录回调失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("读取登录结果失败: {e}"))?;
    let env: Envelope<LoginResult> = serde_json::from_str(&text)
        .map_err(|e| format!("登录结果格式异常（HTTP {status}）: {e}"))?;
    if !env.ok {
        return Err(env
            .error
            .unwrap_or_else(|| format!("服务端拒绝登录（HTTP {status}）")));
    }
    env.data.ok_or_else(|| "登录结果缺少 data".into())
}
