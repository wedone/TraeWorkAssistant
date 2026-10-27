//! 会话粘性双模式（T2.4/F-31 v1.2）
//!
//! - **显式模式**：客户端请求体携带 `conversation_id` → 按 id 绑定账号与上游
//!   会话，TTL 30m 滚动续期（每次命中刷新 last_seen）；
//! - **指纹模式**：无 conversation_id 时退化为「前 3 条消息 SHA256 取 6 位 +
//!   60s 时间窗锁定」——仅当同一指纹在 60s 内再次出现才续用绑定（antigravity-tools
//!   实证设计；Buddy 上游代理流量缓存恒不命中 §5.5 #10，价值在会话一致性）。
//!
//! 线程安全：单 Mutex 内完成 resolve+bind（写锁 re-check，防 TOCTOU）。
//! 持久化（P6 SQLite 化）：`sticky_bindings` 表；多池共用时按键命名空间
//! 范围替换（save_ns + load_ns 过滤，见 NAMESPACES/owns_key），互不覆盖；
//! 旧根路径 JSON 文件仅作启动加载兼容。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use sha2::{Digest, Sha256};

/// 显式模式 TTL 默认值（滚动续期）；实际生效值可经
/// `set_explicit_ttl` 覆盖（F-76② api_pool.json.wb_sticky_ttl_secs 热应用）
pub const EXPLICIT_TTL_SECS: i64 = 30 * 60;
/// 指纹模式时间窗
pub const FINGERPRINT_WINDOW_SECS: i64 = 60;
/// Qoder 线命名空间前缀（审查 #2：sticky_bindings 表内 WB/Qoder 按前缀隔离；
/// 无前缀键 = WB 线，存量数据向后兼容）
pub const QODER_NS: &str = "q:";
/// Trae 线命名空间前缀（per-pool 会话粘性批次新增：与 q:/无前缀三线隔离）
pub const TRAE_NS: &str = "t:";
/// Buddy 国际版（buddy-ai）线命名空间前缀（§4.2：与 q:/t:/无前缀四线隔离，
/// Intl 池显式会话粘性）
pub const BUDDY_AI_NS: &str = "g:";
/// 落盘节流间隔：距上次成功保存不足该时长则跳过本次写盘
const SAVE_THROTTLE_MS: u64 = 1000;

/// 一条绑定：uid + 上游 conversation_id + 最后命中时间
#[derive(Debug, Clone)]
pub struct Binding {
    pub uid: String,
    pub conv_id: String,
    pub last_seen: i64,
    /// true = 显式 conversationId 模式（30m TTL）；false = 指纹模式（60s 窗）
    pub explicit: bool,
}

#[derive(Default)]
pub struct StickyStore {
    inner: Mutex<HashMap<String, Binding>>,
    /// 上次成功落盘时刻（节流，见 save）：Some 之前的 save 一律跳过写盘
    last_save: Mutex<Option<std::time::Instant>>,
    /// 显式模式 TTL 秒（F-76② 可配置，0 视为未设置 → 回退 EXPLICIT_TTL_SECS）
    explicit_ttl_secs: AtomicI64,
    /// 审查 #2：表内键命名空间前缀（"" = WB 线存量兼容；"q:" = Qoder 线）。
    /// 内存键与落库键格式一致（均含前缀），避免进出存取时二次变换
    ns: String,
}

/// 会话键：显式 conversationId 或消息指纹
#[derive(Debug, PartialEq, Eq)]
pub enum SessionKey {
    Explicit(String),
    Fingerprint(String),
}

impl SessionKey {
    /// 从请求体提取会话键：顶层 `conversation_id`（非空字符串）为显式；
    /// 否则取 messages 前 3 条（role + content 规范化拼接）的 SHA256 前 6 位
    pub fn from_body(body: &serde_json::Value) -> SessionKey {
        if let Some(cid) = body
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return SessionKey::explicit(cid);
        }
        SessionKey::Fingerprint(fingerprint_messages(body))
    }

    /// 显式键统一构造（port main 74b4d55）：限长 128 字符——cid 进 sticky 表做
    /// 会话匹配并随绑定留档，超长值放大存储体积；from_body / from_slice 两构造点
    /// 共用，确定性截断保证 WB/Qoder 两侧匹配一致性
    fn explicit(cid: &str) -> SessionKey {
        SessionKey::Explicit(cid.chars().take(128).collect())
    }

    /// 从原始请求字节窄提取会话键（issue #138 P0）：语义对齐 [from_body]
    /// （非空 conversation_id → 显式；否则前 3 条消息指纹），但不构建整包 DOM——
    /// 只实例化探测所需字段，未声明字段与 messages 第 4+ 条由 serde 流式跳过
    /// （大 body 下省去 3~6 倍 DOM 内存放大，且不再有 peek 闭包存活整条流）。
    /// 字段级畸形（conversation_id/role/block text 类型不匹配、非对象消息或
    /// 内容块、null content）宽松跳过，与 from_body 的 as_str 宽松口径一致；
    /// 仅结构性畸形（messages 非数组、顶层非对象）探测失败 → 回退仅提取
    /// conversation_id，再失败按「不可粘」（空指纹）处理
    pub fn from_slice(body: &[u8]) -> SessionKey {
        if let Ok(p) = serde_json::from_slice::<SessionKeyProbe>(body) {
            if let Some(cid) =
                p.conversation_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                return SessionKey::explicit(cid);
            }
            return SessionKey::Fingerprint(fingerprint_probe(&p.messages));
        }
        if let Ok(c) = serde_json::from_slice::<CidProbe>(body) {
            if let Some(cid) =
                c.conversation_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                return SessionKey::explicit(cid);
            }
        }
        SessionKey::Fingerprint(String::new())
    }

    /// 缓存键（"cid:{id}" / "fp:{指纹}"）——池粘性键复用同一前缀约定（§4.4）
    pub fn cache_key(&self) -> String {
        match self {
            SessionKey::Explicit(s) => format!("cid:{}", s),
            SessionKey::Fingerprint(s) => format!("fp:{}", s),
        }
    }

    fn is_explicit(&self) -> bool {
        matches!(self, SessionKey::Explicit(_))
    }
}

/// 前 3 条消息指纹：SHA256(role1|content1|role2|content2|...) 前 6 位 hex。
/// content 数组取 text 块拼接；空 messages → 空串（不可粘）。
pub fn fingerprint_messages(body: &serde_json::Value) -> String {
    let mut norm = String::new();
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs.iter().take(3) {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let content = match msg.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            norm.push_str(role);
            norm.push('|');
            norm.push_str(content.trim());
            norm.push('|');
        }
    }
    if norm.is_empty() {
        return String::new();
    }
    fingerprint_hex(&norm)
}

/// SHA256(norm) 前 6 位 hex（from_body / from_slice 共用尾段，指纹逐字节一致）
fn fingerprint_hex(norm: &str) -> String {
    let mut h = Sha256::new();
    h.update(norm.as_bytes());
    let digest = h.finalize();
    digest.iter().take(3).map(|b| format!("{:02x}", b)).collect()
}

// ==================== issue #138 P0：原始字节窄探测（消灭 peek DOM） ====================
//
// 流式/聚合路径原先整包 serde_json::Value 后取 SessionKey：大 body（长上下文
// 重发完整历史 + base64 图片）DOM 内存放大 3~6 倍，且 peek 在流式闭包中存活
// 整条流，是 2C2G 部署 RSS 高水位的最大单点（issue #138）。以下窄探测载荷
// 仅声明指纹/显式模式所需字段，其余 token 零分配跳过。

/// 顶层窄探测载荷（未声明字段由 serde 默认忽略）
#[derive(serde::Deserialize)]
struct SessionKeyProbe {
    #[serde(default, deserialize_with = "opt_string_lenient")]
    conversation_id: Option<String>,
    #[serde(default, deserialize_with = "first_three_messages")]
    messages: Vec<ProbeMessage>,
}

/// 兜底探测：主探测失败时仅提取 conversation_id（显式粘性尽力保留）
#[derive(serde::Deserialize)]
struct CidProbe {
    #[serde(default)]
    conversation_id: Option<String>,
}

/// 宽松字符串探测 newtype：字符串 → Some；null/数字/布尔/数组/对象 → None
/// （数组/对象整体流式消费保持 JSON 流合法）。对齐 from_body 链路 `as_str()`
/// 的宽松口径：字段类型不匹配按「无此字段」降级而非整体探测失败
struct OptStringLenient(Option<String>);

impl<'de> serde::Deserialize<'de> for OptStringLenient {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = OptStringLenient;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a string or any")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(OptStringLenient(Some(v.to_owned())))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(OptStringLenient(None))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(OptStringLenient(None))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(OptStringLenient(None))
            }
        }
        d.deserialize_any(V)
    }
}

/// deserialize_with 入口（SessionKeyProbe.conversation_id 字段复用）
fn opt_string_lenient<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    <OptStringLenient as serde::Deserialize>::deserialize(d).map(|v| v.0)
}

/// 单条消息窄探测：role 缺失/null/非串 → None（对齐 as_str().unwrap_or("")）；
/// 非对象消息（数组混入标量）按空消息降级，对齐 from_body 链路 `msg.get(..)`
/// 对标量返回 None 的宽松口径
#[derive(Default)]
struct ProbeMessage {
    role: Option<String>,
    content: ProbeContent,
}

impl<'de> serde::Deserialize<'de> for ProbeMessage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ProbeMessage;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a message object or any")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut m = ProbeMessage::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "role" => m.role = map.next_value::<OptStringLenient>()?.0,
                        "content" => m.content = map.next_value::<ProbeContent>()?,
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(m)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ProbeMessage::default())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ProbeMessage::default())
            }
        }
        d.deserialize_any(V)
    }
}

/// content 窄探测：字符串原样；数组逐块取 text；null/对象/标量 → 空
/// （对齐 fingerprint_messages：非 String/Array → 空串）
#[derive(Default)]
enum ProbeContent {
    #[default]
    Empty,
    Text(String),
    Blocks(Vec<ProbeBlock>),
}

impl<'de> serde::Deserialize<'de> for ProbeContent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ProbeContent;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("string, array, null or any")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ProbeContent::Text(v.to_owned()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut blocks = Vec::new();
                while let Some(b) = seq.next_element::<ProbeBlock>()? {
                    blocks.push(b);
                }
                Ok(ProbeContent::Blocks(blocks))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ProbeContent::Empty)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(ProbeContent::Empty)
            }
        }
        d.deserialize_any(V)
    }
}

/// content 数组块窄探测：仅取 text（未知字段自动忽略，图片 base64 零分配
/// 跳过）；text 非串 → None；非对象块（数组混入标量）按无 text 降级，
/// 对齐 from_body 链路 `b.get("text")` 对标量块返回 None 的宽松口径
#[derive(Default)]
struct ProbeBlock {
    text: Option<String>,
}

impl<'de> serde::Deserialize<'de> for ProbeBlock {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ProbeBlock;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a content block or any")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut b = ProbeBlock::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "text" => b.text = map.next_value::<OptStringLenient>()?.0,
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(b)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ProbeBlock::default())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ProbeBlock::default())
            }
        }
        d.deserialize_any(V)
    }
}

/// messages 窄提取：仅实例化前 3 条（指纹输入），第 4+ 条以 IgnoredAny 流式
/// 吞掉（零分配），对齐 fingerprint_messages 的 take(3)
fn first_three_messages<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<ProbeMessage>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<ProbeMessage>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a sequence of messages")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while out.len() < 3 {
                match seq.next_element::<ProbeMessage>()? {
                    Some(m) => out.push(m),
                    None => break,
                }
            }
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(out)
        }
    }
    d.deserialize_seq(V)
}

/// 探测载荷 → 指纹（norm 口径与 fingerprint_messages 逐字节一致：
/// role|trim(content)| 前 3 条；空消息序列 → 空串）
fn fingerprint_probe(msgs: &[ProbeMessage]) -> String {
    if msgs.is_empty() {
        return String::new();
    }
    let mut norm = String::new();
    for m in msgs {
        norm.push_str(m.role.as_deref().unwrap_or(""));
        norm.push('|');
        norm.push_str(probe_content_text(&m.content).trim());
        norm.push('|');
    }
    fingerprint_hex(&norm)
}

/// ProbeContent → content 文本（对齐 fingerprint_messages：String 原样、
/// 数组取 text 块拼接、其余空串）
fn probe_content_text(c: &ProbeContent) -> String {
    match c {
        ProbeContent::Text(s) => s.clone(),
        ProbeContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join(""),
        ProbeContent::Empty => String::new(),
    }
}

/// 多池共用 sticky_bindings 表的键命名空间登记（F-80-余 v2）。
/// 归属判定见 [owns_key]：ns 非空 = key 以该前缀开头；ns=""（WB/遗留键）=
/// key 不以任何已登记命名空间开头。**新增池命名空间时必须在此登记**，
/// load 过滤与 save 范围删除随之生效（审查 P1：两个 store 整表替换会互删
/// 对方运行期新增的绑定，范围化后各写各的键集）。
const NAMESPACES: &[&str] = &[QODER_NS, TRAE_NS, BUDDY_AI_NS];

/// 该 store 是否拥有持久化键 key（load_ns 过滤与 save 范围删除共用同一判定）
fn owns_key(ns: &str, key: &str) -> bool {
    if ns.is_empty() {
        !NAMESPACES.iter().any(|p| key.starts_with(p))
    } else {
        key.starts_with(ns)
    }
}

impl StickyStore {
    // 预留 API（持久化运维/前端扩展用；当前主要消费 resolve/bind/save/load）
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// 按命名空间前缀构造（审查 #2：Qoder 线传 QODER_NS；"" = WB 线）
    pub fn with_ns(ns: &str) -> Self {
        Self { ns: ns.to_string(), ..Self::default() }
    }

    /// 存储键 = 命名空间前缀 + 会话键（resolve/bind 统一入口；内存键与落库同格式）
    fn store_key(&self, ck: &str) -> String {
        format!("{}{}", self.ns, ck)
    }

    /// 设置显式模式 TTL 秒（F-76②，pool_set 热应用；≤0 回退默认值）
    pub fn set_explicit_ttl(&self, secs: i64) {
        self.explicit_ttl_secs.store(secs, Ordering::Relaxed);
    }

    /// 当前生效的显式模式 TTL 秒（未设置/非法回退 EXPLICIT_TTL_SECS）
    fn effective_explicit_ttl(&self) -> i64 {
        let v = self.explicit_ttl_secs.load(Ordering::Relaxed);
        if v > 0 {
            v
        } else {
            EXPLICIT_TTL_SECS
        }
    }

    /// 解析绑定（TTL 校验 + 显式模式滚动续期）。过期/不匹配即返回 None。
    pub fn resolve(&self, key: &SessionKey, now: i64) -> Option<Binding> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ttl = if key.is_explicit() {
            self.effective_explicit_ttl()
        } else {
            FINGERPRINT_WINDOW_SECS
        };
        let ck = self.store_key(&key.cache_key());
        let b = map.get(&ck)?;
        if !b.explicit == key.is_explicit() {
            return None; // 键类型变化（少见）：按无绑定处理
        }
        if now - b.last_seen > ttl {
            map.remove(&ck);
            return None;
        }
        let mut b = b.clone();
        b.last_seen = now; // 滚动续期
        map.insert(ck, b.clone());
        Some(b)
    }

    /// 写入/覆盖绑定（Mutex 内 re-check：同键已有更晚绑定且 uid 不同时，
    /// 以更晚者为准——并发首请求只留一个胜者）
    pub fn bind(&self, key: &SessionKey, uid: &str, conv_id: &str, now: i64) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let ck = self.store_key(&key.cache_key());
        if let Some(existing) = map.get(&ck) {
            if existing.uid != uid && existing.last_seen > now - 5 {
                return; // 5s 内他账号刚绑定：让胜者保持
            }
        }
        map.insert(
            ck,
            Binding {
                uid: uid.to_string(),
                conv_id: conv_id.to_string(),
                last_seen: now,
                explicit: key.is_explicit(),
            },
        );
    }

    /// 按账号解绑全部粘性绑定（issue #67，移植 main@36d628f）：账号进入硬冷却/禁用时调用，
    /// 清理其绑定避免其他会话在 TTL 窗口内反复解析到该账号（resolve 返回的
    /// Binding 还有调用方 pick_by_uid 的 selectable 校验兜底，此处为卫生性
    /// 清理，让重绑定立即可见）。有变更时重置 save 节流，下一次 save 立即落盘。
    pub fn unbind_uid(&self, uid: &str) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = map.len();
        map.retain(|_, b| b.uid != uid);
        if map.len() != before {
            *self.last_save.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// 清理全部过期绑定，返回清理条数（save 落库前调用，控制绑定表无界增长）。
    /// 显式模式 TTL 读运行时配置值（per-pool 可配后固定 1800 会在用户调大 TTL
    /// 时提前删除仍有效的绑定，F-76② per-pool 版修正）
    pub fn evict_expired(&self, now: i64) -> usize {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = map.len();
        let explicit_ttl = self.effective_explicit_ttl();
        map.retain(|_, b| {
            let ttl = if b.explicit { explicit_ttl } else { FINGERPRINT_WINDOW_SECS };
            now - b.last_seen <= ttl
        });
        before - map.len()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ---------- 持久化 ----------

    /// 落盘（原子写 + 节流，写入 data/ 子目录）。
    ///
    /// 节流取舍：bind 后高频全量重写浪费 IO——距上次成功保存不足
    /// SAVE_THROTTLE_MS 时跳过本次落盘。内存态照常更新（resolve 不受影响），
    /// 跳过的落盘由下一次间隔超过阈值的请求兜底；代价是进程崩溃时可能丢失
    /// 最近约 1s 的绑定——粘性绑定本就有 TTL，丢失仅导致个别会话重新取号，
    /// 可接受。
    pub fn save(&self, data_dir: &std::path::Path) {
        {
            let last = self.last_save.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(t) = *last {
                if t.elapsed() < std::time::Duration::from_millis(SAVE_THROTTLE_MS) {
                    return;
                }
            }
        }
        // P6 流水化：落库前清理过期绑定（原实现过期项滞留内存/落盘缓慢增长）
        let now_secs = chrono::Utc::now().timestamp();
        self.evict_expired(now_secs);
        let file = {
            let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            serde_json::json!({
                "version": 1,
                "saved_at": now_ts(),
                "bindings": map.iter().map(|(k, b)| serde_json::json!({
                    "key": k, "uid": b.uid, "conv_id": b.conv_id,
                    "last_seen": b.last_seen, "explicit": b.explicit,
                })).collect::<Vec<_>>(),
            })
        };
        if crate::store::docs::sticky_bindings_save_ns(
            &crate::store::db(data_dir),
            &file,
            &self.ns,
            &NAMESPACES.iter().copied().filter(|p| *p != self.ns).collect::<Vec<_>>(),
        )
        .is_ok() {
            let mut last = self.last_save.lock().unwrap_or_else(|e| e.into_inner());
            *last = Some(std::time::Instant::now());
        }
    }

    /// 启动时加载（过期的条目在 resolve 时自然失效）。WB 线（无前缀键）。
    /// SQLite 化（P6）：wb_sticky_sessions → sticky_bindings 表；
    /// 旧根路径兼容由启动迁移器完成。
    pub fn load(data_dir: &std::path::Path) -> Self {
        Self::load_ns(data_dir, "")
    }

    /// 同 load，附键命名空间（F-80-余 v2：Qoder 池传 "q:"，与 WB 绑定同表隔离）。
    /// 仅加载归属本命名空间的键（owns_key）——多池并存时各自内存 map 只含
    /// 自己的键，配合 save_ns 范围替换互不覆盖（审查 P1 回归修复）
    pub fn load_ns(data_dir: &std::path::Path, ns: &str) -> Self {
        let file: serde_json::Value =
            crate::store::docs::sticky_bindings_load(&crate::store::db(data_dir));
        let mut map = HashMap::new();
        if let Some(list) = file.get("bindings").and_then(|b| b.as_array()) {
            for item in list {
                let key = match item.get("key").and_then(|v| v.as_str()) {
                    Some(k) if !k.is_empty() => k.to_string(),
                    _ => continue,
                };
                let uid = item.get("uid").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let conv_id = item.get("conv_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let last_seen = item.get("last_seen").and_then(|v| v.as_i64()).unwrap_or(0);
                let explicit = item.get("explicit").and_then(|v| v.as_bool()).unwrap_or(false);
                if uid.is_empty() || conv_id.is_empty() {
                    continue;
                }
                map.insert(key, Binding { uid, conv_id, last_seen, explicit });
            }
        }
        // 命名空间过滤：只保留归属本 store 的键（多池共用表互不串载）
        map.retain(|k, _| owns_key(ns, k));
        Self {
            inner: Mutex::new(map),
            last_save: Mutex::new(None),
            explicit_ttl_secs: std::sync::atomic::AtomicI64::new(0),
            ns: ns.to_string(),
        }
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body_with(conv: Option<&str>, msgs: serde_json::Value) -> serde_json::Value {
        let mut b = json!({ "messages": msgs });
        if let Some(c) = conv {
            b["conversation_id"] = json!(c);
        }
        b
    }

    #[test]
    fn explicit_mode_binds_and_renews_with_30m_ttl() {
        let store = StickyStore::new();
        let key = SessionKey::from_body(&body_with(Some("conv-1"), json!([{"role":"user","content":"hi"}])));
        assert!(matches!(key, SessionKey::Explicit(_)));
        assert!(store.resolve(&key, 1000).is_none());
        store.bind(&key, "uid_a", "up-conv-1", 1000);
        let b = store.resolve(&key, 1000 + EXPLICIT_TTL_SECS - 1).unwrap();
        assert_eq!(b.uid, "uid_a");
        assert_eq!(b.conv_id, "up-conv-1");
        // 滚动续期：resolve 刷新 last_seen → 命中即续命（TTL 30m 滚动）
        assert!(store.resolve(&key, 1000 + EXPLICIT_TTL_SECS + 500).is_some());
        // 过期判定：重新绑定后不再访问，超过 TTL → 失效
        store.bind(&key, "uid_a", "up-conv-1", 1000);
        assert!(store.resolve(&key, 1000 + EXPLICIT_TTL_SECS + 1).is_none());
    }

    #[test]
    fn fingerprint_mode_only_within_60s_window() {
        let store = StickyStore::new();
        let body = body_with(None, json!([
            {"role":"system","content":"you are helpful"},
            {"role":"user","content":"hello"},
            {"role":"assistant","content":"hi"},
            {"role":"user","content":"continue"},
        ]));
        let key = SessionKey::from_body(&body);
        assert!(matches!(key, SessionKey::Fingerprint(_)));
        assert_eq!(fingerprint_messages(&body).len(), 6);
        store.bind(&key, "uid_b", "up-conv-2", 5000);
        assert!(store.resolve(&key, 5000 + FINGERPRINT_WINDOW_SECS - 1).is_some());
        // 超过 60s 窗 → 失效（这正是指纹模式的语义：仅短窗内粘住）
        // 注意：上面的 resolve 已滚动续期，重新绑定回到 t=5000 再验证过期
        store.bind(&key, "uid_b", "up-conv-2", 5000);
        assert!(store.resolve(&key, 5000 + FINGERPRINT_WINDOW_SECS + 1).is_none());
    }

    #[test]
    fn fingerprint_is_content_sensitive() {
        let a = fingerprint_messages(&body_with(None, json!([{"role":"user","content":"问题A"}])));
        let b = fingerprint_messages(&body_with(None, json!([{"role":"user","content":"问题B"}])));
        let a2 = fingerprint_messages(&body_with(None, json!([
            {"role":"user","content":[{"type":"text","text":"问题A"}]}
        ])));
        assert_ne!(a, b);
        assert_eq!(a, a2); // string 与 [{type:text}] 等价
        // 空 messages → 空指纹
        assert_eq!(fingerprint_messages(&json!({"messages":[]})), "");
    }

    /// issue #138 P0：from_slice 窄探测与 from_body 全 DOM 口径逐字节一致
    /// （显式/指纹/空体/畸形类型/仅前 3 条参与），保证替换无行为漂移
    #[test]
    fn from_slice_matches_from_body_semantics() {
        // 显式 conversation_id（含 trim/空白拒绝）
        let explicit = json!({"conversation_id":" conv-1 ", "messages":[]});
        assert_eq!(
            SessionKey::from_slice(&serde_json::to_vec(&explicit).unwrap()),
            SessionKey::from_body(&explicit)
        );
        // 显式：超长 cid 按 chars 限长 128（port main 74b4d55），两构造点一致
        let long = "长".repeat(200);
        let long_body = json!({"conversation_id": long, "messages":[]});
        let long_raw = serde_json::to_vec(&long_body).unwrap();
        match (SessionKey::from_body(&long_body), SessionKey::from_slice(&long_raw)) {
            (SessionKey::Explicit(a), SessionKey::Explicit(b)) => {
                assert_eq!(a, b);
                assert_eq!(a.chars().count(), 128);
            }
            _ => panic!("显式键两侧应一致"),
        }
        // 指纹模式：string content 与 blocks content 等价、第 4+ 条不参与
        let msgs = json!([
            {"role":"user","content":[{"type":"text","text":"问题A"}]},
            {"role":"assistant","content":"回答"},
            {"role":"user","content":"续"},
            {"role":"user","content":"应被忽略的第四条"},
            {"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]},
        ]);
        let fp_body = json!({"messages": msgs});
        assert_eq!(
            SessionKey::from_slice(&serde_json::to_vec(&fp_body).unwrap()),
            SessionKey::from_body(&fp_body)
        );
        // 畸形 messages（对象而非数组）：两路同为兜底口径（from_body 走
        // fingerprint_messages 的非数组 → 空；from_slice 主探测失败 → CidProbe
        // 仍取不到 → 空指纹）
        let bad = json!({"messages":{"role":"user"}});
        assert_eq!(
            SessionKey::from_slice(&serde_json::to_vec(&bad).unwrap()),
            SessionKey::from_body(&bad)
        );
        // 空 body（非 JSON）：空指纹（对齐 unwrap_or(json!({})) 口径）
        assert_eq!(SessionKey::from_slice(b"not-json"), SessionKey::Fingerprint(String::new()));
        // 字段级畸形宽松跳过（修复审查 #1）：非串 conversation_id/role、content
        // 数组混入标量块、对象 content、null content —— from_slice 与 from_body
        // 宽松口径逐字节一致（同产指纹，而非窄探测整体失败）
        let malformed = json!({
            "conversation_id": 123,
            "messages": [
                {"role": 1, "content": "问题A"},
                {"role": "user", "content": ["plain", {"type": "text", "text": "B"}]},
                {"role": "user", "content": {"type": "text", "text": "对象"}},
                {"role": "user", "content": null},
            ],
        });
        assert_eq!(
            SessionKey::from_slice(&serde_json::to_vec(&malformed).unwrap()),
            SessionKey::from_body(&malformed)
        );
        // 且确为指纹模式且非空（防两路同错为空串的平凡通过）
        assert_eq!(
            SessionKey::from_body(&malformed),
            SessionKey::Fingerprint(fingerprint_hex("|问题A|user|B|user||"))
        );
        // 结构性畸形（messages 非数组）+ 合法 conversation_id：主探测失败 →
        // CidProbe 兜底，两路均显式粘性（explicit 尽力保留）
        let structural = json!({"conversation_id":" conv-9 ","messages":5});
        assert_eq!(
            SessionKey::from_slice(&serde_json::to_vec(&structural).unwrap()),
            SessionKey::from_body(&structural)
        );
        assert_eq!(
            SessionKey::from_body(&structural),
            SessionKey::Explicit("conv-9".to_string())
        );
    }

    #[test]
    fn bind_recheck_keeps_recent_winner() {
        let store = StickyStore::new();
        let key = SessionKey::from_body(&body_with(Some("c"), json!([{"role":"user","content":"x"}])));
        store.bind(&key, "uid_a", "cv1", 1000);
        // 并发双请求各自取号后先后绑定（TOCTOU）：5s 内先绑定者保持胜者
        store.bind(&key, "uid_b", "cv2", 1003);
        assert_eq!(store.resolve(&key, 1004).unwrap().uid, "uid_a");
        // 5s 后再绑定（账号已失效重取等场景）→ 允许覆盖
        store.bind(&key, "uid_b", "cv2", 1010);
        assert_eq!(store.resolve(&key, 1011).unwrap().uid, "uid_b");
    }

    #[test]
    fn evict_expired_counts() {
        let store = StickyStore::new();
        let k1 = SessionKey::from_body(&body_with(Some("c1"), json!([{"role":"user","content":"a"}])));
        let k2 = SessionKey::from_body(&body_with(Some("c2"), json!([{"role":"user","content":"b"}])));
        store.bind(&k1, "u1", "v1", 1000);
        store.bind(&k2, "u2", "v2", 2000);
        assert_eq!(store.evict_expired(1000 + EXPLICIT_TTL_SECS + 10), 1);
        assert_eq!(store.len(), 1);
    }

    /// 落盘写入 data/ 子目录；1s 节流窗口内的重复 save 跳过写盘（内存态照常更新）
    #[test]
    fn save_writes_data_subdir_and_throttles_within_one_second() {
        let dir = std::env::temp_dir().join(format!(
            "twa_sticky_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = StickyStore::new();
        let key = SessionKey::from_body(&body_with(Some("c"), json!([{"role":"user","content":"x"}])));
        // P6：save 落库前会按真实时钟清理过期绑定 → 测试绑定也用真实时间
        let now = chrono::Utc::now().timestamp();
        store.bind(&key, "u1", "v1", now);
        store.save(&dir);
        // SQLite 化（P6）：落盘 = sticky_bindings 表
        let snapshot1 = crate::store::docs::sticky_bindings_load(&crate::store::db(&dir)).to_string();
        assert!(snapshot1.contains("v1"), "落盘应写入 sticky_bindings 表");
        // 1s 内再次 bind + save：落盘被节流跳过，内容不变
        store.bind(&key, "u2", "v2", now + 6);
        store.save(&dir);
        let snapshot2 = crate::store::docs::sticky_bindings_load(&crate::store::db(&dir)).to_string();
        assert_eq!(snapshot1, snapshot2, "距上次成功保存 <1000ms 应跳过落库");
        // 内存态已更新（resolve 读到新绑定）
        assert_eq!(store.resolve(&key, now + 7).unwrap().uid, "u2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 审查 #2：WB/Qoder 共表命名空间隔离——load 互不串载、save 合并保留他线绑定
    #[test]
    fn namespace_isolation_between_wb_and_qoder() {
        let dir = std::env::temp_dir().join(format!(
            "twa_sticky_ns_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let now = chrono::Utc::now().timestamp();
        let wb = StickyStore::new(); // ns = ""
        let q = StickyStore::with_ns(QODER_NS);
        let key = SessionKey::Explicit("shared-conv".into()); // 同一会话键，两线各自绑定
        wb.bind(&key, "wb_uid", "wb_conv", now);
        wb.save(&dir);
        // qoder 线 save 不得删除 wb 线绑定（合并保留）
        q.bind(&key, "q_uid", "q_conv", now);
        q.save(&dir);
        let wb2 = StickyStore::load(&dir);
        let q2 = StickyStore::load_ns(&dir, QODER_NS);
        assert_eq!(wb2.resolve(&key, now + 1).unwrap().uid, "wb_uid");
        assert_eq!(q2.resolve(&key, now + 1).unwrap().uid, "q_uid");
        // 反向：wb 线再 save，qoder 绑定同样保留
        wb.save(&dir);
        let q3 = StickyStore::load_ns(&dir, QODER_NS);
        assert_eq!(q3.resolve(&key, now + 1).unwrap().uid, "q_uid");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 读取兼容：data/ 新路径不存在时回退旧根路径（存量用户数据），save 迁移写新路径
    #[test]
    fn load_falls_back_to_legacy_root_path() {
        let dir = std::env::temp_dir().join(format!(
            "twa_sticky_legacy_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        // SQLite 化（P6）：种子 = sticky_bindings 表
        crate::store::docs::sticky_bindings_save(
            &crate::store::db(&dir),
            &json!({"version": 1, "bindings": [
                {"key": "cid:legacy", "uid": "u9", "conv_id": "c9", "last_seen": 500, "explicit": true}
            ]}),
        )
        .unwrap();
        let store = StickyStore::load(&dir);
        let key = SessionKey::Explicit("legacy".into());
        // 种子 last_seen=500 为历史值（会过期失效），重新绑定到真实时钟后再验证
        let now = chrono::Utc::now().timestamp();
        store.bind(&key, "u9", "c9", now);
        assert_eq!(store.resolve(&key, now + 1).unwrap().conv_id, "c9");
        // save 落 sticky_bindings 表
        store.save(&dir);
        assert!(crate::store::db(&dir)
            .with_conn(|c| {
                let n: i64 = c.query_row("SELECT COUNT(*) FROM sticky_bindings", [], |r| r.get(0))?;
                Ok(n > 0)
            })
            .unwrap_or(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
