//! 迁移包（`.aiworkpack`）导入：解密后按应用段分发给各池既有导入函数。
//!
//! 设计：包内每个应用段就是该池的原生导出文档（`aiwork-trae-pool` /
//! `aiwork-workbuddy-pool` / `aiwork-qoder-pool` / `buddy-ai-pool`），因此这里
//! 不重复实现合并/去重逻辑，直接调用既有导入函数，保持三池口径一致。
//!
//! 单应用失败不阻断其它应用：错误按应用名聚合返回，已成功导入的部分保留
//!（与各池「部分导入 + rejected 明细」的既有语义一致）。

use serde_json::{json, Map, Value};

use crate::fs_utils;
use crate::state::AppState;

/// 导入迁移包。`envelope` 为采集器导出的外层 JSON，`password` 为导出时设置的口令。
pub fn bundle_import(
    state: &AppState,
    envelope: Value,
    password: String,
) -> Result<Value, String> {
    let payload = aiwork_bundle::open(&envelope, &password)?;

    let mut apps: Map<String, Value> = Map::new();
    let mut errors: Map<String, Value> = Map::new();

    // 逐应用导入；某段失败只记入 errors，不中断其余应用
    if let Some(doc) = payload.trae.filter(|v| !v.is_null()) {
        match serde_json::to_string(&doc)
            .map_err(|e| format!("Trae 载荷序列化失败: {e}"))
            .and_then(|content| crate::commands::accounts::accounts_import(state, content, None))
            .and_then(|report| {
                serde_json::to_value(report).map_err(|e| format!("Trae 结果序列化失败: {e}"))
            }) {
            Ok(v) => {
                apps.insert("trae".into(), v);
            }
            Err(e) => {
                errors.insert("trae".into(), json!(e));
            }
        }
    }

    if let Some(doc) = payload.workbuddy.filter(|v| !v.is_null()) {
        match crate::commands::workbuddy::workbuddy_accounts_import(state, doc) {
            Ok(v) => {
                apps.insert("workbuddy".into(), v);
            }
            Err(e) => {
                errors.insert("workbuddy".into(), json!(e));
            }
        }
    }

    if let Some(doc) = payload.qoder.filter(|v| !v.is_null()) {
        // 外层包已解密，内层 Qoder 文档按明文导入（password=None）
        match crate::commands::qoder::qoder_accounts_import(state, doc, None) {
            Ok(v) => {
                apps.insert("qoder".into(), v);
            }
            Err(e) => {
                errors.insert("qoder".into(), json!(e));
            }
        }
    }

    if let Some(doc) = payload.buddy_ai.filter(|v| !v.is_null()) {
        // Buddy 国际版池：同样走既有明文导入路径
        match crate::commands::workbuddy::buddy_ai_accounts_import(state, doc, None) {
            Ok(v) => {
                apps.insert("buddy_ai".into(), v);
            }
            Err(e) => {
                errors.insert("buddy_ai".into(), json!(e));
            }
        }
    }

    // 审计日志只记应用名与成功/失败数量，绝不记录账号或凭证内容
    let ok_apps: Vec<&str> = apps.keys().map(String::as_str).collect();
    let failed_apps: Vec<&str> = errors.keys().map(String::as_str).collect();
    fs_utils::app_log(
        &state.data_dir,
        &format!(
            "迁移包导入: 成功应用 [{}] / 失败应用 [{}]",
            ok_apps.join(","),
            failed_apps.join(",")
        ),
    );

    Ok(json!({
        "apps": apps,
        "errors": errors,
    }))
}
