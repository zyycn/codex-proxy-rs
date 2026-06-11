use std::collections::BTreeMap;

use crate::fingerprint::model::Fingerprint;

pub fn build_codex_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "text/event-stream".to_string());
    headers.insert(
        "authorization".to_string(),
        format!("Bearer {access_token}"),
    );
    headers.insert("originator".to_string(), fp.originator.clone());
    headers.insert("user-agent".to_string(), fp.user_agent());
    headers.insert("x-client-request-id".to_string(), request_id.to_string());
    // 中文注释：这些 Codex Desktop 私有头是上游识别链路的一部分，不能按普通 OpenAI API 精简。
    headers.insert(
        "x-openai-internal-codex-residency".to_string(),
        "global".to_string(),
    );
    if let Some(account_id) = account_id {
        headers.insert("chatgpt-account-id".to_string(), account_id.to_string());
    }
    if let Some(turn_state) = turn_state {
        headers.insert("x-codex-turn-state".to_string(), turn_state.to_string());
    }
    headers
}
