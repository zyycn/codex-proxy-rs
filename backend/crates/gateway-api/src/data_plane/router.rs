//! 模型数据面路由

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use super::{
    chat_completions::chat_completions,
    images::{image_edits, image_generations},
    live,
    messages::{count_tokens, messages},
    models::{model_detail, models},
    responses::{responses, responses_websocket},
    search::standalone_search,
    usage,
};

use crate::ApiState;

// Core 使用操作类型，不根据这些协议路径分派业务
pub(super) use gateway_protocol::openai::RESPONSES_PATH;
pub(super) const MESSAGES_PATH: &str = "/v1/messages";
pub(super) const MESSAGE_TOKENS_PATH: &str = "/v1/messages/count_tokens";
pub(super) const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
pub(super) const IMAGE_GENERATIONS_PATH: &str = "/v1/images/generations";
pub(super) const IMAGE_EDITS_PATH: &str = "/v1/images/edits";
pub(super) const SEARCH_PATH: &str = "/v1/alpha/search";
pub(super) const MODELS_PATH: &str = "/v1/models";
pub(super) const MODEL_DETAIL_PATH: &str = "/v1/models/{model_id}";
pub(super) const USAGE_PATH: &str = "/v1/usage";
pub(super) const PEER_LINK_PATH: &str = "/v1/cpr/link";

/// 构造模型数据面路由
pub(crate) fn router() -> Router<ApiState> {
    Router::new()
        .route(IMAGE_GENERATIONS_PATH, post(image_generations))
        .route(IMAGE_EDITS_PATH, post(image_edits))
        .route(SEARCH_PATH, post(standalone_search))
        .route(RESPONSES_PATH, get(responses_websocket).post(responses))
        .route(CHAT_COMPLETIONS_PATH, post(chat_completions))
        .route(MESSAGES_PATH, post(messages))
        .route(MESSAGE_TOKENS_PATH, post(count_tokens))
        .route(MODELS_PATH, get(models))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束
        .route(MODEL_DETAIL_PATH, get(model_detail))
        .merge(super::genai::router())
        .merge(usage::router())
        .merge(super::peer_link::router())
        .merge(live::router())
        // 生成请求正文属于客户端/上游协议；代理不能用私有大小上限提前拒绝
        // 上游本可接受的未来 payload
        .layer(DefaultBodyLimit::disable())
        .merge(super::provider_endpoints::router())
}

#[derive(Clone, Copy)]
pub(crate) enum DataPlaneNamespace {
    V1,
    GenAi,
}

impl DataPlaneNamespace {
    pub(crate) fn for_path(path: &str) -> Option<Self> {
        if path == "/v1" || path.starts_with("/v1/") {
            Some(Self::V1)
        } else if path == "/v1beta" || path.starts_with("/v1beta/") || path.starts_with("/v1beta:")
        {
            Some(Self::GenAi)
        } else {
            None
        }
    }

    pub(crate) const fn unmatched_path(self) -> &'static str {
        match self {
            Self::V1 => "/v1/<unmatched>",
            Self::GenAi => "/v1beta/<unmatched>",
        }
    }
}

/// 模型数据面的未知路径由协议答复，其余未匹配路径交给静态资源服务
pub(crate) async fn fallback(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use crate::client_error::{ProtocolError, ProtocolErrorBody};
    let Some(namespace) = DataPlaneNamespace::for_path(request.uri().path()) else {
        return next.run(request).await;
    };
    let response = super::error::protocol_error_response(
        axum::http::StatusCode::NOT_FOUND,
        ProtocolErrorBody {
            error: ProtocolError {
                kind: "invalid_request_error",
                code: "unknown_endpoint",
                message: "The requested API endpoint does not exist.".into(),
                param: None,
            },
        },
    );
    match namespace {
        DataPlaneNamespace::V1 => response,
        DataPlaneNamespace::GenAi => super::genai::project_error_response(response).await,
    }
}
