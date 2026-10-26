//! 实机环境档案导入（UI「实机环境」页签）。
//!
//! 背景：网关部署在无法安装 Trae 客户端的机器（Debian 容器）时，只能靠内置
//! 兜底值伪造身份。本模块接收真机导出的档案（scripts/export-trae-profile.ps1
//! 或 .py 生成），覆盖版本身份与设备指纹，并解出真机 icube 私钥用于
//! DeviceInfo/DeviceProof，使整条链路声称的身份与真机完全一致。
//!
//! 落库：
//! - kv `client_profile`：ImportedProfile（版本 + 指纹 + device_id/machine_id）
//! - kv `client_profile_device`：DeviceCredential（含私钥 PEM，只存服务端自有库，
//!   不进日志、不回传前端）

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fs_utils;
use crate::state::AppState;
use crate::trae_identity::ImportedProfile;

const KV_PROFILE: &str = "client_profile";
const KV_DEVICE: &str = "client_profile_device";

/// 导出文件中的版本身份段
#[derive(Debug, Deserialize)]
struct IdentitySec {
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    build_version: String,
    #[serde(default)]
    version_code: String,
    #[serde(default)]
    package_type: String,
    #[serde(default)]
    app_id: String,
}

/// 导出文件中的设备指纹段
#[derive(Debug, Deserialize)]
struct DeviceSec {
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    machine_id: String,
    #[serde(default)]
    device_model: String,
    #[serde(default)]
    device_manufacturer: String,
    #[serde(default)]
    cpu: String,
    #[serde(default)]
    os_name: String,
    #[serde(default)]
    os_version: String,
}

/// 导出文件中的 icube 凭证段（密文，私钥明文只在后端解密后短暂存在）
#[derive(Debug, Deserialize)]
struct CredentialSec {
    #[serde(default)]
    tc_envelope: String,
    /// 键形如 `iCubeAuthInfo://icube-dc:<deviceId>`，device_id 缺失时由此兜底
    #[serde(default)]
    storage_key: String,
}

/// 导出文件（分段格式）
#[derive(Debug, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    identity: Option<IdentitySec>,
    #[serde(default)]
    device: Option<DeviceSec>,
    #[serde(default)]
    credential: Option<CredentialSec>,
}

/// 导入结果（回传前端，绝不含私钥）
#[derive(Debug, Serialize)]
pub struct ProfileView {
    pub imported: bool,
    pub profile: ImportedProfile,
    pub device_id: String,
    pub machine_id: String,
    pub has_private_key: bool,
}

impl ProfileView {
    fn empty() -> Self {
        Self {
            imported: false,
            profile: ImportedProfile::default(),
            device_id: String::new(),
            machine_id: String::new(),
            has_private_key: false,
        }
    }
}

fn is_imported(p: &ImportedProfile) -> bool {
    !p.app_version.is_empty()
        || !p.device_id.is_empty()
        || !p.machine_id.is_empty()
        || !p.os_version.is_empty()
}

/// 读取 kv 中的 JSON 文档（缺失/解析失败返回 null）
fn kv_json(store: &crate::store::Store, key: &str) -> Value {
    store
        .kv_get_raw(key)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null)
}

/// 全部为 0-9 且长度合理（真机 16 位，历史合成 15 位）
fn valid_device_id(s: &str) -> bool {
    (10..=20).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
}

/// machineId 形态：真实客户端为 64 位十六进制
fn valid_machine_id(s: &str) -> bool {
    s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// 解析导出文件为规范化档案（分段格式与扁平格式均接受）
fn parse_profile(content: &str) -> Result<ImportedProfile, String> {
    let v: Value = serde_json::from_str(content).map_err(|e| format!("档案不是合法 JSON: {e}"))?;
    // 扁平格式：直接就是 ImportedProfile 的字段
    let mut p = serde_json::from_value::<ImportedProfile>(v.clone()).unwrap_or_default();
    if let Ok(file) = serde_json::from_value::<ProfileFile>(v) {
        if let Some(id) = file.identity {
            p.app_version = id.app_version;
            p.build_version = id.build_version;
            p.version_code = id.version_code;
            p.package_type = id.package_type;
            p.app_id = id.app_id;
        }
        if let Some(d) = file.device {
            if p.device_id.is_empty() {
                p.device_id = d.device_id;
            }
            if p.machine_id.is_empty() {
                p.machine_id = d.machine_id;
            }
            if p.device_model.is_empty() {
                p.device_model = d.device_model;
            }
            if p.device_manufacturer.is_empty() {
                p.device_manufacturer = d.device_manufacturer;
            }
            if p.cpu.is_empty() {
                p.cpu = d.cpu;
            }
            if p.os_name.is_empty() {
                p.os_name = d.os_name;
            }
            if p.os_version.is_empty() {
                p.os_version = d.os_version;
            }
        }
    }
    if !is_imported(&p) {
        return Err("档案缺少身份/设备字段（app_version、device_id、machine_id 至少一项）".into());
    }
    if !p.device_id.is_empty() && !valid_device_id(&p.device_id) {
        return Err(format!(
            "device_id 形态异常（应为 10-20 位纯数字）：{}",
            p.device_id
        ));
    }
    if !p.machine_id.is_empty() && !valid_machine_id(&p.machine_id) {
        return Err(format!(
            "machine_id 形态异常（应为至少 32 位十六进制）：{}",
            p.machine_id
        ));
    }
    Ok(p)
}

/// 从凭证段解出 DeviceCredential（无凭证段返回 None）
fn parse_credential(
    content: &str,
    profile: &ImportedProfile,
) -> Result<Option<crate::icube_auth::DeviceCredential>, String> {
    let Ok(v) = serde_json::from_str::<ProfileFile>(content) else {
        return Ok(None);
    };
    let Some(cred) = v.credential else {
        return Ok(None);
    };
    let envelope = cred.tc_envelope.trim();
    if envelope.is_empty() {
        return Ok(None);
    }
    let plain = crate::icube_auth::tc_decrypt(envelope, false)
        .map_err(|e| format!("icube 凭证解密失败: {e}"))?;
    let parsed: Value =
        serde_json::from_str(&plain).map_err(|e| format!("icube 凭证内容非 JSON: {e}"))?;
    let pem = parsed
        .get("privateKeyPEM")
        .and_then(|x| x.as_str())
        .filter(|s| s.contains("BEGIN"))
        .ok_or_else(|| "icube 凭证缺少 privateKeyPEM".to_string())?;
    // device_id 优先取档案，其次取 storage_key 内嵌值
    let mut device_id = profile.device_id.clone();
    if device_id.is_empty() {
        if let Some(id) = cred.storage_key.split("icube-dc:").nth(1) {
            device_id = id.trim().to_string();
        }
    }
    if device_id.is_empty() {
        return Err("凭证缺少 device_id（档案 device.device_id 与 storage_key 均为空）".into());
    }
    // 私钥可解析性校验：避免导入后到登录才发现密钥坏了
    let check = crate::icube_auth::DeviceCredential {
        device_id: device_id.clone(),
        private_key_pem: pem.to_string(),
        machine_id: profile.machine_id.clone(),
        app_version: profile.app_version.clone(),
        source_app: "imported".to_string(),
    };
    crate::icube_auth::device_public_key_pem(&check)
        .map_err(|e| format!("icube 私钥不可用: {e}"))?;
    Ok(Some(check))
}

/// 当前生效档案（前端展示用）
pub fn client_profile_get(state: &AppState) -> ProfileView {
    let store = crate::store::db(&state.data_dir);
    let profile: ImportedProfile = store.kv_get(KV_PROFILE);
    if !is_imported(&profile) {
        let effective = ImportedProfile {
            app_version: crate::trae_identity::app_version().to_string(),
            build_version: crate::trae_identity::build_version().to_string(),
            version_code: crate::trae_identity::version_code().to_string(),
            ide_version_code: crate::trae_identity::ide_version_code().to_string(),
            package_type: crate::trae_identity::package_type().to_string(),
            app_id: crate::trae_identity::app_id().to_string(),
            os_name: crate::trae_identity::os_name().to_string(),
            os_version: crate::trae_identity::os_version().to_string(),
            device_model: crate::trae_identity::device_model().to_string(),
            device_manufacturer: crate::trae_identity::device_manufacturer().to_string(),
            cpu: crate::trae_identity::cpu().to_string(),
            device_id: String::new(),
            machine_id: String::new(),
        };
        return ProfileView {
            imported: false,
            profile: effective,
            device_id: String::new(),
            machine_id: String::new(),
            has_private_key: false,
        };
    }
    let cred = kv_json(&store, KV_DEVICE);
    let has_key = cred
        .get("private_key_pem")
        .and_then(|x| x.as_str())
        .map(|s| s.contains("BEGIN"))
        .unwrap_or(false);
    ProfileView {
        imported: true,
        device_id: profile.device_id.clone(),
        machine_id: profile.machine_id.clone(),
        has_private_key: has_key,
        profile,
    }
}

/// 导入实机档案：解析 → 解密凭证 → 落库 → 进程内热应用
pub fn client_profile_import(state: &AppState, content: String) -> Result<ProfileView, String> {
    let profile = parse_profile(&content)?;
    let cred = parse_credential(&content, &profile)?;
    let store = crate::store::db(&state.data_dir);
    store
        .kv_set(KV_PROFILE, &profile)
        .map_err(|e| format!("档案写入失败: {e}"))?;
    match cred {
        Some(c) => {
            store
                .kv_set(KV_DEVICE, &c)
                .map_err(|e| format!("设备凭证写入失败: {e}"))?;
        }
        None => {
            // 无凭证段时清掉旧凭证，避免档案与私钥不同源
            let _ = store.kv_delete(KV_DEVICE);
        }
    }
    crate::trae_identity::apply_imported(profile.clone());
    let has_key = !kv_json(&store, KV_DEVICE).is_null();
    fs_utils::app_log(
        &state.data_dir,
        &format!(
            "实机环境档案已导入: app={}/{}, device_id={}, machine_id={}, 私钥={}",
            profile.app_version,
            profile.version_code,
            if profile.device_id.is_empty() { "-" } else { &profile.device_id },
            if profile.machine_id.is_empty() { "-" } else { "已设置" },
            if has_key { "有" } else { "无" }
        ),
    );
    Ok(client_profile_get(state))
}

/// 清除导入档案（回退内置/本机扫描身份）
pub fn client_profile_clear(state: &AppState) -> Result<ProfileView, String> {
    let store = crate::store::db(&state.data_dir);
    let _ = store.kv_delete(KV_PROFILE);
    let _ = store.kv_delete(KV_DEVICE);
    crate::trae_identity::clear_imported();
    fs_utils::app_log(&state.data_dir, "实机环境档案已清除，回退内置身份");
    Ok(ProfileView::empty())
}

/// 进程启动时应用已落库的档案（无档案则不覆盖）
pub fn apply_persisted(data_dir: &Path) {
    let store = crate::store::db(data_dir);
    let profile: ImportedProfile = store.kv_get(KV_PROFILE);
    if !is_imported(&profile) {
        return;
    }
    crate::trae_identity::apply_imported(profile);
}

/// 已导入的真机凭证（登录/交换/签名用）；未导入返回 None
pub fn imported_credential(data_dir: &Path) -> Option<crate::icube_auth::DeviceCredential> {
    let store = crate::store::db(data_dir);
    let v = kv_json(&store, KV_DEVICE);
    crate::icube_auth::device_credential_from_json(&v)
}
