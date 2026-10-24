//! JWT 本地解析（不校验签名）：提取 Cloud-IDE uid 与过期时间。

use base64::Engine;

#[derive(Default, Clone)]
pub struct JwtInfo {
    pub user_id: Option<String>,
    pub exp: Option<i64>,
}

/// 解析 JWT（兼容 `Cloud-IDE-JWT ` / `Bearer ` 前缀）
pub fn parse(jwt_full: &str) -> JwtInfo {
    let trimmed = jwt_full.trim();
    let token = trimmed
        .strip_prefix("Cloud-IDE-JWT ")
        .or_else(|| trimmed.strip_prefix("Bearer "))
        .unwrap_or(trimmed)
        .trim();
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return JwtInfo::default();
    }
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1].trim_end_matches('='))
    else {
        return JwtInfo::default();
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return JwtInfo::default();
    };
    let user_id = payload
        .get("data")
        .and_then(|d| d.get("id"))
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .or_else(|| {
            payload
                .get("auth_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            payload
                .get("sub")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
    let exp = payload.get("exp").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_f64().map(|f| f as i64))
            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
    });
    JwtInfo { user_id, exp }
}

/// 校验并规范 Cloud-IDE-JWT：要求 RS256 + payload.data.id 存在。
/// 返回带 `Cloud-IDE-JWT ` 前缀的完整值（与桌面版写回账号池的形态一致）。
pub fn valid_cloud_ide_jwt(tok: &str) -> Option<String> {
    let parts: Vec<&str> = tok.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let decode = |s: &str| -> Option<serde_json::Value> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.trim_end_matches('='))
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    let header = decode(parts[0])?;
    let payload = decode(parts[1])?;
    let alg_ok = header.get("alg").and_then(|v| v.as_str()) == Some("RS256");
    let has_data_id = payload
        .get("data")
        .and_then(|d| d.get("id"))
        .map(|v| !v.is_null())
        .unwrap_or(false);
    (alg_ok && has_data_id).then(|| format!("Cloud-IDE-JWT {}", tok.trim()))
}
