//! 入口消费互联推理路径，拒绝回到本部署的请求

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse as _, Response},
};
use gateway_protocol::peer::{ROUTE_HEADER, ROUTING_LOOP_CODE};

pub(super) enum Error {
    Invalid,
    Loop,
}

impl Error {
    pub(super) fn response(self, path: &str) -> Response {
        let (status, message, code) = match self {
            Self::Invalid => (
                StatusCode::BAD_REQUEST,
                "Invalid peer routing path.",
                "invalid_peer_route",
            ),
            Self::Loop => (
                StatusCode::LOOP_DETECTED,
                "Peer routing loop detected.",
                ROUTING_LOOP_CODE,
            ),
        };
        let (_, mut json) = crate::client_error::openai_error_response(
            status,
            message,
            "invalid_request_error",
            code,
        );
        if matches!(path, "/v1/messages" | "/v1/messages/count_tokens") {
            json.0["type"] = serde_json::json!("error");
        }
        (status, json).into_response()
    }
}

pub(super) fn enter(headers: &mut HeaderMap, node: HeaderValue) -> Result<HeaderValue, Error> {
    if !headers.contains_key(ROUTE_HEADER) {
        return Ok(node);
    }
    let mut path = Vec::new();
    for value in headers.get_all(ROUTE_HEADER) {
        for part in value.to_str().map_err(|_| Error::Invalid)?.split(',') {
            let id = part.trim();
            if id.len() != 32
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Error::Invalid);
            }
            if id.as_bytes() == node.as_bytes() {
                return Err(Error::Loop);
            }
            path.extend_from_slice(id.as_bytes());
            path.push(b',');
        }
    }
    // 消费线协议头；内部调用继承执行作用域，上游重试不能重复追加本节点
    headers.remove(ROUTE_HEADER);
    path.extend_from_slice(node.as_bytes());
    HeaderValue::from_maybe_shared(bytes::Bytes::from(path)).map_err(|_| Error::Invalid)
}
