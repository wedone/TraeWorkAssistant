//! Codex Responses API 投影转换器（T4.1/F-40）
//!
//! `/v1/responses`（Codex CLI `wire_api="responses"` 直配 base_url）：
//! - 请求投影：Responses 格式 → OpenAI chat completions 内部格式，
//!   复用 wb_route 既有取号/重试/粘性/脱敏管线（三协议一份）；
//! - 非流式响应投影：内部 OpenAI completion → Responses 对象；
//! - 流式响应投影在 wb_sse.rs `Protocol::Responses` 分支实现。
//!
//! 宽容解析红线：字段缺失不报错，只按规范降级（input 缺失 → 空消息列表）。

use super::wb_sse::cached_tokens_of;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Responses 请求 → OpenAI chat completions 内部格式
///
/// 映射：instructions → system；input（string | items[]）→ messages；
/// function_call/function_call_output → tool_calls/tool 消息；
/// tools 平铺（Responses 顶层 name/parameters → chat function 包裹）；
/// max_output_tokens → max_tokens；reasoning.effort → reasoning_effort。
pub fn responses_to_chat(body: &Value) -> Result<Value, String> {
    if !body.is_object() {
        return Err("request body must be a JSON object".to_string());
    }
    let mut out = Map::new();

    // 消息列表
    let mut messages: Vec<Value> = Vec::new();
    if let Some(inst) = body.get("instructions").and_then(|v| v.as_str()) {
        if !inst.is_empty() {
            messages.push(json!({"role": "system", "content": inst}));
        }
    }
    match body.get("input") {
        None | Some(Value::Null) => {
            messages.push(json!({"role": "user", "content": ""}));
        }
        Some(Value::String(s)) => {
            messages.push(json!({"role": "user", "content": s}));
        }
        Some(Value::Array(items)) => {
            let converted = convert_input_items(items);
            if converted.is_empty() {
                return Err("input: at least one usable item required".to_string());
            }
            messages.extend(converted);
        }
        Some(_) => return Err("input: expected string or array".to_string()),
    }
    out.insert("messages".to_string(), json!(messages));

    // 采样参数直传
    for (src, dst) in [("temperature", "temperature"), ("top_p", "top_p")] {
        if let Some(v) = body.get(src) {
            if v.is_number() {
                out.insert(dst.to_string(), v.clone());
            }
        }
    }
    if let Some(v) = body.get("max_output_tokens").and_then(|v| v.as_u64()) {
        out.insert("max_tokens".to_string(), json!(v));
    }
    if let Some(v) = body.get("parallel_tool_calls") {
        if v.is_boolean() {
            out.insert("parallel_tool_calls".to_string(), v.clone());
        }
    }
    // reasoning.effort → reasoning_effort（WB 目录 effort 降级链复用）
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(|v| v.as_str()) {
        if !effort.is_empty() {
            out.insert("reasoning_effort".to_string(), json!(effort));
        }
    }

    // tools：function 直投影；namespace（Codex 注入的 multi_agent_v1 / mcp__* 等，
    // issue #93）展平为内层 function，编码名 `<ns>__<inner>`（上游 chat 协议无
    // namespace 概念），响应回程由 split_namespaced 还原为 {name, namespace}
    let mut choice_inner_lookup: HashMap<String, String> = HashMap::new();
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        // 编码名碰撞去重（审查修复 #4）：与顶层 function 或先展平项重名的编码名
        // 跳过（保留先声明者），避免向上游重复声明同名工具
        let mut seen: HashSet<String> = HashSet::new();
        let mut converted: Vec<Value> = tools
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"))
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                seen.insert(name.clone());
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.get("description").cloned().unwrap_or(json!("")),
                        "parameters": t.get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}})),
                        "strict": t.get("strict").cloned().unwrap_or(json!(false)),
                    },
                }))
            })
            .collect();
        for t in tools
            .iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("namespace"))
        {
            let Some(ns) = t
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(inner) = t.get("tools").and_then(|v| v.as_array()) else {
                continue;
            };
            // 组级 description 保留（审查修复 #5）：内层为空回退组级，
            // 均非空则组级在前拼接
            let ns_desc = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            for it in inner.iter() {
                if it.get("type").and_then(|v| v.as_str()) != Some("function") {
                    continue;
                }
                let Some(iname) = it
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let encoded = format!("{}__{}", ns, iname);
                if !seen.insert(encoded.clone()) {
                    continue;
                }
                // 裸名 → 编码名映射（审查修复 #3）：tool_choice 裸名命中组内
                // 工具时拼回编码名，与 tools 声明一致；置于 seen 去重之后，
                // 与 encoded_tool_names 同口径——仅映射实际下发的工具，
                // 防止映射指向被去重跳过、未向上游声明的名字（审查 #2）
                choice_inner_lookup
                    .entry(iname.to_string())
                    .or_insert_with(|| encoded.clone());
                let inner_desc = it.get("description").cloned().unwrap_or(json!(""));
                let desc = if !ns_desc.is_empty() {
                    match inner_desc.as_str() {
                        Some(id) if id.trim().is_empty() => json!(ns_desc),
                        Some(id) => json!(format!("{}\n\n{}", ns_desc, id)),
                        None => inner_desc,
                    }
                } else {
                    inner_desc
                };
                converted.push(json!({
                    "type": "function",
                    "function": {
                        "name": encoded,
                        "description": desc,
                        "parameters": it.get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}})),
                        "strict": it.get("strict").cloned().unwrap_or(json!(false)),
                    },
                }));
            }
        }
        if !converted.is_empty() {
            out.insert("tools".to_string(), json!(converted));
        }
    }
    if let Some(tc) = body.get("tool_choice") {
        match tc {
            Value::String(s) => {
                out.insert("tool_choice".to_string(), json!(s));
            }
            Value::Object(o) if o.get("type").and_then(|v| v.as_str()) == Some("function") => {
                if let Some(name) = o.get("name").and_then(|v| v.as_str()) {
                    // 审查修复 #3：tool_choice 裸名命中 namespace 组内工具 →
                    // 拼回编码名；已编码名/顶层名无命中，原样直传
                    let resolved = choice_inner_lookup
                        .get(name)
                        .map(|s| s.as_str())
                        .unwrap_or(name);
                    out.insert(
                        "tool_choice".to_string(),
                        json!({"type": "function", "function": {"name": resolved}}),
                    );
                }
            }
            _ => {}
        }
    }

    // tool_choice string 化（F-30）与 effort 改写由 wb_payload::prepare_wb_chat_body 统一处理
    Ok(Value::Object(out))
}

/// 编码名 `<ns>__<inner>` 还原为 (name, namespace)；无分隔符则不拆。
/// 按**最后一个** `__` 切分：namespace 自身可含 `__`（如 `mcp__node_repl` 的工具
/// 编码为 `mcp__node_repl__js`，应拆出 ns=`mcp__node_repl`、name=`js`）。
/// 供流式响应（wb_sse）复用（issue #93）。
pub fn split_namespaced(name: &str) -> (String, Option<String>) {
    match name.rfind("__") {
        Some(i) if i > 0 && i + 2 < name.len() => {
            (name[i + 2..].to_string(), Some(name[..i].to_string()))
        }
        _ => (name.to_string(), None),
    }
}

/// 判定转发 body 的 tools 中是否存在含 `__` 分隔符的 function 名。
/// 请求侧编码名集合（回程拆分闸门，F4 移植，审查 2026-10-13）：
/// `responses_to_chat` 的 namespace 组展平时实际生成的全部 `<ns>__<inner>`
/// 编码名（与展平同一遍历口径：顶层 function 同名碰撞去重后仍在集合内的
/// 才算有效编码名）。/v1/responses 入口在投影前收集并随执行链下发，
/// 回程投影（completion_to_responses / wb_sse 流式）仅对集合内命中者做
/// split_namespaced 还原——客户端原生扁平工具名可能天然含 `__`（MCP 惯例
/// `mcp__server__tool`），无差别拆分会把未编码扁平名误拆成
/// {name, namespace} 双字段，客户端工具分发匹配失败
pub fn encoded_tool_names(body: &Value) -> HashSet<String> {
    let mut encoded = HashSet::new();
    let Some(tools) = body.get("tools").and_then(|v| v.as_array()) else {
        return encoded;
    };
    // 与 responses_to_chat 同口径的碰撞去重：顶层 function 名先占位，
    // 与之或与先前展平项重名的编码名被跳过（保留先声明者）——
    // 被跳过的编码名未向上游声明，上游不可能回呼，无需登记
    let mut seen: HashSet<String> = tools
        .iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("function"))
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    for t in tools
        .iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("namespace"))
    {
        let Some(ns) = t
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(inner) = t.get("tools").and_then(|v| v.as_array()) else {
            continue;
        };
        for it in inner.iter() {
            if it.get("type").and_then(|v| v.as_str()) != Some("function") {
                continue;
            }
            let Some(iname) = it
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let name = format!("{}__{}", ns, iname);
            if seen.insert(name.clone()) {
                encoded.insert(name);
            }
        }
    }
    encoded
}

/// input items → chat messages
///
/// 上游对「assistant.tool_calls ↔ role:"tool" 结果」做严格逐条配对校验（issue #69）：
/// 每个 tool_call 的结果必须连续紧跟、逐条对应，中间插入任何其它角色消息即
/// 400 `code:11148 tool calls and tool results do not match`。Codex 在并行工具
/// 调用的 `function_call_output` 之间常夹 `role:"developer"` 提示（典型
/// `<image_resize_notice>`），旧实现 1:1 直投影 + developer 降级成 user 正好打断
/// 配对（多图并行必现，单图不触发）。本实现只调整顺序与合并，不改 tool 内容语义：
/// - 连续 `function_call` 合并进同一条 assistant 消息（tool_calls 累加）；
/// - 配对窗口内（有调用未回填结果）到达的非 tool 消息推迟到该批结果之后按序补回；
///   窗口内的 assistant 文本并入 tool_calls 载体（属于同一轮 assistant 输出）
/// - 工具结果先入缓冲，窗口关闭/收尾时按载体 tool_calls 顺序统一回填（与到达序
///   无关，消除严格按位置校验上游的残余风险），无结果的调用补占位；
/// - 回填结果必须紧跟 assistant，其后才补被推迟的消息（顺序不可换，否则占位结果
///   又被推迟消息隔开、复现 11148）；孤儿/重复 `function_call_output`（无对应
///   未回填调用）直接丢弃
fn convert_input_items(items: &[Value]) -> Vec<Value> {
    let mut messages: Vec<Value> = Vec::new();
    // 已发出但尚未收到结果的 tool_call_id：非空即处于「工具结果必须连续」窗口。
    // 不变量：pending 非空 ⟺ open_assistant 为 Some
    let mut pending_calls: Vec<String> = Vec::new();
    // 窗口内已收到的工具结果 (call_id, text)，窗口关闭/收尾时按 tool_calls 顺序回填
    let mut results: Vec<(String, String)> = Vec::new();
    // 配对窗口内被推迟的非 tool 消息（role 已降级：developer→user，system 保留）；
    // content 为 Value 以携带图片数组形态（issue #154）
    let mut deferred: Vec<(String, Value)> = Vec::new();
    // 缺 call_id 时的兜底自增序号
    let mut seq: usize = 0;
    // 当前收集 tool_calls 的 assistant 消息下标
    let mut open_assistant: Option<usize> = None;

    for item in items {
        let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("message");
        match itype {
            "function_call" => {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        seq += 1;
                        format!("call_{}", seq)
                    });
                let raw_name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                // Codex 回传历史时 function_call 带 {name, namespace} 双字段，
                // 拼回请求侧 tools 声明用的编码名 `<ns>__<name>`（issue #93）；
                // 审查修复 #2：name 已是编码名（以 `<ns>__` 开头，双重前缀）时
                // 原样保留，不再拼出 `<ns>__<ns>__<name>`
                let name = match item.get("namespace").and_then(|v| v.as_str()) {
                    Some(ns) if !ns.is_empty()
                        && !raw_name.is_empty()
                        && !raw_name.starts_with(&format!("{}__", ns)) =>
                    {
                        std::borrow::Cow::Owned(format!("{}__{}", ns, raw_name))
                    }
                    _ => std::borrow::Cow::Borrowed(raw_name),
                };
                let args = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                // 连续多个 function_call 合并进同一条 assistant，避免拆成多条
                let idx = match open_assistant {
                    Some(i) if i < messages.len() => i,
                    _ => {
                        messages.push(json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [],
                        }));
                        messages.len() - 1
                    }
                };
                messages[idx]["tool_calls"]
                    .as_array_mut()
                    .expect("carrier always carries a tool_calls array")
                    .push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": {"name": name, "arguments": args},
                    }));
                open_assistant = Some(idx);
                pending_calls.push(call_id);
            }
            "function_call_output" => {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty());
                let Some(call_id) = call_id else {
                    continue; // 无 call_id 的结果无法配对，跳过
                };
                // 孤儿（无对应调用）或重复结果：丢弃，保持 tool 条数 == tool_calls 条数
                let Some(pos) = pending_calls.iter().position(|c| c == call_id) else {
                    continue;
                };
                let call_id = call_id.to_string();
                let output = tool_output_text(item.get("output"));
                // 结果先入缓冲，窗口关闭时按 tool_calls 顺序统一回填
                pending_calls.remove(pos);
                results.push((call_id, output));
                if pending_calls.is_empty() {
                    // 窗口关闭：先回填结果（紧跟 assistant），再补回被推迟的消息
                    emit_tool_results(&mut messages, open_assistant, &mut results);
                    flush_deferred(&mut messages, &mut deferred);
                    open_assistant = None;
                }
            }
            "message" => {
                let role = item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("user")
                    .to_string();
                // issue #154：message content 数组中的 input_image 不再静默丢弃，
                // 投影成 OpenAI image_url；无图时维持纯字符串（下游文本消费方零影响）
                let content = message_content(item.get("content"));
                let text = content
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_default();
                if pending_calls.is_empty() {
                    let role = if role == "assistant" || role == "system" {
                        role
                    } else {
                        "user".to_string()
                    };
                    messages.push(json!({"role": role, "content": content}));
                } else if role == "assistant" {
                    // 窗口内的 assistant 文本并入 tool_calls 载体（同一轮输出）
                    if !text.is_empty() {
                        if let Some(i) = open_assistant {
                            append_assistant_text(&mut messages[i], &text);
                        }
                    }
                } else if !text.is_empty() || content.as_array().is_some_and(|a| !a.is_empty()) {
                    // user/developer/system：推迟到本批工具结果之后补回，保住配对连续性
                    let r = if role == "system" { "system" } else { "user" };
                    deferred.push((r.to_string(), content));
                }
            }
            // reasoning / local_shell_call / 其他：跳过（reasoning 不回投，上游自行产生）
            _ => {}
        }
    }

    // 收尾顺序不可换：先按 tool_calls 顺序回填结果/占位（必须紧跟 assistant.tool_calls），
    // 再补被推迟的非工具消息（属于批次之后）
    emit_tool_results(&mut messages, open_assistant, &mut results);
    flush_deferred(&mut messages, &mut deferred);
    messages
}

/// 配对窗口结束后，把被推迟的非工具消息按原序补回（developer 已降级为 user）
fn flush_deferred(messages: &mut Vec<Value>, deferred: &mut Vec<(String, Value)>) {
    for (role, content) in deferred.drain(..) {
        messages.push(json!({"role": role, "content": content}));
    }
}

/// 窗口关闭/收尾时回填工具结果：按载体 tool_calls 顺序输出（与到达序无关），
/// 未收到结果的调用补占位——保证 tool 消息连续且逐条对齐 tool_calls
fn emit_tool_results(
    messages: &mut Vec<Value>,
    open_assistant: Option<usize>,
    results: &mut Vec<(String, String)>,
) {
    let Some(i) = open_assistant else { return };
    let order: Vec<String> = messages[i]
        .get("tool_calls")
        .and_then(|t| t.as_array())
        .map(|tcs| {
            tcs.iter()
                .filter_map(|tc| tc.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for call_id in order {
        let text = results
            .iter()
            .find(|(c, _)| c == &call_id)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| "(tool did not return a result)".to_string());
        messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
    }
    results.clear();
}

/// 窗口内 assistant 文本并入 tool_calls 载体：content 为字符串则续写，否则直接采用
fn append_assistant_text(msg: &mut Value, text: &str) {
    match msg.get_mut("content") {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(text);
        }
        _ => msg["content"] = json!(text),
    }
}

/// tool 输出文本化：字符串直取；数组/object 仅拼接 text 部分，input_image 等
/// 非文本内容折叠为占位符——旧实现 `to_string()` 会把含 data URL（可达数 MB）的
/// 内容整段 JSON 序列化塞进 content，撑爆上游载荷；上游 tool content 为纯字符串，
/// 图片本就无法作为多模态透传
fn tool_output_text(output: Option<&Value>) -> String {
    const NON_TEXT: &str = "[non-text content omitted]";
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => {
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        Some(t.to_string())
                    } else if p.get("type").and_then(|t| t.as_str()).is_some() {
                        Some(NON_TEXT.to_string())
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        Some(Value::Object(o)) => o
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| NON_TEXT.to_string()),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// message content → OpenAI content（issue #154）：无图时返回纯字符串（与旧
/// content_text 行为一致），含 input_image 时返回 [{type:text},{type:image_url}]
/// 数组。reasoning 之外的 content parts（input_text/output_text/refusal）拼接为
/// 文本段，input_image 按序插入
fn message_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(s)) => json!(s),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            let mut images: Vec<Value> = Vec::new();
            for p in parts {
                if p.get("type").and_then(|t| t.as_str()) == Some("input_image") {
                    if let Some(img) = input_image_to_openai(p) {
                        images.push(img);
                    }
                    continue;
                }
                if let Some(t) = p
                    .get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| p.get("refusal").and_then(|t| t.as_str()))
                {
                    text.push_str(t);
                }
            }
            if images.is_empty() {
                return json!(text);
            }
            let mut out: Vec<Value> = Vec::new();
            if !text.is_empty() {
                out.push(json!({ "type": "text", "text": text }));
            }
            out.extend(images);
            json!(out)
        }
        _ => json!(""),
    }
}

/// Responses input_image part → OpenAI image_url 块（issue #154）：
/// `image_url`（规范字段）或 `url`（宽容形态）取一；http(s) 直链与白名单内
/// 图片 data URL 放行，其余拒绝（M1 审查收口：data: 形态不校验 MIME 可注入
/// text/html 诱导上游内容嗅探，与 Anthropic base64 分支白名单同一安全口径）。
/// 两者皆缺/为空/校验不过时返回 None（丢弃单个坏块而非整条消息）
fn input_image_to_openai(part: &Value) -> Option<Value> {
    let url = part
        .get("image_url")
        .and_then(|u| u.as_str())
        .or_else(|| part.get("url").and_then(|u| u.as_str()))?;
    if url.is_empty() {
        return None;
    }
    if !super::payload::image_url_allowed(url) {
        return None;
    }
    Some(json!({ "type": "image_url", "image_url": { "url": url } }))
}

/// 内部 OpenAI completion → Responses 对象（非流式 /v1/responses）
///
/// `encoded`（F4 移植，审查 2026-10-13）：请求侧 `encoded_tool_names` 收集的
/// namespace 组展平编码名集合——仅集合内命中的工具名拆回 {name, namespace}；
/// 未登记的扁平名（可能天然含 `__`，如 MCP 惯例 `mcp__server__tool`）保持
/// 单 name 字段。非 /v1/responses 入口的防御性分支传空集合。
pub fn completion_to_responses(v: &Value, resp_id: &str, model: &str, encoded: &HashSet<String>) -> Value {
    let message = v
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let finish = v
        .pointer("/choices/0/finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop")
        .to_string();
    // 截断（finish_reason=length）→ status=incomplete + incomplete_details（Responses 规范），
    // 客户端据此区分「正常完成」与「max_output_tokens 截断」
    let (status, incomplete_details) = if finish == "length" {
        ("incomplete", json!({"reason": "max_output_tokens"}))
    } else {
        ("completed", Value::Null)
    };
    let mut output: Vec<Value> = Vec::new();
    if let Some(t) = message.get("content").and_then(|c| c.as_str()) {
        if !t.is_empty() {
            output.push(json!({
                "id": format!("msg_{}", resp_id),
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": t, "annotations": []}],
            }));
        }
    }
    if let Some(tcs) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for (i, tc) in tcs.iter().enumerate() {
            // 编码名 `<ns>__<name>` 还原为 Codex 可路由的 {name, namespace} 双字段
            // （issue #93：Codex router 只认双字段，namespace 拼进 name 会被判未知工具）；
            // F4 移植：仅对请求侧登记的编码名拆分——客户端原生扁平工具名可能
            // 天然含 `__`（MCP 惯例），未登记者保持单 name 字段
            let raw = tc
                .pointer("/function/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (nm, ns) = if encoded.contains(raw) {
                split_namespaced(raw)
            } else {
                (raw.to_string(), None)
            };
            let mut item = json!({
                "id": format!("fc_{}_{}", resp_id, i),
                "type": "function_call",
                "status": "completed",
                "call_id": tc.get("id").cloned().unwrap_or(json!(format!("call_{}", i))),
                "name": nm,
                "arguments": tc.pointer("/function/arguments").cloned().unwrap_or(json!("{}")),
            });
            if let Some(ns) = ns {
                item["namespace"] = json!(ns);
            }
            output.push(item);
        }
    }
    let (it, ot) = v.get("usage").map(u64_pair).unwrap_or((0, 0));
    json!({
        "id": resp_id,
        "object": "response",
        "created_at": now_ts(),
        "status": status,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "incomplete_details": incomplete_details,
        "error": null,
        "usage": {
            "input_tokens": it,
            "input_tokens_details": {"cached_tokens": v.get("usage").map(cached_tokens_of).unwrap_or(0)},
            "output_tokens": ot,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": it + ot,
        },
        "stop_reason": if finish == "tool_calls" { json!("tool_use") } else { json!(null) },
    })
}

fn u64_pair(u: &Value) -> (u64, u64) {
    let get = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| u.get(*k).and_then(|v| v.as_u64()))
            .unwrap_or(0)
    };
    (
        get(&["prompt_tokens", "input_tokens"]),
        get(&["completion_tokens", "output_tokens"]),
    )
}

fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_instructions_string_input_and_tools() {
        let body = json!({
            "model": "glm-5.3",
            "instructions": "You are a coder.",
            "input": "hello",
            "tools": [{"type": "function", "name": "shell", "description": "run", "parameters": {"type": "object"}}],
            "reasoning": {"effort": "high", "summary": "auto"},
            "max_output_tokens": 1024,
        });
        let chat = responses_to_chat(&body).unwrap();
        assert_eq!(chat["messages"][0]["role"], json!("system"));
        assert_eq!(chat["messages"][0]["content"], json!("You are a coder."));
        assert_eq!(chat["messages"][1], json!({"role": "user", "content": "hello"}));
        assert_eq!(chat["tools"][0]["function"]["name"], json!("shell"));
        assert_eq!(chat["reasoning_effort"], json!("high"));
        assert_eq!(chat["max_tokens"], json!(1024));
    }

    #[test]
    fn converts_message_items_and_function_history() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "list files"}]},
                {"type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{\"cmd\":\"ls\"}"},
                {"type": "function_call_output", "call_id": "c1", "output": "a.txt"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]},
                {"type": "reasoning", "summary": []},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["content"], json!("list files"));
        assert_eq!(msgs[1]["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(msgs[2]["role"], json!("tool"));
        assert_eq!(msgs[2]["tool_call_id"], json!("c1"));
        assert_eq!(msgs[2]["content"], json!("a.txt"));
        assert_eq!(msgs[3]["content"], json!("done"));
    }

    #[test]
    fn rejects_missing_input_items() {
        assert!(responses_to_chat(&json!({"model": "m", "input": []})).is_err());
        assert!(responses_to_chat(&json!({"model": "m", "input": 42})).is_err());
        // input 缺失 → 宽容降级为空 user 消息
        let chat = responses_to_chat(&json!({"model": "m"})).unwrap();
        assert_eq!(chat["messages"][0]["role"], json!("user"));
    }

    /// issue #154：message content 数组中的 input_image 投影成 image_url，
    /// 无图消息维持纯字符串；data URL / url 两种取值字段都识别
    #[test]
    fn converts_message_input_image() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "这是什么？"},
                    {"type": "input_image", "image_url": "data:image/png;base64,aGVsbG8="}
                ]},
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "这张呢？"},
                    {"type": "input_image", "url": "https://example.com/b.png"}
                ]},
                {"type": "message", "role": "user", "content": "纯文本"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        let c0 = msgs[0]["content"].as_array().unwrap();
        assert_eq!(c0.len(), 2);
        assert_eq!(c0[0], json!({"type": "text", "text": "这是什么？"}));
        assert_eq!(
            c0[1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}})
        );
        let c1 = msgs[1]["content"].as_array().unwrap();
        assert_eq!(
            c1[1],
            json!({"type": "image_url", "image_url": {"url": "https://example.com/b.png"}})
        );
        assert_eq!(msgs[2]["content"], json!("纯文本"));
    }

    /// M1 回归锁：input_image 的 data: URL 必须过图片 MIME 白名单——
    /// text/html 等异常 MIME 丢块不透传（防 data: 前缀注入诱导上游内容嗅探），
    /// 白名单内图片 data URL 与 http(s) 直链不受影响
    #[test]
    fn input_image_data_url_mime_whitelist() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "恶意 MIME"},
                    {"type": "input_image", "image_url": "data:text/html;base64,PHNjcmlwdD4="}
                ]},
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "恶意 scheme"},
                    {"type": "input_image", "url": "file:///etc/passwd"}
                ]},
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "合法 data 图"},
                    {"type": "input_image", "image_url": "data:image/webp;base64,aGVsbG8="}
                ]},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // 前两条仅剩 text（坏块丢弃不弃整条消息），第三条图保留
        assert_eq!(msgs[0]["content"], json!("恶意 MIME"));
        assert_eq!(msgs[1]["content"], json!("恶意 scheme"));
        let c2 = msgs[2]["content"].as_array().unwrap();
        assert_eq!(
            c2[1],
            json!({"type": "image_url", "image_url": {"url": "data:image/webp;base64,aGVsbG8="}})
        );
    }

    /// issue #154：配对窗口内含图的 user 消息经 deferred 携带数组 content，
    /// 补回后形态不丢
    #[test]
    fn defers_image_message_after_tool_window() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "list"}]},
                {"type": "function_call", "call_id": "c1", "name": "shell", "arguments": "{}"},
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "顺便看图"},
                    {"type": "input_image", "image_url": "data:image/png;base64,QQ=="}
                ]},
                {"type": "function_call_output", "call_id": "c1", "output": "a.txt"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // user(list) / assistant(tool_calls) / tool(a.txt) / user(文本+图)
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2]["role"], json!("tool"));
        assert_eq!(msgs[3]["role"], json!("user"));
        let c = msgs[3]["content"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["type"], json!("text"));
        assert_eq!(
            c[1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,QQ=="}})
        );
    }

    #[test]
    fn completion_maps_to_responses_object() {
        let v = json!({
            "id": "chatcmpl-1",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3},
        });
        let resp = completion_to_responses(&v, "resp_1", "glm-5.3", &HashSet::new());
        assert_eq!(resp["object"], json!("response"));
        assert_eq!(resp["status"], json!("completed"));
        assert_eq!(resp["output"][0]["type"], json!("message"));
        assert_eq!(resp["output"][0]["content"][0]["text"], json!("hi"));
        assert_eq!(resp["usage"]["input_tokens"], json!(7));
        assert_eq!(resp["usage"]["total_tokens"], json!(10));
    }

    #[test]
    fn completion_maps_tool_calls_to_function_call_items() {
        let v = json!({
            "choices": [{"index": 0, "finish_reason": "tool_calls",
                         "message": {"role": "assistant", "content": null,
                                     "tool_calls": [{"id": "call_x", "type": "function",
                                                     "function": {"name": "shell", "arguments": "{\"cmd\":\"ls\"}"}}]}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        });
        let resp = completion_to_responses(&v, "resp_2", "m", &HashSet::new());
        assert_eq!(resp["output"][0]["type"], json!("function_call"));
        assert_eq!(resp["output"][0]["call_id"], json!("call_x"));
        assert_eq!(resp["stop_reason"], json!("tool_use"));
    }

    /// finish_reason=length（max_output_tokens 截断）→ status=incomplete + incomplete_details
    #[test]
    fn completion_maps_length_truncation_to_incomplete() {
        let v = json!({
            "choices": [{"index": 0, "finish_reason": "length",
                         "message": {"role": "assistant", "content": "truncat"}}],
        });
        let resp = completion_to_responses(&v, "resp_3", "m", &HashSet::new());
        assert_eq!(resp["status"], json!("incomplete"));
        assert_eq!(resp["incomplete_details"]["reason"], json!("max_output_tokens"));
        // 正常完成仍是 completed + incomplete_details=null
        let mut v2 = v.clone();
        v2["choices"][0]["finish_reason"] = json!("stop");
        let resp2 = completion_to_responses(&v2, "resp_4", "m", &HashSet::new());
        assert_eq!(resp2["status"], json!("completed"));
        assert!(resp2["incomplete_details"].is_null());
    }

    /// issue #69 核心：并行工具调用间夹 developer notice（image_resize_notice），
    /// assistant.tool_calls 后必须紧跟连续 tool 结果，notice 推迟到批次之后
    #[test]
    fn interleaved_developer_notices_do_not_break_tool_pairing() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "describe the screenshots"}]},
                {"type": "function_call", "call_id": "c1", "name": "view_image", "arguments": "{}"},
                {"type": "function_call", "call_id": "c2", "name": "view_image", "arguments": "{}"},
                {"type": "function_call", "call_id": "c3", "name": "view_image", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": [{"type": "input_image", "image_url": "data:image/png;base64,AAA"}]},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<notice>1</notice>"}]},
                {"type": "function_call_output", "call_id": "c2", "output": [{"type": "input_image", "image_url": "data:image/png;base64,BBB"}]},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<notice>2</notice>"}]},
                {"type": "function_call_output", "call_id": "c3", "output": [{"type": "input_image", "image_url": "data:image/png;base64,CCC"}]},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "<notice>3</notice>"}]},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // user, assistant(3 tool_calls), tool×3 连续, user notice×3
        assert_eq!(msgs.len(), 8);
        assert_eq!(msgs[1]["role"], json!("assistant"));
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 3);
        for (i, cid) in ["c1", "c2", "c3"].iter().enumerate() {
            assert_eq!(msgs[2 + i]["role"], json!("tool"));
            assert_eq!(msgs[2 + i]["tool_call_id"], json!(cid));
            // output 数组不整串 JSON 化：图片折叠为占位符，不携带 data URL
            let content = msgs[2 + i]["content"].as_str().unwrap();
            assert!(!content.contains("image_url"));
            assert!(content.contains("omitted"));
        }
        for (i, n) in ["1", "2", "3"].iter().enumerate() {
            assert_eq!(msgs[5 + i]["role"], json!("user"));
            assert_eq!(msgs[5 + i]["content"], json!(format!("<notice>{}</notice>", n)));
        }
    }

    /// 缺结果补占位 + 收尾顺序：占位 tool 必须先于被推迟消息（否则占位结果又被
    /// 推迟消息隔开，再次打断配对——参考补丁的收尾顺序缺陷回归）
    #[test]
    fn missing_outputs_get_placeholders_before_deferred_messages() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": "go"},
                {"type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}"},
                {"type": "function_call", "call_id": "c2", "name": "b", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "ok"},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "trailing note"}]},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // user, assistant(2 calls), tool c1, tool c2(占位), user(trailing note)
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["tool_call_id"], json!("c1"));
        assert_eq!(msgs[2]["content"], json!("ok"));
        assert_eq!(msgs[3]["role"], json!("tool"));
        assert_eq!(msgs[3]["tool_call_id"], json!("c2"));
        assert_eq!(msgs[3]["content"], json!("(tool did not return a result)"));
        assert_eq!(msgs[4]["role"], json!("user"));
        assert_eq!(msgs[4]["content"], json!("trailing note"));
    }

    /// 孤儿（无对应调用）与重复 function_call_output 丢弃，tool 条数与 tool_calls 对齐
    #[test]
    fn orphan_and_duplicate_outputs_are_dropped() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "first"},
                {"type": "function_call_output", "call_id": "c1", "output": "dup"},
                {"type": "function_call_output", "call_id": "ghost", "output": "orphan"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // assistant(tc c1), tool c1 —— 仅一条结果
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["content"], json!("first"));
    }

    /// 窗口内的 assistant 文本并入 tool_calls 载体，不拆出独立消息打断配对
    #[test]
    fn assistant_text_during_tool_window_merges_into_carrier() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": "go"},
                {"type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "thinking..."}]},
                {"type": "function_call_output", "call_id": "c1", "output": "done"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], json!("assistant"));
        assert_eq!(msgs[1]["content"], json!("thinking..."));
        assert_eq!(msgs[1]["tool_calls"][0]["id"], json!("c1"));
        assert_eq!(msgs[2]["role"], json!("tool"));
    }

    /// 多批次工具调用 + 批间 user 消息：各批次独立成对，批间消息不推迟
    #[test]
    fn sequential_tool_batches_with_interleaved_user_messages() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}"},
                {"type": "function_call", "call_id": "c2", "name": "b", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c2", "output": "r2"},
                {"type": "function_call_output", "call_id": "c1", "output": "r1"},
                {"type": "message", "role": "user", "content": "next"},
                {"type": "function_call", "call_id": "c3", "name": "c", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c3", "output": "r3"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // 乱序结果（c2 先到）在窗口关闭时按 tool_calls 顺序重排：
        // assistant(tc c1,c2), tool c1, tool c2, user, assistant(tc c3), tool c3
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[0]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[1]["tool_call_id"], json!("c1"));
        assert_eq!(msgs[1]["content"], json!("r1"));
        assert_eq!(msgs[2]["tool_call_id"], json!("c2"));
        assert_eq!(msgs[2]["content"], json!("r2"));
        assert_eq!(msgs[3]["role"], json!("user"));
        assert_eq!(msgs[4]["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(msgs[5]["tool_call_id"], json!("c3"));
    }

    /// 窗口内 system 消息推迟补回时保留 system 角色，developer 降级 user
    #[test]
    fn system_message_inside_tool_window_keeps_role_after_deferral() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "message", "role": "user", "content": "go"},
                {"type": "function_call", "call_id": "c1", "name": "a", "arguments": "{}"},
                {"type": "message", "role": "system", "content": "sys note"},
                {"type": "message", "role": "developer", "content": "dev note"},
                {"type": "function_call_output", "call_id": "c1", "output": "done"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        // user, assistant(tc c1), tool c1, system, user(dev note)
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[2]["role"], json!("tool"));
        assert_eq!(msgs[2]["tool_call_id"], json!("c1"));
        assert_eq!(msgs[3]["role"], json!("system"));
        assert_eq!(msgs[3]["content"], json!("sys note"));
        assert_eq!(msgs[4]["role"], json!("user"));
        assert_eq!(msgs[4]["content"], json!("dev note"));
    }

    /// function_call 缺 call_id：call_N 兜底自增，可与显式 call_N 结果配对
    #[test]
    fn function_call_without_call_id_falls_back_to_sequential_id() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "name": "a", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "r"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(msgs[1]["tool_call_id"], json!("call_1"));
        assert_eq!(msgs[1]["content"], json!("r"));
    }

    /// issue #93：namespace 工具（Codex multi_agent_v1 / mcp__* 等）不再被静默
    /// 丢弃，展平为编码名 `<ns>__<inner>` 的 function 送达上游
    #[test]
    fn namespace_tools_are_flattened_to_encoded_functions() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "shell", "description": "run", "parameters": {"type": "object"}},
                {
                    "type": "namespace",
                    "name": "multi_agent_v1",
                    "description": "Tools for spawning and managing sub-agents.",
                    "tools": [
                        {"type": "function", "name": "spawn_agent", "description": "spawn", "parameters": {"type": "object"}},
                        {"type": "function", "name": "wait_agent", "description": "wait", "parameters": {"type": "object"}},
                    ],
                },
                {
                    "type": "namespace",
                    "name": "mcp__node_repl",
                    "tools": [{"type": "function", "name": "js", "parameters": {"type": "object"}}],
                },
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        assert_eq!(tools[0]["function"]["name"], json!("shell"));
        assert_eq!(tools[1]["function"]["name"], json!("multi_agent_v1__spawn_agent"));
        assert_eq!(tools[2]["function"]["name"], json!("multi_agent_v1__wait_agent"));
        assert_eq!(tools[3]["function"]["name"], json!("mcp__node_repl__js"));
    }

    /// issue #93：split_namespaced 按最后一个 `__` 切分（namespace 自身可含 `__`），
    /// 无分隔符或分隔符在边界时不拆
    #[test]
    fn split_namespaced_handles_nested_and_plain_names() {
        let (nm, ns) = split_namespaced("mcp__node_repl__js");
        assert_eq!(nm, "js");
        assert_eq!(ns.as_deref(), Some("mcp__node_repl"));
        let (nm, ns) = split_namespaced("multi_agent_v1__spawn_agent");
        assert_eq!(nm, "spawn_agent");
        assert_eq!(ns.as_deref(), Some("multi_agent_v1"));
        // 无分隔符：原样返回（既有扁平工具回归）
        let (nm, ns) = split_namespaced("exec_command");
        assert_eq!(nm, "exec_command");
        assert!(ns.is_none());
        // 边界：分隔符在开头/结尾不拆
        let (nm, ns) = split_namespaced("__spawn");
        assert_eq!(nm, "__spawn");
        assert!(ns.is_none());
        let (nm, ns) = split_namespaced("ns__");
        assert_eq!(nm, "ns__");
        assert!(ns.is_none());
    }

    /// issue #93：Codex 回传历史 function_call 带 {name, namespace} 双字段，
    /// 拼回与 tools 声明一致的编码名
    #[test]
    fn namespaced_function_call_history_recombines_encoded_name() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "spawn_agent",
                 "namespace": "multi_agent_v1", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "c1", "output": "ok"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let msgs = chat["messages"].as_array().unwrap();
        assert_eq!(
            msgs[0]["tool_calls"][0]["function"]["name"],
            json!("multi_agent_v1__spawn_agent")
        );
    }

    /// issue #93：非流式响应把编码名还原为 {name, namespace} 双字段；
    /// 扁平工具输出无 namespace 字段（行为与旧实现一致）
    #[test]
    fn completion_restores_namespace_from_encoded_name() {
        let v = json!({
            "choices": [{"index": 0, "finish_reason": "tool_calls",
                         "message": {"role": "assistant", "content": null,
                                     "tool_calls": [
                                     {"id": "call_a", "type": "function",
                                      "function": {"name": "multi_agent_v1__spawn_agent", "arguments": "{}"}},
                                     {"id": "call_b", "type": "function",
                                      "function": {"name": "shell", "arguments": "{}"}},
                                 ]}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        });
        let resp = completion_to_responses(
            &v,
            "resp_ns",
            "m",
            &HashSet::from(["multi_agent_v1__spawn_agent".to_string()]),
        );
        let out = resp["output"].as_array().unwrap();
        assert_eq!(out[0]["type"], json!("function_call"));
        assert_eq!(out[0]["name"], json!("spawn_agent"));
        assert_eq!(out[0]["namespace"], json!("multi_agent_v1"));
        assert_eq!(out[1]["name"], json!("shell"));
        assert!(out[1].get("namespace").is_none());
    }

    /// 审查修复 #2：function_call 已带编码名 + namespace 双字段（双重前缀）时
    /// 原样保留，不拼出 `<ns>__<ns>__<name>`
    #[test]
    fn namespaced_function_call_with_encoded_name_not_double_prefixed() {
        let body = json!({
            "model": "m",
            "input": [
                {"type": "function_call", "call_id": "c1",
                 "name": "multi_agent_v1__spawn_agent", "namespace": "multi_agent_v1",
                 "arguments": "{}"},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        assert_eq!(
            chat["messages"][0]["tool_calls"][0]["function"]["name"],
            json!("multi_agent_v1__spawn_agent")
        );
    }

    /// 审查修复 #3：tool_choice 裸名命中 namespace 组内工具 → 拼回编码名；
    /// 顶层 function 名无命中 → 原样直传
    #[test]
    fn tool_choice_bare_name_resolves_to_encoded_name() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "shell"},
                {"type": "namespace", "name": "multi_agent_v1",
                 "tools": [{"type": "function", "name": "spawn_agent",
                            "parameters": {"type": "object"}}]},
            ],
            "tool_choice": {"type": "function", "name": "spawn_agent"},
        });
        let chat = responses_to_chat(&body).unwrap();
        assert_eq!(
            chat["tool_choice"]["function"]["name"],
            json!("multi_agent_v1__spawn_agent")
        );
        let body2 = json!({
            "model": "m",
            "input": "hi",
            "tools": [{"type": "function", "name": "shell"}],
            "tool_choice": {"type": "function", "name": "shell"},
        });
        let chat2 = responses_to_chat(&body2).unwrap();
        assert_eq!(chat2["tool_choice"]["function"]["name"], json!("shell"));
    }

    /// 审查修复 #4：namespace 组内编码名与顶层 function 重名 → 去重保留先声明者
    #[test]
    fn encoded_name_collision_keeps_first_declared() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "mcp__node_repl__js", "description": "top"},
                {"type": "namespace", "name": "mcp__node_repl",
                 "tools": [{"type": "function", "name": "js", "description": "inner"}]},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], json!("mcp__node_repl__js"));
        assert_eq!(tools[0]["function"]["description"], json!("top"));
    }

    /// 审查 #2 回归：裸名映射仅登记实际下发的组内工具——首个同名编码名
    /// 与顶层 function 碰撞被去重跳过时，tool_choice 裸名应映射到后续
    /// 实际下发的组内工具，而非指向被跳过、未向上游声明者
    #[test]
    fn choice_lookup_skips_deduped_tools() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [
                {"type": "function", "name": "ns1__js", "description": "top"},
                {"type": "namespace", "name": "ns1",
                 "tools": [{"type": "function", "name": "js", "description": "a"}]},
                {"type": "namespace", "name": "ns2",
                 "tools": [{"type": "function", "name": "js", "description": "b"}]},
            ],
            "tool_choice": {"type": "function", "name": "js"},
        });
        let chat = responses_to_chat(&body).unwrap();
        assert_eq!(
            chat["tool_choice"]["function"]["name"],
            json!("ns2__js")
        );
    }

    /// 审查修复 #5：组级 description 保留——内层为空/缺失回退组级，
    /// 均非空则组级在前拼接
    #[test]
    fn namespace_group_description_is_preserved() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "tools": [
                {"type": "namespace", "name": "multi_agent_v1",
                 "description": "Codex multi-agent toolset.",
                 "tools": [
                     {"type": "function", "name": "spawn_agent", "description": "Spawn a sub agent."},
                     {"type": "function", "name": "wait_agent", "description": ""},
                     {"type": "function", "name": "list_agents"},
                 ]},
            ],
        });
        let chat = responses_to_chat(&body).unwrap();
        let tools = chat["tools"].as_array().unwrap();
        assert_eq!(
            tools[0]["function"]["description"],
            json!("Codex multi-agent toolset.\n\nSpawn a sub agent.")
        );
        assert_eq!(
            tools[1]["function"]["description"],
            json!("Codex multi-agent toolset.")
        );
        assert_eq!(
            tools[2]["function"]["description"],
            json!("Codex multi-agent toolset.")
        );
    }

    /// F4 移植：encoded_tool_names 仅收集 namespace 组展平名，
    /// 顶层 function 不入集合；无 tools / 碰撞去重语义与 responses_to_chat 同口径
    #[test]
    fn encoded_tool_names_collects_only_namespace_groups() {
        let body = json!({
            "tools": [
                {"type": "function", "name": "mcp__server__tool", "parameters": {}},
                {"type": "namespace", "name": "multi_agent_v1", "tools": [
                    {"type": "function", "name": "spawn_agent", "parameters": {}}
                ]},
                {"type": "namespace", "name": "", "tools": [
                    {"type": "function", "name": "skipped_empty_ns", "parameters": {}}
                ]},
            ],
        });
        let encoded = encoded_tool_names(&body);
        assert!(encoded.contains("multi_agent_v1__spawn_agent"));
        assert_eq!(encoded.len(), 1);
        assert!(!encoded.contains("mcp__server__tool"));
        // 无 tools / 非对象 tools：空集合
        assert!(encoded_tool_names(&json!({"model": "m"})).is_empty());
    }

    /// F4 移植防漂移锁：encoded_tool_names 与 responses_to_chat 的 namespace 展平
    /// 是两处独立遍历，必须同口径——展平产物中所有含 `__` 的名字恰为登记集合
    /// （含同名碰撞去重、空组名/空内名跳过语义）。两侧漂移会导致回程误拆或漏还原
    #[test]
    fn encoded_tool_names_matches_flattened_tools_exactly() {
        let body = json!({
            "tools": [
                {"type": "function", "name": "shell", "parameters": {}},
                // 与顶层同名碰撞：编码名被去重跳过，不得入集合
                {"type": "namespace", "name": "collide", "tools": [
                    {"type": "function", "name": "shell", "parameters": {}}
                ]},
                {"type": "namespace", "name": "mcp__node_repl", "tools": [
                    {"type": "function", "name": "js", "parameters": {}},
                    // 空内名跳过
                    {"type": "function", "parameters": {}},
                    // 非 function 类型跳过
                    {"type": "other", "name": "not_fn", "parameters": {}}
                ]},
                // 空组名跳过
                {"type": "namespace", "tools": [{"type": "function", "name": "x", "parameters": {}}]},
            ],
        });
        let encoded = encoded_tool_names(&body);
        let chat = responses_to_chat(&body).unwrap();
        let with_sep: HashSet<String> = chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| {
                t.pointer("/function/name")
                    .and_then(|v| v.as_str())
                    .filter(|n| n.contains("__"))
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            encoded, with_sep,
            "登记集合必须与展平产物中含 `__` 的名字完全一致"
        );
        // 去重语义（与 responses_to_chat 一致，字面比较）：collide 组编码名
        // `collide__shell` 与顶层名 `shell` 字面不同 → 不冲突、正常展平入集合；
        // 仅当顶层 function 名字面等于编码名形式（如既有测试的
        // encoded_name_collision_keeps_first_declared）才被去重跳过
        assert_eq!(
            encoded,
            HashSet::from([
                "collide__shell".to_string(),
                "mcp__node_repl__js".to_string(),
            ])
        );
    }

    /// F4 移植回归：回程拆分以请求侧编码名集合为闸门——客户端原生扁平工具名
    /// 含 `__`（MCP 惯例 mcp__server__tool）未登记时不误拆，登记时正常还原
    #[test]
    fn completion_split_gated_by_encoded_names() {
        let v = json!({
            "choices": [{"index": 0, "finish_reason": "tool_calls",
                         "message": {"role": "assistant", "content": null,
                                     "tool_calls": [
                                        {"id": "call_1", "type": "function",
                                         "function": {"name": "mcp__server__tool", "arguments": "{}"}}]}}],
        });
        // 未登记（扁平原生名）：保持单 name，不产出 namespace
        let resp = completion_to_responses(&v, "resp_g1", "m", &HashSet::new());
        let fc = &resp["output"][0];
        assert_eq!(fc["type"], json!("function_call"));
        assert_eq!(fc["name"], json!("mcp__server__tool"));
        assert!(fc.get("namespace").is_none());
        // 登记（确属 namespace 分组展平名）：正常还原双字段
        let encoded = HashSet::from(["mcp__server__tool".to_string()]);
        let resp = completion_to_responses(&v, "resp_g2", "m", &encoded);
        let fc = &resp["output"][0];
        assert_eq!(fc["name"], json!("tool"));
        assert_eq!(fc["namespace"], json!("mcp__server"));
    }

    /// 编码名未登记（空集合）时原样透传，不拆分出 namespace（旧 split_ns=false 语义）
    #[test]
    fn completion_without_encoded_registration_keeps_encoded_name() {
        let v = json!({
            "choices": [{"index": 0, "finish_reason": "tool_calls",
                         "message": {"role": "assistant", "content": null,
                                     "tool_calls": [{"id": "call_a", "type": "function",
                                      "function": {"name": "mcp__node_repl__js", "arguments": "{}"}}]}}],
        });
        let resp = completion_to_responses(&v, "resp_nosplit", "m", &HashSet::new());
        let out = resp["output"].as_array().unwrap();
        assert_eq!(out[0]["type"], json!("function_call"));
        assert_eq!(out[0]["name"], json!("mcp__node_repl__js"));
        assert!(out[0].get("namespace").is_none());
    }
}
