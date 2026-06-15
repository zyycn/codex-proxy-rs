use indexmap::IndexMap;

use crate::codex::gateway::fingerprint::model::Fingerprint;

pub fn build_codex_base_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let mut headers = IndexMap::new();

    headers.insert(
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    if let Some(id) = account_id {
        headers.insert("chatgpt-account-id".to_string(), id.to_string());
    }
    headers.insert("originator".to_string(), fp.originator.clone());
    headers.insert("user-agent".to_string(), fp.user_agent());
    headers.insert("sec-ch-ua".to_string(), fp.sec_ch_ua());

    for (key, value) in &fp.default_headers {
        let key_lower = key.to_lowercase();
        if !headers.contains_key(&key_lower) {
            headers.insert(key_lower, value.clone());
        }
    }

    headers
}

pub fn build_codex_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let mut headers = build_codex_base_headers(fp, access_token, account_id);
    headers.insert(
        "x-openai-internal-codex-residency".to_string(),
        "us".to_string(),
    );
    headers.insert("x-client-request-id".to_string(), request_id.to_string());

    if let Some(state) = turn_state {
        headers.insert("x-codex-turn-state".to_string(), state.to_string());
    }

    headers.insert("accept".to_string(), "text/event-stream".to_string());

    headers
}

fn order_headers(headers: IndexMap<String, String>, order: &[String]) -> IndexMap<String, String> {
    let mut ordered = IndexMap::new();

    for key in order {
        if let Some(value) = headers.get(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }

    for (key, value) in headers {
        if !ordered.contains_key(&key) {
            ordered.insert(key, value);
        }
    }

    ordered
}

pub fn build_ordered_codex_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> IndexMap<String, String> {
    let headers = build_codex_headers(fp, access_token, account_id, turn_state, request_id);
    order_headers(headers, &fp.header_order)
}

pub fn build_ordered_codex_base_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
) -> IndexMap<String, String> {
    let headers = build_codex_base_headers(fp, access_token, account_id);
    order_headers(headers, &fp.header_order)
}
