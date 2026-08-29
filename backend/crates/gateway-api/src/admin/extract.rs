//! 管理端 JSON 与 Query extractor 的稳定 rejection 映射。

use axum::{
    Json,
    extract::{
        FromRequest, FromRequestParts, OptionalFromRequest, Query, Request,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

use super::AdminError;

/// 将 Axum JSON rejection 收口为管理端错误信封。
pub struct AdminJson<T>(pub T);

impl<T, S> FromRequest<S> for AdminJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        <Json<T> as FromRequest<S>>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(map_json_rejection)
    }
}

impl<T, S> OptionalFromRequest<S> for AdminJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request(request: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        <Json<T> as OptionalFromRequest<S>>::from_request(request, state)
            .await
            .map(|value| value.map(|Json(value)| Self(value)))
            .map_err(map_json_rejection)
    }
}

/// 将 Axum Query rejection 收口为管理端错误信封。
pub struct AdminQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for AdminQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AdminError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(map_query_rejection)
    }
}

fn map_json_rejection(rejection: JsonRejection) -> AdminError {
    let status = rejection.status();
    match rejection {
        JsonRejection::JsonSyntaxError(_) => AdminError::malformed_json(),
        JsonRejection::JsonDataError(_) => {
            AdminError::invalid_request(StatusCode::UNPROCESSABLE_ENTITY, "请求字段不合法")
        }
        JsonRejection::MissingJsonContentType(_) => AdminError::invalid_request(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "请求必须使用 application/json",
        ),
        JsonRejection::BytesRejection(_) if status == StatusCode::PAYLOAD_TOO_LARGE => {
            AdminError::invalid_request(status, "请求体过大")
        }
        JsonRejection::BytesRejection(_) => AdminError::internal(),
        _ => AdminError::invalid_request(status, "请求体不合法"),
    }
}

fn map_query_rejection(_: QueryRejection) -> AdminError {
    AdminError::bad_request("请求参数不合法")
}
