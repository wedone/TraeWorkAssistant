//! 本机登录态扫描：按应用分发到独立扫描器，单应用失败不影响其它应用。

pub mod qoder;
pub mod trae;
pub mod workbuddy;

use serde_json::Value;

/// 用户勾选要扫描的应用
#[derive(Default, Clone, Copy)]
pub struct Selection {
    pub trae: bool,
    pub workbuddy: bool,
    pub qoder: bool,
}

#[derive(Default, Clone, Copy)]
pub struct Counts {
    pub trae: usize,
    pub workbuddy: usize,
    pub qoder: usize,
}

#[derive(Default)]
pub struct ScanOutcome {
    pub trae: Option<Value>,
    pub workbuddy: Option<Value>,
    pub qoder: Option<Value>,
    pub counts: Counts,
    /// 面向用户的扫描消息（不含 token）
    pub messages: Vec<String>,
}

impl ScanOutcome {
    pub fn has_any(&self) -> bool {
        self.trae.is_some() || self.workbuddy.is_some() || self.qoder.is_some()
    }
}

fn count_accounts(doc: &Option<Value>) -> usize {
    doc.as_ref()
        .and_then(|d| d.get("accounts"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub fn scan(sel: Selection) -> ScanOutcome {
    let mut out = ScanOutcome::default();

    if sel.trae {
        match trae::scan() {
            Ok((doc, msgs)) => {
                out.counts.trae = count_accounts(&doc);
                out.trae = doc;
                out.messages.extend(msgs);
            }
            Err(e) => out.messages.push(format!("Trae 扫描失败：{e}")),
        }
    }

    if sel.workbuddy {
        match workbuddy::scan() {
            Ok((doc, msgs)) => {
                out.counts.workbuddy = count_accounts(&doc);
                out.workbuddy = doc;
                out.messages.extend(msgs);
            }
            Err(e) => out.messages.push(format!("WorkBuddy 扫描失败：{e}")),
        }
    }

    if sel.qoder {
        match qoder::scan() {
            Ok((doc, msgs)) => {
                out.counts.qoder = count_accounts(&doc);
                out.qoder = doc;
                out.messages.extend(msgs);
            }
            Err(e) => out.messages.push(format!("Qoder 扫描失败：{e}")),
        }
    }

    out
}
