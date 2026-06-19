//! 资产路由。

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{Path as AxumPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::headers::{apply_static_headers, content_type_for_path};

#[derive(Clone)]
struct AssetState {
    dist_dir: Arc<PathBuf>,
}

/// 构造前端静态资源路由。
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
    serve_file(&state.dist_dir.join("index.html"), "/").await
}

async fn serve_spa_fallback(State(state): State<AssetState>) -> Response {
    serve_file(&state.dist_dir.join("index.html"), "/").await
}

async fn serve_asset(
    State(state): State<AssetState>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let Some(relative_path) = safe_relative_path(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let request_path = format!("/assets/{path}");

    serve_file(
        &state.dist_dir.join("assets").join(relative_path),
        &request_path,
    )
    .await
}

async fn serve_file(path: &Path, request_path: &str) -> Response {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(bytes));
    let path_for_content_type = path.to_string_lossy();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        content_type_for_path(&path_for_content_type),
    );
    apply_static_headers(headers, request_path);
    response
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
