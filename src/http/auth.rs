use axum::http::HeaderMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientApiKey<'a>(&'a str);

impl<'a> ClientApiKey<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

pub fn client_api_key(headers: &HeaderMap) -> Option<ClientApiKey<'_>> {
    let auth = headers.get("authorization")?.to_str().ok()?.trim();
    let token = auth.strip_prefix("Bearer ")?.trim();
    token.starts_with("cpr_").then_some(ClientApiKey(token))
}

pub fn admin_session_id(headers: &HeaderMap) -> Option<&str> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    cookie.split(';').map(str::trim).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == "cpr_admin_session" && !value.is_empty()).then_some(value)
    })
}
