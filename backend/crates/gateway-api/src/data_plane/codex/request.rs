//! Codex 请求身份、Guardian 与子代理元数据的客户端边界

use crate::data_plane::headers::header_string;
use axum::http::HeaderMap;
use gateway_core::operation::ClientTurn;
use gateway_protocol::openai::{
    X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER, X_OPENAI_MEMGEN_REQUEST_HEADER,
};
use serde_json::{Map, Value};

const OPENAI_SUBAGENT_KEY: &str = "x-openai-subagent";
const PASSTHROUGH_HEADERS_CONTEXT_KEY: &str = "opaque_request_headers";

#[derive(Clone, Default)]
pub(crate) struct CodexRequestHeaders {
    guardian: Option<String>,
    ambiguous_guardian_headers: bool,
    turn_state: Option<String>,
    turn_metadata: Option<String>,
    version: Option<String>,
    codex_window_id: Option<String>,
    parent_thread_id: Option<String>,
    turn_id: Option<String>,
    responses_lite: Option<String>,
    memgen_request: Option<String>,
    subagent: Option<String>,
}

impl CodexRequestHeaders {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            guardian: header_string(headers, "x-codex-guardian"),
            ambiguous_guardian_headers: [
                "x-codex-guardian",
                OPENAI_SUBAGENT_KEY,
                "x-codex-turn-metadata",
            ]
            .into_iter()
            .any(|name| {
                let mut values = headers.get_all(name).iter();
                values.next().is_some_and(|v| v.to_str().is_err()) || values.next().is_some()
            }),
            turn_state: header_string(headers, "x-codex-turn-state"),
            turn_metadata: header_string(headers, "x-codex-turn-metadata"),
            version: header_string(headers, "version"),
            codex_window_id: header_string(headers, "x-codex-window-id"),
            parent_thread_id: header_string(headers, "x-codex-parent-thread-id"),
            turn_id: header_string(headers, "x-codex-turn-id"),
            responses_lite: header_string(headers, X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER),
            memgen_request: header_string(headers, X_OPENAI_MEMGEN_REQUEST_HEADER),
            subagent: header_string(headers, OPENAI_SUBAGENT_KEY),
        }
    }

    pub(crate) fn append_context(&self, context: &mut Map<String, Value>) {
        for (key, value) in [
            ("turn_state", self.turn_state.as_ref()),
            ("turn_metadata", self.turn_metadata.as_ref()),
            ("subagent", self.subagent.as_ref()),
            ("version", self.version.as_ref()),
            ("codex_window_id", self.codex_window_id.as_ref()),
            ("parent_thread_id", self.parent_thread_id.as_ref()),
            ("turn_id", self.turn_id.as_ref()),
            ("responses_lite", self.responses_lite.as_ref()),
            ("memgen_request", self.memgen_request.as_ref()),
        ] {
            if let Some(value) = value {
                context.insert(key.into(), Value::String(value.clone()));
            }
        }
    }

    pub(crate) fn apply(
        &self,
        body: &mut Map<String, Value>,
        context: &mut Map<String, Value>,
        websocket_frame: bool,
    ) -> Option<ClientTurn> {
        gateway_protocol::openai::capture_codex_guardian_identity(
            body,
            context,
            self.guardian.as_deref(),
            self.ambiguous_guardian_headers,
        );
        self.apply_subagent(body, context, websocket_frame);

        let semantics = gateway_protocol::openai::codex_responses_request_semantics(body, context);
        if body.get("generate").and_then(Value::as_bool) != Some(false)
            && semantics.request_kind.as_deref() == Some("turn")
            && !semantics.compact
        {
            gateway_protocol::openai::codex_account_thread_id(body, context)
                .zip(gateway_protocol::openai::codex_turn_id(body, context))
                .and_then(|(thread, turn)| ClientTurn::new(thread, turn))
        } else {
            None
        }
    }

    fn apply_subagent(
        &self,
        body: &mut Map<String, Value>,
        context: &mut Map<String, Value>,
        websocket_frame: bool,
    ) {
        if websocket_frame
            && let Some(frame_agent) = body
                .get("client_metadata")
                .and_then(|m| m.get(OPENAI_SUBAGENT_KEY))
                .and_then(Value::as_str)
            && (frame_agent == "guardian"
                || self.subagent.as_deref() == Some("guardian")
                || self.guardian.is_some())
        {
            // 帧内声明优先；旧 opening 的不透明副本也不能在 Provider 再度覆盖
            let stale_subagent = self
                .subagent
                .as_deref()
                .is_some_and(|opening| opening != frame_agent);
            let stale_guardian = self.guardian.as_deref().is_some_and(|opening| {
                frame_agent != "guardian"
                    || gateway_protocol::openai::codex_guardian_role(body, context)
                        .is_some_and(|role| role.header() != Some(opening))
            });
            if stale_subagent {
                context.insert("subagent".into(), Value::from(frame_agent));
            }
            if (stale_subagent || stale_guardian)
                && let Some(Value::Array(headers)) =
                    context.get_mut(PASSTHROUGH_HEADERS_CONTEXT_KEY)
            {
                headers.retain(|pair| match pair.get(0).and_then(Value::as_str) {
                    Some(OPENAI_SUBAGENT_KEY) => !stale_subagent,
                    Some("x-codex-guardian") => !stale_guardian,
                    _ => true,
                });
            }
            return;
        }
        if let Some(subagent) = self.subagent.as_deref() {
            inject_subagent_metadata(body, subagent);
        }
    }
}

fn inject_subagent_metadata(body: &mut Map<String, Value>, subagent: &str) {
    let metadata = body
        .entry("client_metadata".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };
    metadata.insert(
        OPENAI_SUBAGENT_KEY.to_owned(),
        Value::String(subagent.to_owned()),
    );
}
