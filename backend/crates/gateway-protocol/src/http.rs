//! 跨协议 HTTP 头部传输边界与重试时间解析

/// 解析 HTTP 和流内错误共用的 Retry-After，保留零延迟并向上取整剩余秒数
pub fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    let now = std::time::SystemTime::now();
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse().ok();
    }
    let remaining = httpdate::parse_http_date(value)
        .ok()?
        .duration_since(now)
        .unwrap_or_default();
    Some(
        remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() != 0)),
    )
}

/// 判断小写请求头是否属于传输层管理的字段，不得作为业务扩展头透传
///
/// API 入站和 Provider 编码共用此分类；`Connection` 动态声明的逐跳头由入站额外剥离
/// 只用于请求方向，不影响上游响应中的代理诊断信息
#[must_use]
pub fn is_transport_managed_request_header(name: &str) -> bool {
    name.starts_with("sec-websocket-")
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-connection"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "host"
                | "content-length"
                // 请求实体与响应压缩能力属于各段 transport，不能继承下游协商
                | "content-encoding"
                | "accept-encoding"
                // 网关中间件也会生成该链路诊断 ID，不作为业务上下文转发
                | "x-request-id"
        )
}

/// 判断响应头是否满足 HTTP 传输和通用鉴权材料的转发边界
///
/// `connection_options` 来自同一响应的 `Connection` 字段，调用方应先按逗号拆分并
/// 转成小写
/// 此函数拒绝逐跳头、实体 framing 和通用认证材料
/// 平台专属账号身份由调用边界组合对应协议规则过滤
#[must_use]
pub fn response_header_is_forwardable(name: &str, connection_options: &[String]) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if connection_options
        .iter()
        .any(|option| option.eq_ignore_ascii_case(&name))
        || name.starts_with("sec-websocket-")
    {
        return false;
    }

    !matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "content-type"
            | "content-encoding"
            | "authorization"
            | "x-api-key"
            | "www-authenticate"
            | "authentication-info"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-authentication-info"
            | "cookie"
            | "cookie2"
            | "set-cookie"
            | "set-cookie2"
    )
}
