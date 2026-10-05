//! 下游客户端模型请求正文的 Codex 协议兼容

use serde_json::{Map, Value, json};

/// 非官方客户端及跨客户端历史回填的兼容入口；调用方必须已选定 Codex/OAuth 上游
/// 只处理已验证被拒绝的字段，不根据客户端品牌推断整份历史是否合法
pub(crate) fn normalize_non_codex_request_body(body: &mut Map<String, Value>) {
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

/// 补齐 Codex 请求缺省字段并适配已确认不兼容的请求形状，不递归清洗业务正文
///
/// 兼容基准是 Codex Core/Desktop 的模型请求，不是公开 OpenAI Responses API
/// 未知字段继续透传，不能因官方请求结构中没有某个字段就将其列入过滤规则
pub(in crate::transport) fn normalize_codex_request_body(body: &mut Map<String, Value>) {
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

    for field in [
        // Pi 普通 Responses 适配将 maxTokens 映射为 max_output_tokens，
        // temperature 则原样写入；Pi 的 Codex 适配也可能发送 temperature
        "max_output_tokens",
        "temperature",
        // Pi 开启长缓存时发送 24h；保留有效的 prompt_cache_key，
        // 只剥离 Codex Responses 明确拒绝的缓存保留时长参数
        "prompt_cache_retention",
    ] {
        body.remove(field);
    }
}
