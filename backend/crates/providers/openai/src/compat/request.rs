//! 下游客户端模型请求正文的 Codex 协议兼容

use serde_json::{Map, Value, json};

/// 非官方客户端及跨客户端历史回填的兼容入口；调用方必须已选定 Codex/OAuth 上游
/// 只处理已验证被拒绝的字段，不根据客户端品牌推断整份历史是否合法
pub(super) fn normalize_non_codex_request_body(body: &mut Map<String, Value>) {
    normalize_codex_oauth_options(body);
    let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in input {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        // SDK 输出项的 status 不属于 Codex reasoning 输入合同；其他项的 status 可能合法
        item.shift_remove("status");
        // Codex 只接受空 content 数组；仅在加密历史仍可回填时去掉冗余明文，
        // 没有加密内容的历史不自动丢弃，保留给上游明确拒绝
        if item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.trim().is_empty())
            && item
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| !content.is_empty())
        {
            item.shift_remove("content");
        }
    }
}

fn normalize_codex_oauth_options(body: &mut Map<String, Value>) {
    // Codex Core 省略默认档位，OAuth 不接受公开 API 的 auto/default 字面值
    if matches!(
        body.get("service_tier").and_then(Value::as_str),
        Some("auto" | "default")
    ) {
        body.shift_remove("service_tier");
    }
    // Codex OAuth 拒绝这些客户端选项，即使采样参数取默认值
    for field in [
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "chat_template_kwargs",
        "user",
        "metadata",
        "safety_identifier",
    ] {
        body.shift_remove(field);
    }
    // Responses 自带用量；保留官方流式推理摘要等其他选项
    if let Some(options) = body
        .get_mut("stream_options")
        .and_then(Value::as_object_mut)
    {
        options.shift_remove("include_usage");
    }
    // 只省略没有实际效果的配置，不静默取消客户端要求的截断、停止条件或缓存策略
    if body.get("truncation").and_then(Value::as_str) == Some("disabled") {
        body.shift_remove("truncation");
    }
    if body
        .get("stop_sequences")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        body.shift_remove("stop_sequences");
    }
    if body
        .get("prompt_cache_options")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        body.shift_remove("prompt_cache_options");
    }
    // 原生上限优先，Chat 别名只补缺，非法值仍交给上游校验
    for alias in ["max_completion_tokens", "max_tokens"] {
        if let Some(limit) = body.shift_remove(alias)
            && !limit.is_null()
            && body.get("max_output_tokens").is_none_or(Value::is_null)
        {
            body.insert("max_output_tokens".to_owned(), limit);
        }
    }
}

/// 补齐 Codex 请求缺省字段并适配已确认不兼容的请求形状，不递归清洗业务正文
///
/// 兼容基准是 Codex Core/Desktop 的模型请求，不是公开 OpenAI Responses API
/// 未知字段继续透传，不能因官方请求结构中没有某个字段就将其列入过滤规则
pub(crate) fn normalize_codex_request_body(body: &mut Map<String, Value>) {
    // 官方 Core/Desktop 显式发送 store=false；仅为缺字段的下游请求补齐，保留显式值
    body.entry("store").or_insert(Value::Bool(false));

    // 公开 Responses API 允许 `input` 为字符串（等价于一条 user 文本消息），
    // 而 Codex 后端只接受条目数组，否则返回 400 "Input must be a list"
    // 这里按官方 ResponseItem::Message 的形状展开；其他非数组类型不猜测语义，交给上游判定
    if let Some(Value::String(text)) = body.get_mut("input") {
        let text = std::mem::take(text);
        body.insert(
            "input".to_owned(),
            json!([{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            }]),
        );
    }

    // Codex 上游拒绝显式 message 的 system role；沿用官方客户端的 developer
    // role 承载指令，只转换已确认的消息形状，保留内容与其他字段
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some("system")
            {
                item.insert("role".to_owned(), Value::String("developer".to_owned()));
            }
        }
    }

    // Codex 拒绝显式采样温度和缓存保留时长，独立的缓存键仍须保留
    for field in ["temperature", "prompt_cache_retention"] {
        body.remove(field);
    }
}
