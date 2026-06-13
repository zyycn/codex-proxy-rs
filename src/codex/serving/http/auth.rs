use axum::http::HeaderMap;

use crate::{platform::http::auth::client_api_key, runtime::state::AppState};

pub(super) async fn authorize_client_api_key(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(api_key) = client_api_key(headers) else {
        return false;
    };
    state
        .services
        .api_keys
        .verify(api_key.as_str())
        .await
        .unwrap_or(false)
}
