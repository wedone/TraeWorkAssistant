//! Trae 客户端身份解析（T1）。
//!
//! 目标：让 OAuth 授权页、模型同步、推理请求三段共用同一套真实身份，
//! 消除"登录声称 IDE、调用声称 SOLO"的内部矛盾。
//!
//! 取值优先级：环境变量 > 本机安装 manifest.json > 内置兜底。
//! 兜底值取自本机 Trae CN IDE 3.4.4 实测（manifest.json + main.log），
//! 不是猜测值；本机升级后重启进程即自动跟随，无需改代码。

use std::sync::OnceLock;

/// 内置兜底：本机 Trae CN IDE 实测值（2026-10-25 取证）
const DEFAULT_APP_VERSION: &str = "3.4.4";
const DEFAULT_BUILD_VERSION: &str = "2.3.93438";
const DEFAULT_VERSION_CODE: &str = "344";
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

/// 客户端版本，如 "3.4.4"
pub fn app_version() -> &'static str {
    &identity().app_version
}

/// 构建号，如 "2.3.93438"
pub fn build_version() -> &'static str {
    &identity().build_version
}

/// 版本号（数字递增），如 "344"
pub fn version_code() -> &'static str {
    &identity().version_code
}

/// 包类型，如 "stable_cn"
pub fn package_type() -> &'static str {
    &identity().package_type
}

/// 客户端 appId，如 "787976"
pub fn app_id() -> &'static str {
    &identity().app_id
}

/// 请求来源标识，如 "IDE"
pub fn req_source() -> &'static str {
    &identity().req_source
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
        package_type: DEFAULT_PACKAGE_TYPE.into(),
        app_id: DEFAULT_APP_ID.into(),
        req_source: DEFAULT_REQ_SOURCE.into(),
        source,
    });
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
    }
}
