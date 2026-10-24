//! WorkBuddy（CodeBuddy）本地 auth 文件扫描。
//!
//! 数据来源：`%LOCALAPPDATA%\CodeBuddyExtension\Data\Public\auth\workbuddy-desktop.info`
//! （明文 JSON，含 accessToken / refreshToken）。该文件同时含 CN 与国际版账号；
//! 国际版（workbuddy.ai）会被服务端 CN 池拒绝，消息中给出提示。

use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn scan() -> Result<(Option<Value>, Vec<String>), String> {
    let path = auth_file_path();
    let mut messages = Vec::new();
    if !path.is_file() {
        messages.push(format!(
            "WorkBuddy：未找到 auth 文件 {}（请先在 WorkBuddy 客户端登录）",
            path.display()
        ));
        return Ok((None, messages));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("auth 文件读取失败: {e}"))?;
    let source: Value =
        serde_json::from_str(&raw).map_err(|e| format!("auth 文件 JSON 解析失败: {e}"))?;

    let auth = source.get("auth").filter(|v| v.is_object()).unwrap_or(&source);
    let account = source
        .get("account")
        .filter(|v| v.is_object())
        .unwrap_or(&source);

    let access = first_str(auth, &["accessToken", "access_token", "token"]).unwrap_or_default();
    if access.is_empty() {
        return Err("auth 文件存在但未找到 accessToken，请先在 WorkBuddy 客户端登录".into());
    }
    let refresh =
        first_str(auth, &["refreshToken", "refresh_token"]).unwrap_or_default();
    let expires_at_ms = first_i64(
        auth,
        &[
            "expiresAtMs",
            "expires_at_ms",
            "expiresAt",
            "accessTokenExpiresAtMs",
        ],
    );
    let refresh_expires_at_ms = first_i64(
        auth,
        &["refreshExpiresAtMs", "refresh_expires_at_ms", "refreshExpiresAt"],
    );
    let uid = first_str(account, &["uid", "userId", "user_id", "id"])
        .filter(|s| !s.is_empty())
        .or_else(|| crate::jwt::parse(&access).user_id)
        .unwrap_or_default();
    let nickname =
        first_str(account, &["nickname", "name", "displayName"]).unwrap_or_default();
    let edition =
        first_str(account, &["editionType", "edition_type", "edition"]).unwrap_or_default();
    let domain = first_str(&source, &["domain"]).unwrap_or_default();

    if domain.contains(".workbuddy.ai") || domain == "workbuddy.ai" {
        messages.push(
            "WorkBuddy：检测到国际版账号（workbuddy.ai），服务端 CN 池会拒绝；请在 BuddyAi 页面单独处理"
                .into(),
        );
    }

    let id = account_id_of(&access);
    let has_refresh = !refresh.is_empty();
    let doc = json!({
        "kind": "aiwork-workbuddy-pool",
        "version": 1,
        "exported_at": chrono::Local::now().to_rfc3339(),
        "include_credentials": true,
        "accounts": [{
            "id": id,
            "uid": uid.clone(),
            "nickname": nickname.clone(),
            "phone_masked": "",
            "edition_type": edition.clone(),
            "access_token_expires_at": expires_at_ms.map(|v| v / 1000),
            "refresh_token_expires_at": refresh_expires_at_ms.map(|v| v / 1000),
            "group_id": "",
            "note": "",
            "tags": [],
            "credential": {
                "access_token": access,
                "refresh_token": refresh,
                "expires_at_ms": expires_at_ms,
                "refresh_expires_at_ms": refresh_expires_at_ms,
                "uid": uid,
                "domain": domain,
                "nickname": nickname,
                "edition": edition,
            },
        }],
    });
    messages.push(format!(
        "WorkBuddy：找到 1 个账号（access + refresh token{}）",
        if has_refresh { "" } else { "，无 refresh_token" }
    ));
    Ok((Some(doc), messages))
}

fn auth_file_path() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    PathBuf::from(local)
        .join("CodeBuddyExtension")
        .join("Data")
        .join("Public")
        .join("auth")
        .join("workbuddy-desktop.info")
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(*k))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn first_i64(v: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| v.get(*k)).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    })
}

/// 与桌面版同规则：wb-<sha256(token) 前 12 位十六进制小写>
fn account_id_of(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("wb-{}", &hex[..12])
}
