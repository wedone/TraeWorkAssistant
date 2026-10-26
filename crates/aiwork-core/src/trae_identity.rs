//! Trae 客户端身份解析（T1）。
//!
//! 目标：让 OAuth 授权页、模型同步、推理请求三段共用同一套真实身份，
//! 消除"登录声称 IDE、调用声称 SOLO"的内部矛盾。
//!
//! 取值优先级：环境变量 > 本机安装 manifest.json > 内置兜底。
//! 兜底值取自本机 Trae CN IDE 3.4.4 实测（manifest.json + main.log），
//! 不是猜测值；本机升级后重启进程即自动跟随，无需改代码。

use std::sync::{OnceLock, RwLock};

/// 内置兜底：本机 Trae CN IDE 实测值（2026-10-25 取证）
const DEFAULT_APP_VERSION: &str = "3.4.4";
const DEFAULT_BUILD_VERSION: &str = "2.3.93438";
/// TTNet 侧版本码（manifest.ahaNet.ttnet_params.version_code），如 "344"
const DEFAULT_VERSION_CODE: &str = "344";
/// IDE 侧版本码（x-ide-version-code，取自客户端内置 domain config `versionCode`）。
/// 2026-10-26 本机 ai-completion 日志实证：`x-ide-version-code: "20260212"`；
/// 注意它与 TTNet 的 "344"、TTNet 计算出的 "3004004" 是三个不同的口径。
const DEFAULT_IDE_VERSION_CODE: &str = "20260212";
const DEFAULT_PACKAGE_TYPE: &str = "stable_cn";
const DEFAULT_APP_ID: &str = "787976";
/// IDE 线请求来源标识（客户端 GetUserInfo 自报值）
const DEFAULT_REQ_SOURCE: &str = "IDE";

/// 客户端身份快照（进程内唯一）
#[derive(Debug, Clone)]
pub struct TraeIdentity {
    /// 客户端版本，如 "3.4.4"
    pub app_version: String,
    /// 构建号，如 "2.3.93438"
    pub build_version: String,
    /// 版本号（数字递增），如 "344"；注意与 SOLO 线的日期格式不同
    pub version_code: String,
    /// IDE 侧版本码（x-ide-version-code），如 "20260212"
    pub ide_version_code: String,
    /// 包类型，如 "stable_cn"
    pub package_type: String,
    /// 客户端 appId，如 "787976"
    pub app_id: String,
    /// 请求来源标识，IDE 线固定 "IDE"
    pub req_source: String,
    /// 取值来源：env / local / default（诊断用）
    pub source: &'static str,
}

static IDENTITY: OnceLock<TraeIdentity> = OnceLock::new();

/// 进程级身份（首次调用时解析并缓存）
pub fn identity() -> &'static TraeIdentity {
    IDENTITY.get_or_init(resolve)
}

/// 实机环境导入档案（UI「实机环境」页签导入，进程级生效）。
///
/// 用途：网关部署在无法安装 Trae 客户端的机器（如 Debian）时，用真机导出的
/// 档案覆盖内置/本机扫描值，使登录、推理、模型同步声称的身份与真机一致。
/// 字段为空表示该项不覆盖，继续走内置解析值。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ImportedProfile {
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub build_version: String,
    #[serde(default)]
    pub version_code: String,
    /// IDE 侧版本码（x-ide-version-code）；导出档案未带时沿用内置实证值
    #[serde(default)]
    pub ide_version_code: String,
    #[serde(default)]
    pub package_type: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub os_name: String,
    #[serde(default)]
    pub os_version: String,
    #[serde(default)]
    pub device_model: String,
    #[serde(default)]
    pub device_manufacturer: String,
    #[serde(default)]
    pub cpu: String,
    /// 真机 deviceId（全局唯一设备，登录优先使用）
    #[serde(default)]
    pub device_id: String,
    /// 真机 machineId（telemetry.machineId）
    #[serde(default)]
    pub machine_id: String,
}

/// 导入档案覆盖（None = 未导入，全部走内置解析）。
/// 存入的引用由 Box::leak 获得，进程内恒存活，故可安全返回 &'static。
static IMPORTED: RwLock<Option<&'static ImportedProfile>> = RwLock::new(None);

/// 应用实机档案（重复导入以新值覆盖旧值；旧引用随进程结束释放）
pub fn apply_imported(profile: ImportedProfile) {
    let leaked: &'static ImportedProfile = Box::leak(Box::new(profile));
    *IMPORTED.write().unwrap_or_else(|e| e.into_inner()) = Some(leaked);
}

/// 清除实机档案，回退内置/本机扫描值
pub fn clear_imported() {
    *IMPORTED.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 当前生效的导入档案（未导入为 None）
fn imported() -> Option<&'static ImportedProfile> {
    *IMPORTED.read().unwrap_or_else(|e| e.into_inner())
}

/// 导入档案中非空的 device_id / machine_id（全局唯一设备用）
pub fn imported_login_device() -> Option<(String, String)> {
    let p = imported()?;
    if p.device_id.is_empty() || p.machine_id.is_empty() {
        return None;
    }
    Some((p.device_id.clone(), p.machine_id.clone()))
}

/// 客户端版本，如 "3.4.4"
pub fn app_version() -> &'static str {
    match imported() {
        Some(p) if !p.app_version.is_empty() => &p.app_version,
        _ => &identity().app_version,
    }
}

/// 构建号，如 "2.3.93438"
pub fn build_version() -> &'static str {
    match imported() {
        Some(p) if !p.build_version.is_empty() => &p.build_version,
        _ => &identity().build_version,
    }
}

/// 版本号（数字递增），如 "344"
pub fn version_code() -> &'static str {
    match imported() {
        Some(p) if !p.version_code.is_empty() => &p.version_code,
        _ => &identity().version_code,
    }
}

/// 包类型，如 "stable_cn"
pub fn package_type() -> &'static str {
    match imported() {
        Some(p) if !p.package_type.is_empty() => &p.package_type,
        _ => &identity().package_type,
    }
}

/// 客户端 appId，如 "787976"
pub fn app_id() -> &'static str {
    match imported() {
        Some(p) if !p.app_id.is_empty() => &p.app_id,
        _ => &identity().app_id,
    }
}

/// 请求来源标识，如 "IDE"
pub fn req_source() -> &'static str {
    &identity().req_source
}

/// IDE 侧版本码（x-ide-version-code），如 "20260212"
pub fn ide_version_code() -> &'static str {
    match imported() {
        Some(p) if !p.ide_version_code.is_empty() => &p.ide_version_code,
        _ => &identity().ide_version_code,
    }
}

fn resolve() -> TraeIdentity {
    let local = read_local_manifest();
    let source = if local.is_some() {
        "local"
    } else {
        "default"
    };
    let mut id = local.unwrap_or_else(|| TraeIdentity {
        app_version: DEFAULT_APP_VERSION.into(),
        build_version: DEFAULT_BUILD_VERSION.into(),
        version_code: DEFAULT_VERSION_CODE.into(),
        ide_version_code: DEFAULT_IDE_VERSION_CODE.into(),
        package_type: DEFAULT_PACKAGE_TYPE.into(),
        app_id: DEFAULT_APP_ID.into(),
        req_source: DEFAULT_REQ_SOURCE.into(),
        source,
    });
    // manifest 不含 IDE 侧版本码，恒用内置实证值（可用环境变量覆盖）
    id.ide_version_code = DEFAULT_IDE_VERSION_CODE.into();
    // 环境变量逐字段覆盖（上游更换版本时无需发版）
    if let Some(v) = env_nonempty("TRAE_APP_VERSION") {
        id.app_version = v;
        id.source = "env";
    }
    if let Some(v) = env_nonempty("TRAE_BUILD_VERSION") {
        id.build_version = v;
        id.source = "env";
    }
    if let Some(v) = env_nonempty("TRAE_VERSION_CODE") {
        id.version_code = v;
        id.source = "env";
    }
    if let Some(v) = env_nonempty("TRAE_IDE_VERSION_CODE") {
        id.ide_version_code = v;
        id.source = "env";
    }
    if let Some(v) = env_nonempty("TRAE_PACKAGE_TYPE") {
        id.package_type = v;
        id.source = "env";
    }
    if let Some(v) = env_nonempty("TRAE_APP_ID") {
        id.app_id = v;
        id.source = "env";
    }
    id
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// 本机安装目录候选（按产品线顺序）。
/// 只接受 `packageType` 为国内线的包，避免把国际版（stable_i18n）或
/// 遗留目录当成 CN 身份——本机 `TRAE SOLO` 目录实际就是国际版。
fn manifest_candidates() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let base = std::path::Path::new(&local).join("Programs");
        // Trae CN IDE（当前主线）
        out.push(base.join("Trae CN").join("manifest.json"));
        // 历史 SOLO CN 安装残留（只在国内版本存在时命中）
        out.push(base.join("TRAE SOLO CN").join("manifest.json"));
    }
    out
}

fn read_local_manifest() -> Option<TraeIdentity> {
    for path in manifest_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let package_type = v["packageType"].as_str().unwrap_or("");
        // 只要国内线包；国际版/其他渠道不作为身份来源
        if package_type != "stable_cn" {
            continue;
        }
        let app_version = v["appVersion"].as_str().unwrap_or("").trim();
        let build_version = v["buildVersion"].as_str().unwrap_or("").trim();
        if app_version.is_empty() || build_version.is_empty() {
            continue;
        }
        // version_code 藏在 ahaNet.ttnet_params 内，形如 "344"
        let version_code = v["ahaNet"]["ttnet_params"]["version_code"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_VERSION_CODE);
        return Some(TraeIdentity {
            app_version: app_version.to_string(),
            build_version: build_version.to_string(),
            version_code: version_code.to_string(),
            ide_version_code: DEFAULT_IDE_VERSION_CODE.to_string(),
            package_type: package_type.to_string(),
            app_id: v["appId"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_APP_ID)
                .to_string(),
            req_source: DEFAULT_REQ_SOURCE.to_string(),
            source: "local",
        });
    }
    None
}

/// 内置设备指纹兜底：本机真实 Trae CN 客户端实测（2026-10-26）
/// - os_version：客户端取 Node `os.version()`，本机实测 "Windows 10 Pro"
/// - device_model / device_manufacturer：客户端取系统信息 SDK，本机 WMI 实测
///   Model=INVALID、Manufacturer=LENOVO（客户端原样上报，不做修正）
/// - cpu：客户端取 CPU 型号解析出的厂商名，本机为 Intel
const DEFAULT_OS_NAME: &str = "windows";
const DEFAULT_OS_VERSION: &str = "Windows 10 Pro";
const DEFAULT_DEVICE_MODEL: &str = "INVALID";
const DEFAULT_DEVICE_MANUFACTURER: &str = "LENOVO";
const DEFAULT_CPU: &str = "Intel";

/// 设备指纹档案（P1）。
///
/// 真实客户端把同一份 `commonParams` 同时用于路由请求头（`x-device-brand` /
/// `x-device-type` / `x-os-version`）与 `DeviceInfo`（DeviceModel / DeviceBrand /
/// DeviceCPU / OSInfo / OSVersion）。此前这些值分散硬编码在 routes/models_sync/oauth
/// 三处且互相矛盾（`CREFG-XX`、`Windows 11 Home China`），本身即是风险信号；
/// 集中一处后可保证同一"虚拟机器"的型号、系统、CPU 永远自洽。
///
/// 取值优先级：环境变量 > 内置兜底（兜底即本机真实客户端值，非猜测）。
#[derive(Debug, Clone)]
pub struct ClientProfile {
    /// 操作系统名，客户端 `systemInformation.osName` 口径：windows / mac / linux
    pub os_name: String,
    /// 操作系统版本，客户端 `os.version()` 口径（如 "Windows 10 Pro"）
    pub os_version: String,
    /// 设备型号，客户端 `systemInformation.deviceModel` 口径
    pub device_model: String,
    /// 设备厂商，客户端 `systemInformation.deviceManufacturer` 口径
    pub device_manufacturer: String,
    /// CPU 厂商，客户端 `cpuManufacturer` 口径
    pub cpu: String,
}

static PROFILE: OnceLock<ClientProfile> = OnceLock::new();

/// 进程级设备指纹（首次调用时解析并缓存）
pub fn profile() -> &'static ClientProfile {
    PROFILE.get_or_init(resolve_profile)
}

/// 操作系统名（x-device-type 取值）
pub fn os_name() -> &'static str {
    match imported() {
        Some(p) if !p.os_name.is_empty() => &p.os_name,
        _ => &profile().os_name,
    }
}

/// 操作系统版本（x-os-version / DeviceInfo.OSVersion 取值）
pub fn os_version() -> &'static str {
    match imported() {
        Some(p) if !p.os_version.is_empty() => &p.os_version,
        _ => &profile().os_version,
    }
}

/// 设备型号（x-device-brand / DeviceInfo.DeviceModel 取值）
pub fn device_model() -> &'static str {
    match imported() {
        Some(p) if !p.device_model.is_empty() => &p.device_model,
        _ => &profile().device_model,
    }
}

/// 设备厂商（DeviceInfo.DeviceBrand 取值）
pub fn device_manufacturer() -> &'static str {
    match imported() {
        Some(p) if !p.device_manufacturer.is_empty() => &p.device_manufacturer,
        _ => &profile().device_manufacturer,
    }
}

/// CPU 厂商（x-device-cpu / DeviceInfo.DeviceCPU 取值）
pub fn cpu() -> &'static str {
    match imported() {
        Some(p) if !p.cpu.is_empty() => &p.cpu,
        _ => &profile().cpu,
    }
}

fn resolve_profile() -> ClientProfile {
    ClientProfile {
        os_name: env_nonempty("TRAE_OS_NAME").unwrap_or_else(|| DEFAULT_OS_NAME.into()),
        os_version: env_nonempty("TRAE_OS_VERSION").unwrap_or_else(|| DEFAULT_OS_VERSION.into()),
        device_model: env_nonempty("TRAE_DEVICE_MODEL").unwrap_or_else(|| DEFAULT_DEVICE_MODEL.into()),
        device_manufacturer: env_nonempty("TRAE_DEVICE_MANUFACTURER")
            .unwrap_or_else(|| DEFAULT_DEVICE_MANUFACTURER.into()),
        cpu: env_nonempty("TRAE_DEVICE_CPU").unwrap_or_else(|| DEFAULT_CPU.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 身份字段非空且版本号形态为纯数字（IDE 线特征：344，而非 SOLO 的 20260811）
    #[test]
    fn identity_fields_are_well_formed() {
        let id = identity();
        assert!(!id.app_version.is_empty());
        assert!(!id.build_version.is_empty());
        assert!(!id.package_type.is_empty());
        assert!(!id.version_code.is_empty());
        assert!(
            id.version_code.chars().all(|c| c.is_ascii_digit()),
            "version_code 应为纯数字，实际 {}",
            id.version_code
        );
        // IDE 侧版本码与 TTNet 版本码是两个口径，同样必须非空且纯数字
        assert!(!id.ide_version_code.is_empty());
        assert!(
            id.ide_version_code.chars().all(|c| c.is_ascii_digit()),
            "ide_version_code 应为纯数字，实际 {}",
            id.ide_version_code
        );
    }

    /// 设备指纹字段非空：这些值会原样写进请求头与 DeviceInfo，
    /// 空值即为真实客户端不会出现的形态
    #[test]
    fn client_profile_fields_are_non_empty() {
        let p = profile();
        assert!(!p.os_name.is_empty());
        assert!(!p.os_version.is_empty());
        assert!(!p.device_model.is_empty());
        assert!(!p.device_manufacturer.is_empty());
        assert!(!p.cpu.is_empty());
    }
}
