//! 已识别的非 Codex 环境信息过滤；官方身份和会话规则由 transport 自身维护

/// 调用方须传入 HeaderName 规范化后的小写名称
///
/// 此处的“官方”仅指 Codex Core/Desktop 发往 Codex 上游的请求协议，
/// 不包括整个 OpenAI SDK 生态
/// 名单中的字段可能是合法 HTTP 字段，
/// 过滤表示网关不继承下游环境，不代表官方服务端必然拒绝该字段
/// 按已知来源命名空间覆盖扩展；其他未知业务头不因源码中未出现而被过滤
pub(super) fn is_non_codex_request_header(name: &str) -> bool {
    // Cloudflare 链路信息及 Access 认证不跨到上游；cf-* 不只包含诊断字段
    name.starts_with("cf-")
        // 反代记录的是客户端到网关这一段地址、协议和路由
        || name.starts_with("x-forwarded-")
        // OpenAI 官方 Python/Node SDK 也发送 X-Stainless-*；Pi 的普通 Responses
        // 适配通过该 SDK 发送这些环境、版本和重试信息，不作为 Codex 上游画像继承
        || name.starts_with("x-stainless-")
        // UA Client Hints 与 Fetch Metadata 是浏览器标准，描述下游浏览器
        // 和页面请求上下文，不能当作网关连接上游时的环境
        || name.starts_with("sec-ch-ua")
        || name.starts_with("sec-fetch-")
        || matches!(
            name,
            // Forwarded、Via、CDN-Loop 有 RFC 定义；剥离是本应用重建
            // Provider 请求的策略，不是通用 HTTP 代理的协议要求
            "forwarded"
                | "via"
                | "cdn-loop"
                // Nginx/代理与 Cloudflare 使用的原始访客地址
                | "x-real-ip"
                | "true-client-ip"
                // 页面来源与引用地址属于下游请求，不继承到上游
                | "origin"
                | "referer"
                // Pi 普通 Responses 适配使用的会话头别名；协议层已提取其
                // 会话语义，上游按 session-id 输出，这里仅过滤原始 HTTP 头
                | "session_id"
        )
}
