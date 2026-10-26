//! 实机环境档案池 + 账号绑定（UI「实机环境」页签）。
//!
//! 背景：网关部署在无法安装 Trae 客户端的机器（Debian 容器）时，只能靠内置
//! 兜底值伪造身份。本模块接收真机导出的档案（scripts/export-trae-profile.ps1
//! 或 .py 生成），覆盖版本身份与设备指纹，并解出真机 icube 私钥用于
//! DeviceInfo/DeviceProof，使整条链路声称的身份与真机完全一致。
//!
//! **作用域是账号级，不是全局**：导入的档案进入设备池，由绑定表决定哪个账号
//! 使用它；未绑定账号一律走派生/合成身份。多账号共用同一台真机等于在上游制造
//! 设备聚集，正是风控识别的典型特征，因此不做任何全局回落。
//!
//! 落库（kv）：
//! - `client_profile_pool`：`Vec<ImportedProfile>`（设备池，按 device_id 去重）
//! - `client_profile_creds`：`HashMap<device_id, DeviceCredential>`（含私钥 PEM，
//!   只存服务端自有库，不进日志、不回传前端）
//! - `client_profile_bindings`：`HashMap<uid, device_id>`（账号 → 池内设备）

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fs_utils;
use crate::state::AppState;
use crate::trae_identity::ImportedProfile;

const KV_POOL: &str = "client_profile_pool";
const KV_CREDS: &str = "client_profile_creds";
const KV_BINDINGS: &str = "client_profile_bindings";
/// 旧版全局单例档案（迁移用，迁移后删除）
const KV_LEGACY_PROFILE: &str = "client_profile";
const KV_LEGACY_DEVICE: &str = "client_profile_device";

type Pool = Vec<ImportedProfile>;
type CredMap = HashMap<String, crate::icube_auth::DeviceCredential>;
type BindMap = HashMap<String, String>;

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

/// 池内单个设备（回传前端，绝不含私钥）
#[derive(Debug, Serialize)]
pub struct DeviceView {
    pub device_id: String,
    pub machine_id: String,
    pub app_version: String,
    pub version_code: String,
    pub device_model: String,
    pub os_version: String,
    pub has_private_key: bool,
    /// 绑定到该设备的账号 uid 列表
    pub bound_uids: Vec<String>,
}

/// 池 + 绑定表视图
#[derive(Debug, Serialize, Default)]
pub struct ProfilePoolView {
    pub devices: Vec<DeviceView>,
    /// 账号绑定视图：uid → device_id（未绑定账号不出现在此表）
    pub bindings: HashMap<String, String>,
    /// 内置/本机解析的身份（未绑定账号实际使用的值，供前端对照）
    pub builtin: ImportedProfile,
}

/// 内置身份（未绑定账号实际使用的值）
fn builtin_profile() -> ImportedProfile {
    ImportedProfile {
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

fn load_pool(store: &crate::store::Store) -> Pool {
    store.kv_get::<Pool>(KV_POOL)
}

fn load_creds(store: &crate::store::Store) -> CredMap {
    store.kv_get::<CredMap>(KV_CREDS)
}

fn load_bindings(store: &crate::store::Store) -> BindMap {
    store.kv_get::<BindMap>(KV_BINDINGS)
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

/// 把池 + 绑定表灌入进程内热副本；无池时清除
fn publish(pool: &Pool, bindings: &BindMap) {
    if pool.is_empty() {
        crate::trae_identity::clear_bindings();
        return;
    }
    let mut devices = HashMap::new();
    for p in pool {
        if !p.device_id.is_empty() {
            devices.insert(p.device_id.clone(), p.clone());
        }
    }
    let mut b = crate::trae_identity::ProfileBindings::default();
    for (uid, device_id) in bindings {
        // 绑定的设备已被删除则视为未绑定
        if devices.contains_key(device_id) {
            b.set_binding(uid.clone(), device_id.clone());
        }
    }
    b.set_devices(devices);
    crate::trae_identity::apply_bindings(b);
}

fn view(store: &crate::store::Store, pool: &Pool, bindings: &BindMap) -> ProfilePoolView {
    let creds = load_creds(store);
    let devices = pool
        .iter()
        .map(|p| DeviceView {
            device_id: p.device_id.clone(),
            machine_id: p.machine_id.clone(),
            app_version: p.app_version.clone(),
            version_code: p.version_code.clone(),
            device_model: p.device_model.clone(),
            os_version: p.os_version.clone(),
            has_private_key: creds.contains_key(&p.device_id),
            bound_uids: bindings
                .iter()
                .filter(|(_, d)| **d == p.device_id)
                .map(|(u, _)| u.clone())
                .collect(),
        })
        .collect();
    let bindings = bindings
        .iter()
        .filter(|(_, d)| pool.iter().any(|p| &p.device_id == *d))
        .map(|(u, d)| (u.clone(), d.clone()))
        .collect();
    ProfilePoolView {
        devices,
        bindings,
        builtin: builtin_profile(),
    }
}

/// 设备池 + 账号绑定（前端展示用）
pub fn client_profile_get(state: &AppState) -> ProfilePoolView {
    let store = crate::store::db(&state.data_dir);
    let pool = load_pool(&store);
    let bindings = load_bindings(&store);
    view(&store, &pool, &bindings)
}

/// 导入实机档案：解析 → 解密凭证 → 入池（按 device_id 去重）→ 热更新。
/// 只入池，不自动绑定任何账号。
pub fn client_profile_import(state: &AppState, content: String) -> Result<ProfilePoolView, String> {
    let profile = parse_profile(&content)?;
    let cred = parse_credential(&content, &profile)?;
    if profile.device_id.is_empty() {
        return Err("档案缺少 device_id，无法作为设备池条目（需能唯一标识一台设备）".into());
    }
    let store = crate::store::db(&state.data_dir);
    let mut pool = load_pool(&store);
    let mut creds = load_creds(&store);
    let device_id = profile.device_id.clone();

    // 同 device_id 视为同一台真机：覆盖档案字段，保留原私钥（新档案无凭证时）
    match pool.iter_mut().find(|p| p.device_id == device_id) {
        Some(existing) => *existing = profile.clone(),
        None => pool.push(profile.clone()),
    }
    match cred {
        Some(c) => {
            creds.insert(device_id.clone(), c);
        }
        None => {
            // 无凭证段时清掉旧凭证，避免档案与私钥不同源
            creds.remove(&device_id);
        }
    }
    store
        .kv_set(KV_POOL, &pool)
        .map_err(|e| format!("设备池写入失败: {e}"))?;
    store
        .kv_set(KV_CREDS, &creds)
        .map_err(|e| format!("设备凭证写入失败: {e}"))?;
    let bindings = load_bindings(&store);
    publish(&pool, &bindings);
    fs_utils::app_log(
        &state.data_dir,
        &format!(
            "实机环境档案已入池: app={}/{}, device_id={}, machine_id={}, 私钥={}（未自动绑定账号）",
            profile.app_version,
            profile.version_code,
            device_id,
            if profile.machine_id.is_empty() { "-" } else { "已设置" },
            if creds.contains_key(&device_id) { "有" } else { "无" }
        ),
    );
    Ok(view(&store, &pool, &bindings))
}

/// 绑定账号到池内设备（替代原绑定）
pub fn client_profile_bind(
    state: &AppState,
    uid: String,
    device_id: String,
) -> Result<ProfilePoolView, String> {
    if uid.trim().is_empty() {
        return Err("uid 不能为空".into());
    }
    let store = crate::store::db(&state.data_dir);
    let pool = load_pool(&store);
    if !pool.iter().any(|p| p.device_id == device_id) {
        return Err(format!("设备池中不存在 device_id={device_id}"));
    }
    let mut bindings = load_bindings(&store);
    bindings.insert(uid.clone(), device_id.clone());
    store
        .kv_set(KV_BINDINGS, &bindings)
        .map_err(|e| format!("绑定表写入失败: {e}"))?;
    publish(&pool, &bindings);
    fs_utils::app_log(
        &state.data_dir,
        &format!("账号 {uid} 已绑定实机设备 {device_id}"),
    );
    Ok(view(&store, &pool, &bindings))
}

/// 解绑账号（回退派生/合成身份）
pub fn client_profile_unbind(state: &AppState, uid: String) -> Result<ProfilePoolView, String> {
    let store = crate::store::db(&state.data_dir);
    let mut bindings = load_bindings(&store);
    bindings.remove(&uid);
    store
        .kv_set(KV_BINDINGS, &bindings)
        .map_err(|e| format!("绑定表写入失败: {e}"))?;
    let pool = load_pool(&store);
    publish(&pool, &bindings);
    fs_utils::app_log(
        &state.data_dir,
        &format!("账号 {uid} 已解绑实机设备，回退派生身份"),
    );
    Ok(view(&store, &pool, &bindings))
}

/// 从池中删除设备（连带凭证与所有指向它的绑定）
pub fn client_profile_delete(
    state: &AppState,
    device_id: String,
) -> Result<ProfilePoolView, String> {
    let store = crate::store::db(&state.data_dir);
    let mut pool = load_pool(&store);
    pool.retain(|p| p.device_id != device_id);
    let mut creds = load_creds(&store);
    creds.remove(&device_id);
    let mut bindings = load_bindings(&store);
    bindings.retain(|_, d| *d != device_id);
    store
        .kv_set(KV_POOL, &pool)
        .map_err(|e| format!("设备池写入失败: {e}"))?;
    store
        .kv_set(KV_CREDS, &creds)
        .map_err(|e| format!("设备凭证写入失败: {e}"))?;
    store
        .kv_set(KV_BINDINGS, &bindings)
        .map_err(|e| format!("绑定表写入失败: {e}"))?;
    publish(&pool, &bindings);
    fs_utils::app_log(
        &state.data_dir,
        &format!("实机设备 {device_id} 已从池中删除（同时解除相关账号绑定）"),
    );
    Ok(view(&store, &pool, &bindings))
}

/// 清空设备池（全部设备 + 凭证 + 绑定）
pub fn client_profile_clear(state: &AppState) -> Result<ProfilePoolView, String> {
    let store = crate::store::db(&state.data_dir);
    let _ = store.kv_delete(KV_POOL);
    let _ = store.kv_delete(KV_CREDS);
    let _ = store.kv_delete(KV_BINDINGS);
    crate::trae_identity::clear_bindings();
    fs_utils::app_log(&state.data_dir, "实机设备池已清空，所有账号回退派生身份");
    Ok(ProfilePoolView {
        builtin: builtin_profile(),
        ..Default::default()
    })
}

/// 进程启动时载入设备池并重建绑定（旧版全局单例档案就地迁入池，不绑定任何账号）
pub fn apply_persisted(data_dir: &Path) {
    let store = crate::store::db(data_dir);
    migrate_legacy(&store);
    let pool = load_pool(&store);
    let bindings = load_bindings(&store);
    if pool.is_empty() {
        crate::trae_identity::clear_bindings();
        return;
    }
    publish(&pool, &bindings);
}

/// 旧版「全局唯一档案」迁移：入池第 1 项，不自动绑定任何账号
/// （原先所有账号共用一台真机，正是本次改造要消除的设备聚集风险）
fn migrate_legacy(store: &crate::store::Store) {
    let legacy: ImportedProfile = store.kv_get(KV_LEGACY_PROFILE);
    if !is_imported(&legacy) {
        let _ = store.kv_delete(KV_LEGACY_PROFILE);
        let _ = store.kv_delete(KV_LEGACY_DEVICE);
        return;
    }
    if !legacy.device_id.is_empty() {
        let mut pool = load_pool(store);
        if !pool.iter().any(|p| p.device_id == legacy.device_id) {
            pool.push(legacy.clone());
            let _ = store.kv_set(KV_POOL, &pool);
        }
        let old = kv_json(store, KV_LEGACY_DEVICE);
        if let Some(c) = crate::icube_auth::device_credential_from_json(&old) {
            let mut creds = load_creds(store);
            creds.entry(legacy.device_id.clone()).or_insert(c);
            let _ = store.kv_set(KV_CREDS, &creds);
        }
    }
    let _ = store.kv_delete(KV_LEGACY_PROFILE);
    let _ = store.kv_delete(KV_LEGACY_DEVICE);
}

/// 池内某设备的凭证（登录/交换/签名用）
pub fn pool_credential(data_dir: &Path, device_id: &str) -> Option<crate::icube_auth::DeviceCredential> {
    if device_id.is_empty() {
        return None;
    }
    load_creds(&crate::store::db(data_dir))
        .get(device_id)
        .cloned()
}
