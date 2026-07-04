use std::{
    convert::Infallible,
    path::{Path, PathBuf},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;
use tower::service_fn;
use tower_http::services::ServeDir;

pub fn spa_router(dist_dir: impl AsRef<Path>) -> Router {
    let dist_dir = dist_dir.as_ref().to_path_buf();
    let index_file = dist_dir.join("index.html");
    let fallback = service_fn(move |request: Request<Body>| {
        let index_file = index_file.clone();
        async move { Ok::<_, Infallible>(spa_fallback_response(request, index_file).await) }
    });

    Router::new().fallback_service(ServeDir::new(dist_dir).fallback(fallback))
}

async fn spa_fallback_response(request: Request<Body>, index_file: PathBuf) -> Response {
    let path = request.uri().path();
    if is_api_path(path) {
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

    if is_static_file_path(path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    serve_index_file(&index_file).await
}

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/") || path == "/v1" || path.starts_with("/v1/")
}

async fn serve_index_file(path: &Path) -> Response {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

fn is_static_file_path(path: &str) -> bool {
    Path::new(path.trim_start_matches('/'))
        .extension()
        .is_some()
}
