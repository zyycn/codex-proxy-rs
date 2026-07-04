use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;

#[derive(Clone)]
struct AssetState {
    dist_dir: Arc<PathBuf>,
}

pub fn spa_router(dist_dir: impl AsRef<Path>) -> Router {
    let state = AssetState {
        dist_dir: Arc::new(dist_dir.as_ref().to_path_buf()),
    };

    Router::new()
        .route("/", get(serve_index))
        .route("/assets/{*path}", get(serve_asset))
        .fallback(serve_spa_fallback)
        .with_state(state)
}

async fn serve_index(State(state): State<AssetState>) -> Response {
    serve_file(&state.dist_dir.join("index.html")).await
}

async fn serve_spa_fallback(State(state): State<AssetState>, uri: Uri) -> Response {
    if is_api_path(uri.path()) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "code": 40401,
                "message": "API route not found",
                "data": null
            })),
        )
            .into_response();
    }

    serve_file(&state.dist_dir.join("index.html")).await
}

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/") || path == "/v1" || path.starts_with("/v1/")
}

async fn serve_asset(
    State(state): State<AssetState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let Some(relative_path) = safe_relative_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    serve_file(&state.dist_dir.join("assets").join(relative_path)).await
}

async fn serve_file(path: &Path) -> Response {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        content_type_for_path(&path.to_string_lossy()),
    );
    response
}

fn content_type_for_path(path: &str) -> HeaderValue {
    if path.ends_with(".html") || path == "/" {
        HeaderValue::from_static("text/html; charset=utf-8")
    } else if path.ends_with(".js") {
        HeaderValue::from_static("text/javascript; charset=utf-8")
    } else if path.ends_with(".css") {
        HeaderValue::from_static("text/css; charset=utf-8")
    } else if path.ends_with(".json") {
        HeaderValue::from_static("application/json")
    } else if path.ends_with(".svg") {
        HeaderValue::from_static("image/svg+xml")
    } else {
        HeaderValue::from_static("application/octet-stream")
    }
}

fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let mut sanitized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => sanitized.push(segment),
            _ => return None,
        }
    }

    (!sanitized.as_os_str().is_empty()).then_some(sanitized)
}
