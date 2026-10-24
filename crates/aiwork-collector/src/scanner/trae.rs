//! Trae CN 本地登录态扫描。
//!
//! 数据来源（与桌面版 `device_proxy/local_capture.rs` 同链路）：
//! - `%APPDATA%\Trae CN\Network\Cookies`（Chromium SQLite，v10 密文 + DPAPI 密钥）
//! - `%APPDATA%\Trae CN\Partitions\trae-webview\Cookies`
//! - `%APPDATA%\Trae CN\Local Storage\leveldb`（明文兜底扫描）
//!
//! 限制：该链路只能拿到 Cloud-IDE-JWT，拿不到 refresh_token；JWT 到期后需在 Web 端
//! 重新 OAuth 一次。扫描结果不含任何其他本地数据。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::dpapi;
use crate::jwt;

const APP_DIR_NAME: &str = "Trae CN";

/// Trae 账号数据：uid -> (完整 JWT 值, exp)
type JwtMap = HashMap<String, (String, Option<i64>)>;

pub fn scan() -> Result<(Option<Value>, Vec<String>), String> {
    let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA 环境变量缺失".to_string())?;
    let dir = PathBuf::from(appdata).join(APP_DIR_NAME);
    let mut messages = Vec::new();
    if !dir.is_dir() {
        messages.push(format!(
            "Trae：未找到数据目录 {}（请确认已安装并登录 Trae CN）",
            dir.display()
        ));
        return Ok((None, messages));
    }

    let mut map: JwtMap = HashMap::new();
    let key = match chrome_aes_key(&dir) {
        Ok(k) => Some(k),
        Err(e) => {
            messages.push(format!("Trae：未能取得 Cookies 解密密钥（{e}），仅扫描明文值"));
            None
        }
    };

    let mut cookie_errors: Vec<String> = Vec::new();
    let mut locked = false;
    for db in [
        // 主数据目录 Cookies：Trae CN 运行时通常被独占锁定，关闭 Trae 后可读
        dir.join("Network").join("Cookies"),
        // 现代 Chromium 分区路径（Network 子目录）；旧版无该子目录，一并兼容
        dir.join("Partitions")
            .join("trae-webview")
            .join("Network")
            .join("Cookies"),
        dir.join("Partitions").join("trae-webview").join("Cookies"),
    ] {
        if !db.is_file() {
            continue;
        }
        if let Err(e) = scan_cookies_db(&db, key.as_deref(), &mut map) {
            if e.contains("占用") || e.contains("os error 32") {
                locked = true;
            }
            cookie_errors.push(format!("{}：{e}", db.display()));
        }
    }

    for ls in [
        dir.join("Local Storage").join("leveldb"),
        dir.join("Partitions")
            .join("trae-webview")
            .join("Local Storage")
            .join("leveldb"),
    ] {
        if ls.is_dir() {
            scan_leveldb_dir(&ls, &mut map);
        }
    }

    if map.is_empty() {
        if locked {
            messages.push(
                "Trae：Trae CN 正在运行并锁定了 Cookies 数据库，无法读取。\
                 请完全退出 Trae CN（含托盘/后台进程）后重新扫描。"
                    .into(),
            );
        } else if !cookie_errors.is_empty() {
            messages.push(format!("Trae：Cookies 读取失败（{}）", cookie_errors.join("；")));
        } else {
            messages.push("Trae：未找到 Cloud-IDE-JWT（请先在 Trae CN 客户端登录）".into());
        }
        return Ok((None, messages));
    }

    let mut uids: Vec<String> = map.keys().cloned().collect();
    uids.sort();
    let accounts: Vec<Value> = uids
        .iter()
        .map(|uid| {
            let (jwt_value, _) = map.get(uid).expect("uid from map");
            json!({
                "userId": uid,
                "name": format!("Trae-…{}", tail4(uid)),
                "cloudIdeJwt": jwt_value,
                // 本地 Cookies/leveldb 不保存 refresh_token
                "refreshToken": "",
                "dcId": null,
                "groupId": null,
                "tags": [],
            })
        })
        .collect();

    messages.push(format!(
        "Trae：找到 {} 个账号（仅 JWT，无 refresh_token；到期后需在 Web 端 OAuth 一次）",
        accounts.len()
    ));
    let doc = json!({
        "kind": "aiwork-trae-pool",
        "version": 1,
        "exportedAt": chrono::Local::now().to_rfc3339(),
        "appVersion": "aiwork-collector",
        "accountCount": accounts.len(),
        "accounts": accounts,
        "groups": [],
    });
    Ok((Some(doc), messages))
}

fn tail4(uid: &str) -> String {
    uid.chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 读 Local State → os_crypt.encrypted_key（base64）→ DPAPI → AES-256 密钥
fn chrome_aes_key(app_dir: &Path) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let raw = std::fs::read_to_string(app_dir.join("Local State"))
        .map_err(|e| format!("Local State 读取失败: {e}"))?;
    let state: Value =
        serde_json::from_str(&raw).map_err(|e| format!("Local State 解析失败: {e}"))?;
    let b64 = state
        .get("os_crypt")
        .and_then(|v| v.get("encrypted_key"))
        .and_then(Value::as_str)
        .ok_or("Local State 无 os_crypt.encrypted_key")?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("encrypted_key base64 解码失败: {e}"))?;
    if raw.len() < 5 || &raw[..5] != b"DPAPI" {
        return Err("encrypted_key 前缀非 DPAPI".into());
    }
    dpapi::unprotect(&raw[5..])
}

/// v10 密文：'v10' + nonce(12) + ct + tag(16)；旧格式回退 DPAPI
fn decrypt_cookie(enc: &[u8], key: Option<&[u8]>) -> Option<String> {
    if enc.len() >= 3 && &enc[..3] == b"v10" {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
        if enc.len() < 19 {
            return None;
        }
        let key = key?;
        let cipher = Aes256Gcm::new_from_slice(key).ok()?;
        let plain = cipher
            .decrypt(Nonce::from_slice(&enc[3..15]), &enc[15..])
            .ok()?;
        return Some(String::from_utf8_lossy(&plain).to_string());
    }
    dpapi::unprotect(enc)
        .ok()
        .map(|v| String::from_utf8_lossy(&v).to_string())
}

struct TempDirGuard(PathBuf);
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 复制 Cookies 库后只读打开（规避客户端运行时文件锁），-wal/-shm 一并复制
fn scan_cookies_db(db: &Path, key: Option<&[u8]>, map: &mut JwtMap) -> Result<(), String> {
    let td = std::env::temp_dir().join(format!("aiwork_ck_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&td).map_err(|e| format!("临时目录创建失败: {e}"))?;
    let _guard = TempDirGuard(td.clone());
    std::fs::copy(db, td.join("Cookies")).map_err(|e| {
        if e.raw_os_error() == Some(32) {
            "Cookies 文件被 Trae 占用（os error 32）".to_string()
        } else {
            format!("Cookies 复制失败: {e}")
        }
    })?;
    for suffix in ["-wal", "-shm"] {
        let side = db.with_file_name(format!("Cookies{suffix}"));
        if side.exists() {
            let _ = std::fs::copy(&side, td.join(format!("Cookies{suffix}")));
        }
    }

    let conn = rusqlite::Connection::open_with_flags(
        td.join("Cookies"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("打开 Cookies 库失败: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT host_key, name, value, encrypted_value FROM cookies")
        .map_err(|e| format!("查询 cookies 表失败: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, Option<Vec<u8>>>(3)?.unwrap_or_default(),
            ))
        })
        .map_err(|e| format!("遍历 cookies 失败: {e}"))?;
    for r in rows.flatten() {
        let (_host, _name, value, enc) = r;
        let mut blob = value;
        if !enc.is_empty() {
            if let Some(d) = decrypt_cookie(&enc, key) {
                blob = d;
            }
        }
        if let Some(hit) = find_cloud_ide_jwt(&blob) {
            insert_jwt(map, &hit);
        }
    }
    Ok(())
}

fn scan_leveldb_dir(ls: &Path, map: &mut JwtMap) {
    static PREFIX_RE: OnceLock<regex::bytes::Regex> = OnceLock::new();
    let re = PREFIX_RE.get_or_init(|| {
        regex::bytes::Regex::new(r"Cloud-IDE-JWT [A-Za-z0-9_\-\.=]+").expect("jwt prefix regex")
    });
    let Ok(entries) = std::fs::read_dir(ls) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.ends_with(".ldb") || name.ends_with(".log")) {
            continue;
        }
        let Ok(data) = std::fs::read(entry.path()) else {
            continue;
        };
        for m in re.find_iter(&data) {
            let hit = String::from_utf8_lossy(m.as_bytes()).to_string();
            if let Some(valid) = jwt::valid_cloud_ide_jwt(
                hit.strip_prefix("Cloud-IDE-JWT ").unwrap_or(&hit).trim(),
            ) {
                insert_jwt(map, &valid);
            }
        }
    }
}

fn insert_jwt(map: &mut JwtMap, jwt_value: &str) {
    let info = jwt::parse(jwt_value);
    let Some(uid) = info.user_id.filter(|s| !s.is_empty()) else {
        return;
    };
    match map.get(&uid) {
        // exp 防降级：保留过期时间更晚的一条
        Some((_, Some(old_exp))) if info.exp.map(|e| e <= *old_exp).unwrap_or(true) => {}
        _ => {
            map.insert(uid, (jwt_value.to_string(), info.exp));
        }
    }
}

/// 显式前缀优先；前缀命中但非法则直接放弃（不落入通用扫描），与桌面版一致
fn find_cloud_ide_jwt(blob: &str) -> Option<String> {
    static PREFIX_RE: OnceLock<regex::Regex> = OnceLock::new();
    static JWT_RE: OnceLock<regex::Regex> = OnceLock::new();
    if let Some(idx) = blob.find("Cloud-IDE-JWT ") {
        let re = PREFIX_RE.get_or_init(|| {
            regex::Regex::new(r"Cloud-IDE-JWT\s+([A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+)")
                .expect("jwt prefix regex")
        });
        let caps = re.captures(&blob[idx..])?;
        return jwt::valid_cloud_ide_jwt(&caps[1]);
    }
    let re = JWT_RE.get_or_init(|| {
        regex::Regex::new(r"[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+")
            .expect("jwt regex")
    });
    for m in re.find_iter(blob) {
        if let Some(v) = jwt::valid_cloud_ide_jwt(m.as_str()) {
            return Some(v);
        }
    }
    None
}
