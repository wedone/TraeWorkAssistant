//! Qoder IDE 本地登录态扫描。
//!
//! 数据来源（与桌面版 `commands/qoder/ide_store.rs` 同链路）：
//! - `%APPDATA%\QoderCN\Local State` → os_crypt.encrypted_key → DPAPI → AES 密钥
//! - `%APPDATA%\QoderCN\User\globalStorage\state.vscdb`
//!   键 `secret://aicoding.auth.userInfo`（v10 密文 Buffer JSON）
//!
//! 设备指纹：优先读取数据目录根级 `machineid`（客户端真实指纹）；否则生成一份并
//! 持久化到 `%LOCALAPPDATA%\AIWorkCollector\qoder_devices.json`，保证多次导出、
//! 服务端与其它设备使用同一指纹。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::dpapi;

const KEY_USER_INFO: &str = "secret://aicoding.auth.userInfo";

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
struct DeviceProfile {
    machine_id: String,
    device_id: String,
    umid: String,
}

pub fn scan() -> Result<(Option<Value>, Vec<String>), String> {
    let mut messages = Vec::new();
    let Some(data_dir) = ide_data_dir() else {
        messages.push(
            "Qoder：未找到 IDE 数据目录（已探测 %APPDATA%\\QoderCN 与 %APPDATA%\\Qoder；请确认已安装并登录 Qoder IDE）"
                .into(),
        );
        return Ok((None, messages));
    };
    let login = match scan_ide_login(&data_dir) {
        Ok(l) => l,
        Err(e) => {
            messages.push(format!("Qoder：{e}"));
            return Ok((None, messages));
        }
    };
    if login.token.is_empty() {
        messages.push("Qoder：登录态中无 token（请先在 Qoder IDE 登录）".into());
        return Ok((None, messages));
    }

    let id = account_id_of(&login.token);
    let profile = stable_profile(&data_dir, &login.uid);
    let expires_sec = login
        .expires_at_ms
        .and_then(clamp_expires_at)
        .map(|ms| ms / 1000);
    let nickname = if login.name.is_empty() {
        format!("Qoder {}", &id[3..9])
    } else {
        login.name.clone()
    };
    let has_refresh = !login.refresh_token.is_empty();

    let doc = json!({
        "kind": "aiwork-qoder-pool",
        "version": 1,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "include_credentials": true,
        "groups": [],
        "accounts": [{
            "id": id,
            "uid": login.uid.clone(),
            "nickname": nickname.clone(),
            "phone_masked": "",
            "plan": "",
            "credential_source": "ide_store",
            "token_expires_at": expires_sec,
            "needs_relogin": false,
            "relogin_reason": "",
            "group_id": "",
            "note": "",
            "credits_balance": null,
            "credits_fetched_at": null,
            "credits_expire_at": null,
            "device_profile": {
                "machine_id": profile.machine_id,
                "device_id": profile.device_id,
                "umid": profile.umid,
            },
            "tags": [],
            "credential": {
                "access_token": login.token,
                "refresh_token": login.refresh_token,
                "expires_at_ms": login.expires_at_ms,
                "uid": login.uid,
                "nickname": nickname,
                "kind": "client",
                "pat": "",
                "machine_id": "",
                "machine_token": "",
            },
        }],
    });
    messages.push(format!(
        "Qoder：找到 1 个账号（token + refreshToken{}）",
        if has_refresh { "" } else { "，无 refreshToken" }
    ));
    Ok((Some(doc), messages))
}

fn ide_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    ["QoderCN", "Qoder"]
        .iter()
        .map(|n| PathBuf::from(&appdata).join(n))
        .find(|p| p.is_dir())
}

fn ide_aes_key(data_dir: &Path) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let raw = std::fs::read_to_string(data_dir.join("Local State"))
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

fn decrypt_v10(enc: &[u8], key: &[u8]) -> Option<String> {
    if enc.len() < 19 || &enc[..3] != b"v10" {
        return None;
    }
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&enc[3..15]), &enc[15..])
        .ok()?;
    String::from_utf8(plain).ok()
}

fn parse_buffer(text: &str) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.get("data")?.as_array().map(|a| {
        a.iter()
            .filter_map(|x| x.as_u64().map(|n| n as u8))
            .collect()
    })
}

fn read_vscdb_key(vscdb: &Path, key: &str) -> Result<Option<String>, String> {
    if !vscdb.is_file() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(
        vscdb,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("state.vscdb 打开失败（可能被 IDE 占用）: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_millis(1500))
        .map_err(|e| format!("state.vscdb busy_timeout 设置失败: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT value FROM ItemTable WHERE key = ?1")
        .map_err(|e| format!("state.vscdb 查询准备失败: {e}"))?;
    let mut rows = stmt
        .query(rusqlite::params![key])
        .map_err(|e| format!("state.vscdb 查询执行失败: {e}"))?;
    let Some(row) = rows.next().map_err(|e| format!("state.vscdb 读取行失败: {e}"))? else {
        return Ok(None);
    };
    let val = row
        .get::<_, rusqlite::types::Value>(0)
        .map_err(|e| format!("state.vscdb 键值类型读取失败: {e}"))?;
    match val {
        rusqlite::types::Value::Text(s) => Ok(Some(s)),
        rusqlite::types::Value::Blob(b) => {
            String::from_utf8(b)
                .map(Some)
                .map_err(|e| format!("state.vscdb BLOB 非 UTF-8: {e}"))
        }
        _ => Ok(None),
    }
}

struct IdeLogin {
    uid: String,
    token: String,
    refresh_token: String,
    name: String,
    expires_at_ms: Option<i64>,
}

fn scan_ide_login(data_dir: &Path) -> Result<IdeLogin, String> {
    let key = ide_aes_key(data_dir)?;
    let vscdb = data_dir.join("User").join("globalStorage").join("state.vscdb");
    let raw = read_vscdb_key(&vscdb, KEY_USER_INFO)?
        .ok_or("state.vscdb 无登录态（可能未登录或已清除）")?;
    let bytes = parse_buffer(&raw).ok_or("登录态键值形态不识别")?;
    let plain = decrypt_v10(&bytes, &key).ok_or("登录态解密失败（密钥或密文不匹配）")?;
    let v: Value =
        serde_json::from_str(&plain).map_err(|e| format!("userInfo 解析失败: {e}"))?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Ok(IdeLogin {
        uid: s("id"),
        token: s("token"),
        refresh_token: s("refreshToken"),
        name: s("name"),
        expires_at_ms: expire_ms_of(&v),
    })
}

fn expire_ms_of(v: &Value) -> Option<i64> {
    let raw = v.get("expireTime")?;
    let ts = raw
        .as_i64()
        .or_else(|| raw.as_str()?.trim().parse::<i64>().ok())?;
    Some(if ts >= 1_000_000_000_000 { ts } else { ts * 1000 })
}

fn clamp_expires_at(ms: i64) -> Option<i64> {
    let now = chrono::Utc::now().timestamp_millis();
    let max = now + 10 * 365 * 24 * 3_600_000;
    if ms <= 0 || ms > max {
        None
    } else {
        Some(ms)
    }
}

/// 本机真实 machineid：32/64 位 hex
fn capture_local_machine_id(data_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(data_dir.join("machineid")).ok()?;
    let s = raw.trim();
    let hex_ok = (s.len() == 32 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit());
    hex_ok.then(|| s.to_ascii_lowercase())
}

/// 稳定指纹：本地 machineid 优先；否则按 uid 持久化生成一次
fn stable_profile(data_dir: &Path, uid: &str) -> DeviceProfile {
    let local_mid = capture_local_machine_id(data_dir);
    let key = if uid.is_empty() { "local" } else { uid };
    let mut saved = load_profiles();
    let mut profile = saved.get(key).cloned().unwrap_or_else(|| {
        let p = generate_profile();
        saved.insert(key.to_string(), p.clone());
        save_profiles(&saved);
        p
    });
    if let Some(mid) = local_mid {
        if profile.machine_id != mid {
            profile.machine_id = mid;
            saved.insert(key.to_string(), profile.clone());
            save_profiles(&saved);
        }
    }
    profile
}

fn profiles_path() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(local)
        .join("AIWorkCollector")
        .join("qoder_devices.json")
}

fn load_profiles() -> HashMap<String, DeviceProfile> {
    std::fs::read_to_string(profiles_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_profiles(map: &HashMap<String, DeviceProfile>) {
    let path = profiles_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(path, text);
    }
}

fn generate_profile() -> DeviceProfile {
    let seed = uuid::Uuid::new_v4().to_string();
    let mut h = Sha256::new();
    h.update(seed.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    DeviceProfile {
        machine_id: hex[..32].to_string(),
        device_id: uuid::Uuid::new_v4().to_string(),
        umid: uuid::Uuid::new_v4().to_string(),
    }
}

/// 与桌面版同规则：qd-<sha256(token) 前 12 位十六进制小写>
fn account_id_of(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("qd-{}", &hex[..12])
}
