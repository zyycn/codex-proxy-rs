# API Contract

`codex-proxy-rs` exposes two API families.

## `/v1/*`

These endpoints are OpenAI-compatible and are authenticated only by client API keys using `Authorization: Bearer cpr_...`.
Responses include an `X-Request-Id` header for tracing, but the body stays OpenAI-compatible and does not include a custom `requestId` field.

Error body:

```json
{
  "error": {
    "message": "Invalid client API key",
    "type": "invalid_request_error",
    "param": null,
    "code": "invalid_api_key"
  }
}
```

Streaming errors after SSE has started use:

```text
event: error
data: {"error":{"message":"Upstream failed","type":"server_error","param":null,"code":"upstream_error"}}
```

## `/admin/*`

Admin endpoints are authenticated only by HttpOnly admin session cookies.
Admin JSON uses lower camelCase field names. Every admin response includes an `X-Request-Id` header, and every JSON body includes `requestId`.

Use real HTTP status codes and body-level frontend codes together. Do not return HTTP `200` for failed requests. The body `code` exists for frontend branching; the HTTP status remains the transport truth.

### `POST /admin/login`

Authenticates only with the configured admin password stored in `admin_users`. Client API keys (`Bearer cpr_...`) are ignored by the admin login flow and cannot create admin sessions.

Request:

```json
{
  "password": "admin-password"
}
```

Success response:

```json
{
  "code": 200,
  "message": "OK",
  "data": {
    "expiresAt": "2026-06-11T12:00:00Z"
  },
  "requestId": "req_01"
}
```

Success also returns:

```http
Set-Cookie: cpr_admin_session=...; Path=/; HttpOnly; SameSite=Lax; Max-Age=3600
```

Invalid password response uses HTTP `401` and body code `40102`.

### `GET /admin/settings`

Returns the in-scope runtime settings visible to the admin UI. This endpoint is read-only; persistent settings mutation will be added separately and must use admin sessions, not client API keys.

Success response:

```json
{
  "code": 200,
  "message": "OK",
  "data": {
    "defaultModel": "gpt-5.5",
    "defaultReasoningEffort": null,
    "serviceTier": null,
    "modelAliases": {},
    "refreshEnabled": true,
    "refreshMarginSeconds": 300,
    "refreshConcurrency": 2,
    "maxConcurrentPerAccount": 3,
    "requestIntervalMs": 50,
    "rotationStrategy": "least_used",
    "tierPriority": [],
    "quotaRefreshIntervalMinutes": 5,
    "quotaWarningThresholds": {
      "primary": [80, 90],
      "secondary": [80, 90]
    },
    "quotaSkipExhausted": true,
    "logsEnabled": false,
    "logsCapacity": 2000,
    "logsCaptureBody": false,
    "usageHistoryRetentionDays": null
  },
  "requestId": "req_01"
}
```

Success body:

```json
{
  "code": 200,
  "message": "OK",
  "data": {},
  "requestId": "req_01"
}
```

List body:

```json
{
  "code": 200,
  "message": "OK",
  "data": [],
  "page": {
    "limit": 50,
    "nextCursor": null
  },
  "requestId": "req_01"
}
```

Error body:

```json
{
  "code": 40101,
  "message": "Admin login required",
  "data": null,
  "requestId": "req_01"
}
```

Pagination uses cursor ordering by `(created_at desc, id desc)`, default `limit=50`, max `limit=200`.

Rust structs use `PascalCase` type names and `snake_case` fields internally, then expose lower camelCase through serde:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: T,
    pub request_id: String,
}
```
