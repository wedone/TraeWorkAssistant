//! CLI 扫描/导出：供 `aiwork-collector-cli` 与自动化测试使用。
//!
//! 用法：
//! ```text
//! aiwork-collector-cli --export <路径> [--password <口令>] [--apps trae,workbuddy,qoder]
//! ```
//! 口令优先取 `--password`，否则读环境变量 `AIWORK_BUNDLE_PASSWORD`。

use std::path::PathBuf;

use aiwork_bundle::{BundlePayload, MIN_PASSWORD_CHARS};

use crate::scanner::{self, Selection};

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "用法: aiwork-collector-cli --export <路径> [--password <口令>] [--apps trae,workbuddy,qoder]"
        );
        return 0;
    }

    let mut export: Option<String> = None;
    let mut password: Option<String> = None;
    let mut apps = String::from("trae,workbuddy,qoder");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--export" => {
                i += 1;
                export = args.get(i).cloned();
            }
            "--password" => {
                i += 1;
                password = args.get(i).cloned();
            }
            "--apps" => {
                i += 1;
                apps = args.get(i).cloned().unwrap_or_default();
            }
            _ => {}
        }
        i += 1;
    }

    let Some(export) = export else {
        eprintln!("缺少 --export <路径>");
        return 2;
    };
    let password = password
        .or_else(|| std::env::var("AIWORK_BUNDLE_PASSWORD").ok())
        .unwrap_or_default();
    if password.chars().count() < MIN_PASSWORD_CHARS {
        eprintln!("口令至少需要 {MIN_PASSWORD_CHARS} 个字符");
        return 2;
    }

    let sel = Selection {
        trae: has_app(&apps, "trae"),
        workbuddy: has_app(&apps, "workbuddy") || has_app(&apps, "wb"),
        qoder: has_app(&apps, "qoder"),
    };
    let out = scanner::scan(sel);
    for msg in &out.messages {
        println!("{msg}");
    }
    if !out.has_any() {
        eprintln!("未找到可导出的账号");
        return 1;
    }

    let mut selected_apps = Vec::new();
    if out.trae.is_some() {
        selected_apps.push("trae".to_string());
    }
    if out.workbuddy.is_some() {
        selected_apps.push("workbuddy".to_string());
    }
    if out.qoder.is_some() {
        selected_apps.push("qoder".to_string());
    }
    let payload = BundlePayload {
        exported_at: chrono::Local::now().to_rfc3339(),
        source: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
        apps: selected_apps,
        trae: out.trae,
        workbuddy: out.workbuddy,
        qoder: out.qoder,
        buddy_ai: None,
    };

    let envelope = match aiwork_bundle::seal(&payload, &password) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("生成迁移包失败：{e}");
            return 1;
        }
    };
    let text = match serde_json::to_string_pretty(&envelope) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("序列化迁移包失败：{e}");
            return 1;
        }
    };
    let path = PathBuf::from(export);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    if let Err(e) = std::fs::write(&path, text) {
        eprintln!("写入文件失败（{}）：{e}", path.display());
        return 1;
    }
    println!("已导出: {}", path.display());
    0
}

fn has_app(apps: &str, name: &str) -> bool {
    apps.split(',')
        .any(|s| s.trim().eq_ignore_ascii_case(name))
}
