mod support;

mod codex_serving {
    mod chat_completions;
    mod diagnostics_route;
    mod responses_http_sse;
    mod responses_websocket;
    mod routes_chat;
    mod routes_responses;
    mod upstream_errors;
    mod upstream_fallback;
}
