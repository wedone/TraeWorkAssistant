//! 极简 GUI：勾选应用 → 扫描 → 设置口令 → 导出 `.aiworkpack`。

use std::path::PathBuf;

use aiwork_bundle::{BundlePayload, MIN_PASSWORD_CHARS};
use eframe::egui;

use crate::scanner::{self, ScanOutcome, Selection};

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 640.0])
            .with_min_inner_size([560.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AI Work 采集器",
        options,
        Box::new(|cc| {
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(CollectorApp::new()))
        }),
    )
}

struct CollectorApp {
    sel: Selection,
    password: String,
    confirm: String,
    output_path: String,
    outcome: Option<ScanOutcome>,
    status: String,
    status_error: bool,
    log: Vec<String>,
}

impl CollectorApp {
    fn new() -> Self {
        Self {
            sel: Selection {
                trae: true,
                workbuddy: true,
                qoder: true,
            },
            password: String::new(),
            confirm: String::new(),
            output_path: default_output_path(),
            outcome: None,
            status: String::new(),
            status_error: false,
            log: Vec::new(),
        }
    }

    fn do_scan(&mut self) {
        let out = scanner::scan(self.sel);
        self.log = out.messages.clone();
        self.status_error = !out.has_any();
        self.status = if out.has_any() {
            "扫描完成。设置口令后即可导出。".into()
        } else {
            "未找到可导出的账号，请确认对应客户端已登录。".into()
        };
        self.outcome = Some(out);
    }

    fn do_export(&mut self) {
        let (trae, workbuddy, qoder) = match self.outcome.as_ref() {
            Some(o) => (o.trae.clone(), o.workbuddy.clone(), o.qoder.clone()),
            None => {
                self.status = "请先扫描本机账号".into();
                self.status_error = true;
                return;
            }
        };
        if trae.is_none() && workbuddy.is_none() && qoder.is_none() {
            self.status = "没有可导出的账号".into();
            self.status_error = true;
            return;
        }
        if self.password.chars().count() < MIN_PASSWORD_CHARS {
            self.status = format!("口令至少需要 {MIN_PASSWORD_CHARS} 个字符");
            self.status_error = true;
            return;
        }
        if self.password != self.confirm {
            self.status = "两次输入的口令不一致".into();
            self.status_error = true;
            return;
        }

        let mut apps = Vec::new();
        if trae.is_some() {
            apps.push("trae".to_string());
        }
        if workbuddy.is_some() {
            apps.push("workbuddy".to_string());
        }
        if qoder.is_some() {
            apps.push("qoder".to_string());
        }
        let payload = BundlePayload {
            exported_at: chrono::Local::now().to_rfc3339(),
            source: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
            apps,
            trae,
            workbuddy,
            qoder,
            buddy_ai: None,
        };

        let envelope = match aiwork_bundle::seal(&payload, &self.password) {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("生成迁移包失败：{e}");
                self.status_error = true;
                return;
            }
        };
        let text = match serde_json::to_string_pretty(&envelope) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("序列化迁移包失败：{e}");
                self.status_error = true;
                return;
            }
        };

        let path = PathBuf::from(self.output_path.trim());
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.status = format!("已导出：{}", path.display());
                self.status_error = false;
                // 导出成功后清空口令，减少内存驻留
                self.password.clear();
                self.confirm.clear();
            }
            Err(e) => {
                self.status = format!("写入文件失败（{}）：{e}", path.display());
                self.status_error = true;
            }
        }
    }
}

impl eframe::App for CollectorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("AI Work 采集器");
            ui.label("扫描本机 Trae / WorkBuddy / Qoder 登录态，导出加密迁移包供 Web 端导入。");
            ui.add_space(6.0);
            ui.separator();

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.sel.trae, "Trae CN");
                ui.checkbox(&mut self.sel.workbuddy, "WorkBuddy");
                ui.checkbox(&mut self.sel.qoder, "Qoder");
            });
            if ui.button("扫描本机账号").clicked() {
                self.do_scan();
            }
            if let Some(o) = &self.outcome {
                ui.label(format!(
                    "扫描结果：Trae {} 个 / WorkBuddy {} 个 / Qoder {} 个",
                    o.counts.trae, o.counts.workbuddy, o.counts.qoder
                ));
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(110.0)
                .show(ui, |ui| {
                    for line in &self.log {
                        ui.monospace(line);
                    }
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("导出路径：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.output_path)
                        .desired_width(f32::INFINITY),
                );
            });
            ui.horizontal(|ui| {
                ui.label("口令：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(220.0),
                );
                ui.label("确认：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.confirm)
                        .password(true)
                        .desired_width(220.0),
                );
            });

            let can_export = self
                .outcome
                .as_ref()
                .map(ScanOutcome::has_any)
                .unwrap_or(false)
                && self.password.chars().count() >= MIN_PASSWORD_CHARS
                && self.password == self.confirm
                && !self.output_path.trim().is_empty();
            if ui
                .add_enabled(can_export, egui::Button::new("导出加密包"))
                .clicked()
            {
                self.do_export();
            }

            if !self.status.is_empty() {
                let color = if self.status_error {
                    egui::Color32::from_rgb(190, 55, 55)
                } else {
                    egui::Color32::from_rgb(30, 130, 70)
                };
                ui.colored_label(color, &self.status);
            }

            ui.add_space(8.0);
            ui.separator();
            ui.small("说明：");
            ui.small("• 迁移包内含账号凭证，等同于密码，请妥善保管或导入后删除。");
            ui.small("• Trae 本地只能导出 JWT（无 refresh_token），到期后需在 Web 端 OAuth 一次。");
            ui.small("• WorkBuddy / Qoder 含 refresh_token，服务端可长期续期。");
        });
    }
}

fn default_output_path() -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    let desktop = PathBuf::from(&home).join("Desktop");
    let dir = if desktop.is_dir() {
        desktop
    } else {
        PathBuf::from(&home)
    };
    dir.join(format!(
        "aiwork-bundle-{}.aiworkpack",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ))
    .to_string_lossy()
    .to_string()
}

/// 加载 Windows 自带中文字体（egui 默认字体不含 CJK，否则界面显示方框）
fn install_cjk_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in [
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}
