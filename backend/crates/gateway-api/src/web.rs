//! 静态页面与独立于发行文件的部署前缀

use std::path::PathBuf;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, Response};
use axum::routing::get;
use serde::Deserialize;
use tower_http::services::ServeDir;

use crate::ApiConfigError;

/// 已校验的同源部署前缀，以斜杠开头和结尾
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(try_from = "String")]
pub struct WebBasePath(String);

impl Default for WebBasePath {
    fn default() -> Self {
        Self("/".to_owned())
    }
}

impl TryFrom<String> for WebBasePath {
    type Error = ApiConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "/" {
            return Ok(Self(value));
        }
        if !value.starts_with('/')
            || !value.ends_with('/')
            || value[1..value.len() - 1].split('/').any(|segment| {
                segment.is_empty()
                    || matches!(segment, "." | "..")
                    || !segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
                    })
            })
        {
            return Err(ApiConfigError::InvalidWebBasePath);
        }
        Ok(Self(value))
    }
}

#[derive(Clone)]
struct WebAssets {
    index: PathBuf,
    base_path: WebBasePath,
}

pub(super) fn router(directory: PathBuf, base_path: WebBasePath) -> Router {
    let assets = WebAssets {
        index: directory.join("index.html"),
        base_path,
    };
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .fallback_service(
            ServeDir::new(directory)
                .append_index_html_on_directories(false)
                .fallback(get(index).with_state(assets.clone())),
        )
        .with_state(assets)
        .layer(axum::middleware::map_response(cache_control))
}

async fn index(State(assets): State<WebAssets>) -> Result<Html<String>, StatusCode> {
    let html = tokio::fs::read_to_string(&assets.index)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    // 只变换响应，保留更新器校验和回滚依赖的发行文件字节
    Ok(Html(html.replacen(
        "<base href=\"/\"",
        &format!("<base href=\"{}\"", assets.base_path.0),
        1,
    )))
}

async fn cache_control(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}
