//! Gemini 模型目录复用冻结范围与原生协议能力，分页不扩大可见模型集合

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::{
    engine::execution::AuthenticatedClient,
    operation::{GenerateRequest, Operation, ProtocolPayload, RawJsonPayload, TokenCountRequest},
};
use gateway_protocol::genai::GENAI_PROTOCOL;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    ApiState,
    data_plane::{
        auth::{ClientKeyHeaders, authenticate_client, client_access_error_response},
        middleware::{self, RequestInput},
    },
};

pub(super) const MODELS_PATH: &str = "/v1beta/models";

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelsQuery {
    #[serde(alias = "page_size")]
    page_size: Option<i64>,
    #[serde(alias = "page_token")]
    page_token: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    after: String,
    size: usize,
}

pub(super) async fn list(
    State(state): State<ApiState>,
    Query(query): Query<ModelsQuery>,
    headers: HeaderMap,
) -> Response {
    query_response(state, headers, None, query).await
}

pub(super) async fn detail(
    State(state): State<ApiState>,
    Path(model): Path<String>,
    headers: HeaderMap,
) -> Response {
    // action 路由只有 POST，不能让新增详情 GET 接管生成方法
    if model.contains(':') {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "This model action requires POST.",
        );
    }
    if !super::valid_segment(&model) {
        return error(StatusCode::BAD_REQUEST, "Invalid model name.");
    }
    query_response(state, headers, Some(model), ModelsQuery::default()).await
}

async fn query_response(
    state: ApiState,
    headers: HeaderMap,
    model: Option<String>,
    query: ModelsQuery,
) -> Response {
    let service = state.data_plane();
    let client = match authenticate_client(service, &headers, ClientKeyHeaders::Bearer).await {
        Ok(client) => client,
        Err(err) => return client_access_error_response(err),
    };
    let endpoint = if model.is_some() {
        super::MODEL_ACTION_PATH
    } else {
        MODELS_PATH
    };
    let mut input = RequestInput::query(endpoint.to_owned(), model.clone(), &headers);
    input.protocol = GENAI_PROTOCOL.to_owned();
    middleware::query_response(
        service.execution(),
        client,
        input,
        move |client| async move {
            match catalog(&client) {
                Ok(models) => match model {
                    Some(model) => models
                        .into_iter()
                        .find(|value| value["name"] == format!("models/{model}"))
                        .map_or_else(
                            || error(StatusCode::NOT_FOUND, "Requested model was not found."),
                            success,
                        ),
                    None => paginate(models, query),
                },
                Err(()) => error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Model catalog is unavailable.",
                ),
            }
        },
    )
    .await
}

fn catalog(client: &AuthenticatedClient) -> Result<Vec<Value>, ()> {
    let generate = Operation::Generate(
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(GENAI_PROTOCOL, Map::new()).map_err(|_| ())?,
        )
        .requiring_native_protocol(),
    );
    let count = Operation::CountTokens(
        TokenCountRequest::from_raw_json(
            RawJsonPayload::new(GENAI_PROTOCOL, bytes::Bytes::from_static(b"{}"))
                .map_err(|_| ())?,
        )
        .requiring_native_protocol(),
    );
    let snapshot = client.snapshot();
    let scope = client.policy().account_scope();
    let counted = snapshot.public_models_for_operation(scope, &count);
    Ok(snapshot
        .public_models_for_operation(scope, &generate)
        .into_iter()
        .filter(|model| super::valid_segment(model.as_str()))
        .map(|model| {
            let mut methods = vec!["generateContent", "streamGenerateContent"];
            if counted.binary_search(&model).is_ok() {
                methods.push("countTokens");
            }
            // 上游尚未提供完整 Gemini 元数据，未知版本与 Token 上限保持缺失
            json!({
                "name": format!("models/{}", model.as_str()),
                "displayName": model.as_str(),
                "supportedGenerationMethods": methods,
            })
        })
        .collect())
}

fn paginate(models: Vec<Value>, query: ModelsQuery) -> Response {
    let size = match query.page_size.unwrap_or(50) {
        n if n < 0 => return error(StatusCode::BAD_REQUEST, "pageSize must not be negative."),
        0 => 50,
        n => n.min(1000) as usize,
    };
    let start = if let Some(token) = query.page_token.filter(|token| !token.is_empty()) {
        let cursor = URL_SAFE_NO_PAD
            .decode(token)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok());
        let Some(cursor) = cursor.filter(|cursor| cursor.size == size) else {
            return error(
                StatusCode::BAD_REQUEST,
                "Invalid pageToken or changed pageSize.",
            );
        };
        let Some(index) = models
            .iter()
            .position(|model| model["name"] == cursor.after)
        else {
            return error(
                StatusCode::BAD_REQUEST,
                "pageToken is no longer in the model catalog.",
            );
        };
        index + 1
    } else {
        0
    };
    let end = start.saturating_add(size).min(models.len());
    let mut body = json!({"models": &models[start..end]});
    if end < models.len() {
        let cursor = Cursor {
            after: models[end - 1]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            size,
        };
        let Ok(bytes) = serde_json::to_vec(&cursor) else {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not encode pageToken.",
            );
        };
        body["nextPageToken"] = json!(URL_SAFE_NO_PAD.encode(bytes));
    }
    success(body)
}

fn success(value: Value) -> Response {
    ([("cache-control", "private, no-store")], Json(value)).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(super::error::payload(status, message))).into_response()
}
