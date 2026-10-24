//! CLI 入口：Trae OAuth 与 WorkBuddy/Qoder 导出。
//!
//! 用法：
//! ```text
//! aiwork-collector-cli --trae-oauth --server <URL> --token <管理令牌>
//! aiwork-collector-cli --export <路径> [--password <口令>] [--apps workbuddy,qoder]
//! ```
//! 口令优先取 `--password`，否则读环境变量 `AIWORK_BUNDLE_PASSWORD`；
//! 管理令牌也可读环境变量 `AIWORK_ADMIN_TOKEN`。

use std::path::PathBuf;

use aiwork_bundle::{BundlePayload, MIN_PASSWORD_CHARS};

use crate::scanner::{self, Selection};

const USAGE: &str = "用法:\n  aiwork-collector-cli --trae-oauth --server <URL> --token <管理令牌>\n  aiwork-collector-cli --export <路径> [--password <口令>] [--apps workbuddy,qoder]";

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }

    let mut export: Option<String> = None;
    let mut password: Option<String> = None;
    let mut apps = String::from("workbuddy,qoder");
    let mut server: Option<String> = None;
    let mut token: Option<String> = None;
    let mut trae_oauth = false;
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
            "--server" => {
                i += 1;
                server = args.get(i).cloned();
            }
            "--token" => {
                i += 1;
                token = args.get(i).cloned();
            }
            "--trae-oauth" => trae_oauth = true,
            _ => {}
        }
        i += 1;
    }

    if trae_oauth {
        let server = server
            .or_else(|| std::env::var("AIWORK_SERVER_URL").ok())
            .unwrap_or_default();
        let token = token
            .or_else(|| std::env::var("AIWORK_ADMIN_TOKEN").ok())
            .unwrap_or_default();
        return match crate::trae_oauth::login(&server, &token) {
            Ok(msg) => {
                println!("{msg}");
                0
            }
            Err(e) => {
                eprintln!("Trae OAuth 失败：{e}");
                1
            }
        };
    }

    let Some(export) = export else {
        eprintln!("{USAGE}");
        return 2;
    };
    let password = password
        .or_else(|| std::env::var("AIWORK_BUNDLE_PASSWORD").ok())
        .unwrap_or_default();
    if password.chars().count() < MIN_PASSWORD_CHARS {
        eprintln!("口令至少需要 {MIN_PASSWORD_CHARS} 个字符");
        return 2;
    }

    if has_app(&apps, "trae") {
        eprintln!("Trae 已改为 OAuth 登录，不参与迁移包；请使用 --trae-oauth");
    }
    let sel = Selection {
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
        trae: None,
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
