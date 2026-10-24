//! 统一迁移包（`.aiworkpack`）格式。
//!
//! 设计目标：Windows 采集器扫描本机 Trae / WorkBuddy / Qoder 登录态后，把三类账号
//! 放进一个口令加密包，Web 端一次导入。包内载荷直接复用各池现有导出文档结构，
//! 服务端解密后分发给各池既有导入函数，避免重写导入逻辑。
//!
//! 外层 JSON：
//! ```json
//! {
//!   "kind": "aiwork-bundle",
//!   "version": 1,
//!   "exported_at": "2026-10-24T...",
//!   "apps": ["trae", "workbuddy", "qoder"],
//!   "cipher": "AES-256-GCM",
//!   "kdf": "argon2id",
//!   "data": "<base64(magic+salt+nonce+ciphertext)>"
//! }
//! ```
//!
//! 二进制布局：magic(8) + salt(16) + nonce(12) + AES-256-GCM 密文（含 16 字节 tag）。
//! 口令派生：Argon2id（19 MiB / t=2 / p=1 / 32B），与 Qoder 导出信封同档。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;

/// 二进制魔数（8 字节，含两个填充字节便于人眼识别）
const MAGIC: &[u8; 8] = b"AIWPK1\0\0";
const MAGIC_LEN: usize = 8;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
/// 最小口令长度：包内含账号凭证，禁止弱口令
pub const MIN_PASSWORD_CHARS: usize = 8;

const KIND: &str = "aiwork-bundle";
const VERSION: i64 = 1;

/// 统一载荷：各应用段直接承载对应池的导出文档（保持既有导入函数兼容）。
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct BundlePayload {
    #[serde(default)]
    pub exported_at: String,
    /// 采集来源描述（仅展示，如主机名），不含凭证
    #[serde(default)]
    pub source: String,
    /// 包内包含的应用段（仅展示与预检）
    #[serde(default)]
    pub apps: Vec<String>,
    /// Trae 池文档：`kind = aiwork-trae-pool`
    #[serde(default)]
    pub trae: Option<Value>,
    /// WorkBuddy 池文档：`kind = aiwork-workbuddy-pool`
    #[serde(default)]
    pub workbuddy: Option<Value>,
    /// Qoder 池文档：`kind = aiwork-qoder-pool`
    #[serde(default)]
    pub qoder: Option<Value>,
    /// Buddy 国际版池文档：`kind = buddy-ai-pool`
    #[serde(default)]
    pub buddy_ai: Option<Value>,
}

impl BundlePayload {
    /// 是否至少包含一个有效应用段（空包 / null 段拒绝导出与导入）
    pub fn has_any_app(&self) -> bool {
        let present = |v: &Option<Value>| v.as_ref().is_some_and(|x| !x.is_null());
        present(&self.trae)
            || present(&self.workbuddy)
            || present(&self.qoder)
            || present(&self.buddy_ai)
    }
}

/// 校验口令：非空白且长度达到下限（按字符计，兼容中文口令）
fn validate_password(password: &str) -> Result<(), String> {
    if password.trim().is_empty() {
        return Err("口令不能为空".into());
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!("口令至少需要 {MIN_PASSWORD_CHARS} 个字符"));
    }
    Ok(())
}

/// Argon2id 派生 32 字节密钥
fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| format!("Argon2 参数构建失败: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut key[..])
        .map_err(|e| format!("口令派生失败: {e}"))?;
    Ok(key)
}

/// 加密载荷并生成外层信封 JSON。
pub fn seal(payload: &BundlePayload, password: &str) -> Result<Value, String> {
    validate_password(password)?;
    if !payload.has_any_app() {
        return Err("包内没有任何账号数据".into());
    }
    let plain = Zeroizing::new(
        serde_json::to_vec(payload).map_err(|e| format!("载荷序列化失败: {e}"))?,
    );

    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let key = derive_key(password, &salt)?;

    let cipher = Aes256Gcm::new_from_slice(&key[..])
        .map_err(|e| format!("AES 初始化失败: {e}"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_slice())
        .map_err(|_| "载荷加密失败".to_string())?;

    let mut blob = Vec::with_capacity(MAGIC_LEN + SALT_LEN + NONCE_LEN + ciphertext.len());
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);

    Ok(json!({
        "kind": KIND,
        "version": VERSION,
        "exported_at": payload.exported_at.clone(),
        "source": payload.source.clone(),
        "apps": payload.apps.clone(),
        "cipher": "AES-256-GCM",
        "kdf": "argon2id",
        "data": base64::engine::general_purpose::STANDARD.encode(&blob),
    }))
}

/// 解开外层信封并返回载荷。错误信息不包含口令或明文。
pub fn open(envelope: &Value, password: &str) -> Result<BundlePayload, String> {
    validate_password(password)?;
    if envelope.get("kind").and_then(Value::as_str) != Some(KIND) {
        return Err("不是 AI Work 迁移包（缺少 aiwork-bundle 标记）".into());
    }
    if envelope.get("version").and_then(Value::as_i64) != Some(VERSION) {
        return Err("迁移包版本不识别（version 必须为 1）".into());
    }
    let b64 = envelope
        .get("data")
        .and_then(Value::as_str)
        .ok_or("迁移包缺少 data 字段")?;
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("迁移包 data 解码失败: {e}"))?;
    if blob.len() < MAGIC_LEN + SALT_LEN + NONCE_LEN + 16 {
        return Err("迁移包长度异常".into());
    }
    if &blob[..MAGIC_LEN] != MAGIC {
        return Err("迁移包魔数不匹配（文件可能已损坏）".into());
    }
    // 切分时不复制：ciphertext 直接引用 blob 尾部
    let salt = &blob[MAGIC_LEN..MAGIC_LEN + SALT_LEN];
    let nonce = &blob[MAGIC_LEN + SALT_LEN..MAGIC_LEN + SALT_LEN + NONCE_LEN];
    let ciphertext = &blob[MAGIC_LEN + SALT_LEN + NONCE_LEN..];

    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key[..])
        .map_err(|e| format!("AES 初始化失败: {e}"))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| "口令错误或迁移包已损坏".to_string())?;
    let plain = Zeroizing::new(plain);

    let payload: BundlePayload = serde_json::from_slice(plain.as_slice())
        .map_err(|e| format!("迁移包内容解析失败: {e}"))?;
    if !payload.has_any_app() {
        return Err("迁移包内没有任何账号数据".into());
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BundlePayload {
        BundlePayload {
            exported_at: "2026-10-24T00:00:00+08:00".into(),
            source: "test-host".into(),
            apps: vec!["trae".into(), "workbuddy".into()],
            trae: Some(json!({
                "kind": "aiwork-trae-pool",
                "version": 1,
                "accounts": [{"userId": "1001", "cloudIdeJwt": "Cloud-IDE-JWT a.b.c"}]
            })),
            workbuddy: Some(json!({
                "kind": "aiwork-workbuddy-pool",
                "version": 1,
                "accounts": [{"id": "wb-0123456789ab", "uid": "u1"}]
            })),
            qoder: None,
            buddy_ai: None,
        }
    }

    #[test]
    fn seal_open_roundtrip() {
        let env = seal(&sample(), "test-password-123").unwrap();
        assert_eq!(env["kind"], KIND);
        assert_eq!(env["version"], VERSION);
        let payload = open(&env, "test-password-123").unwrap();
        assert_eq!(payload.trae.unwrap()["accounts"][0]["userId"], "1001");
        assert_eq!(payload.workbuddy.unwrap()["accounts"][0]["uid"], "u1");
        assert!(payload.qoder.is_none());
    }

    #[test]
    fn wrong_password_rejected() {
        let env = seal(&sample(), "test-password-123").unwrap();
        let err = open(&env, "wrong-password-123").unwrap_err();
        assert!(err.contains("口令错误") || err.contains("损坏"));
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let mut env = seal(&sample(), "test-password-123").unwrap();
        let mut blob = base64::engine::general_purpose::STANDARD
            .decode(env["data"].as_str().unwrap())
            .unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0x01;
        env["data"] = json!(base64::engine::general_purpose::STANDARD.encode(&blob));
        assert!(open(&env, "test-password-123").is_err());
    }

    #[test]
    fn short_password_rejected() {
        assert!(seal(&sample(), "short").is_err());
        assert!(open(&json!({"kind": KIND, "version": VERSION, "data": ""}), "short").is_err());
    }

    #[test]
    fn empty_payload_rejected() {
        let empty = BundlePayload::default();
        assert!(seal(&empty, "test-password-123").is_err());
    }

    #[test]
    fn wrong_kind_or_version_rejected() {
        let env = seal(&sample(), "test-password-123").unwrap();
        let mut bad_kind = env.clone();
        bad_kind["kind"] = json!("other");
        assert!(open(&bad_kind, "test-password-123").is_err());
        let mut bad_version = env;
        bad_version["version"] = json!(2);
        assert!(open(&bad_version, "test-password-123").is_err());
    }
}
