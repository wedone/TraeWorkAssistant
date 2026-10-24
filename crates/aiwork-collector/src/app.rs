//! 极简 GUI：Trae OAuth 登录 + WorkBuddy/Qoder 扫描导出。
//!
//! Trae 新版登录态是 `storage.json` 中的 `tc` 私有加密，采集器不再尝试离线解密；
//! 改为点击「Trae OAuth 登录」，由本机捕获回调、服务端完成 token 交换并入池。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use aiwork_bundle::{BundlePayload, MIN_PASSWORD_CHARS};
use eframe::egui;

use crate::scanner::{self, ScanOutcome, Selection};

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 760.0])
            .with_min_inner_size([640.0, 620.0]),
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
    server_url: String,
    admin_token: String,
    oauth_busy: bool,
    oauth_status: String,
    oauth_status_error: bool,
    oauth_rx: Option<Receiver<Result<String, String>>>,
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
                workbuddy: true,
                qoder: true,
            },
            server_url: "http://127.0.0.1:8383".into(),
            admin_token: String::new(),
            oauth_busy: false,
            oauth_status: String::new(),
            oauth_status_error: false,
            oauth_rx: None,
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

    fn do_trae_oauth(&mut self) {
        if self.oauth_busy {
            return;
        }
        let server = self.server_url.trim().to_string();
        let token = self.admin_token.trim().to_string();
        if server.is_empty() || token.is_empty() {
            self.oauth_status = "请先填写服务器地址与管理令牌".into();
            self.oauth_status_error = true;
            return;
        }
        self.oauth_busy = true;
        self.oauth_status = "正在打开浏览器，请完成 Trae 登录…".into();
        self.oauth_status_error = false;

        let (tx, rx) = mpsc::channel();
        self.oauth_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(crate::trae_oauth::login(&server, &token));
        });
    }

    fn poll_oauth(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.oauth_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(msg)) => {
                self.oauth_busy = false;
                self.oauth_status = msg;
                self.oauth_status_error = false;
            }
            Ok(Err(e)) => {
                self.oauth_busy = false;
                self.oauth_status = format!("Trae 登录失败：{e}");
                self.oauth_status_error = true;
            }
            Err(TryRecvError::Empty) => {
                self.oauth_rx = Some(rx);
                ctx.request_repaint_after(Duration::from_millis(200));
            }
            Err(TryRecvError::Disconnected) => {
                self.oauth_busy = false;
                self.oauth_status = "登录线程异常退出，请重试".into();
                self.oauth_status_error = true;
            }
        }
    }

    fn do_export(&mut self) {
        let (workbuddy, qoder) = match self.outcome.as_ref() {
            Some(o) => (o.workbuddy.clone(), o.qoder.clone()),
            None => {
                self.status = "请先扫描本机账号".into();
                self.status_error = true;
                return;
            }
        };
        if workbuddy.is_none() && qoder.is_none() {
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
            trae: None,
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
        self.poll_oauth(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("AI Work 采集器");
                ui.label("Trae 通过 OAuth 直接登录服务器；WorkBuddy / Qoder 扫描本机登录态打包导入。");
                ui.add_space(6.0);
                ui.separator();

                // ---------- Trae OAuth ----------
                ui.label(egui::RichText::new("Trae 登录（推荐）").strong());
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("服务器：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.server_url)
                            .desired_width(260.0)
                            .hint_text("http://10.0.0.9:8383"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("管理令牌：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.admin_token)
                            .password(true)
                            .desired_width(320.0)
                            .hint_text("服务器 /var/lib/aiwork/conf/admin_token"),
                    );
                });
                let can_oauth = !self.oauth_busy
                    && !self.server_url.trim().is_empty()
                    && !self.admin_token.trim().is_empty();
                let label = if self.oauth_busy {
                    "等待浏览器登录…"
                } else {
                    "Trae OAuth 登录"
                };
                if ui
                    .add_enabled(can_oauth, egui::Button::new(label))
                    .clicked()
                {
                    self.do_trae_oauth();
                }
                if !self.oauth_status.is_empty() {
                    let color = if self.oauth_status_error {
                        egui::Color32::from_rgb(190, 55, 55)
                    } else {
                        egui::Color32::from_rgb(30, 130, 70)
                    };
                    ui.colored_label(color, &self.oauth_status);
                }
                ui.small("点击后打开浏览器；登录完成账号直接写入服务器，不进入迁移包。");

                ui.add_space(6.0);
                ui.separator();

                // ---------- 本机扫描 ----------
                ui.label(egui::RichText::new("本机账号扫描").strong());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.sel.workbuddy, "WorkBuddy");
                    ui.checkbox(&mut self.sel.qoder, "Qoder");
                });
                if ui.button("扫描本机账号").clicked() {
                    self.do_scan();
                }
                if let Some(o) = &self.outcome {
                    ui.label(format!(
                        "扫描结果：WorkBuddy {} 个 / Qoder {} 个",
                        o.counts.workbuddy, o.counts.qoder
                    ));
                }
                egui::ScrollArea::vertical()
                    .max_height(96.0)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.monospace(line);
                        }
                    });

                ui.add_space(6.0);
                ui.separator();

                // ---------- 导出 ----------
                ui.label(egui::RichText::new("导出迁移包").strong());
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
                            .desired_width(200.0),
                    );
                    ui.label("确认：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.confirm)
                            .password(true)
                            .desired_width(200.0),
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
                ui.small("• 迁移包只包含 WorkBuddy / Qoder；Trae 通过 OAuth 直接入池。");
                ui.small("• Trae 新版登录态为 storage.json 私有加密，采集器不再离线读取。");
                ui.small("• 迁移包内含账号凭证，等同于密码，请妥善保管或导入后删除。");
            });
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
