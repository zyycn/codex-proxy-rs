# API Contract

`codex-proxy-rs` exposes two API families.

## `/v1/*`

These endpoints are OpenAI-compatible and are authenticated only by enabled local client API keys created through `/admin/api-keys` and sent as `Authorization: Bearer cpr_...`.
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

### Model Routes

`GET /v1/models` returns the OpenAI-compatible model list. The default static Codex model is `gpt-5.5`.

`GET /v1/models/catalog` returns Codex model metadata for UI use. Aliases are used for request parsing but are not exposed as standalone models.

`GET /v1/models/{id}` returns an OpenAI-compatible model object or a `404` OpenAI error body with `code=model_not_found`.

`GET /v1/models/{id}/info` returns the extended Codex catalog entry for a known model.

Model name parsing supports configured aliases plus `-low`, `-medium`, `-high`, `-xhigh`, `-fast`, and `-flex` suffixes.

### `POST /v1/responses`

Uses imported Codex accounts to call `POST /codex/responses` on the configured Codex backend. The upstream request is sent with Codex Desktop headers, account bearer token, optional account id, request id, and encrypted account-scoped Cookie replay.

When the client omits `stream` or sets `"stream": false`, the Rust route collects upstream HTTP SSE until `response.completed` and returns the completed OpenAI-compatible response JSON.

When the client sets `"stream": true`, the Rust route returns `text/event-stream` and passes through upstream SSE frames while collecting usage for the account after the stream finishes. Both modes capture upstream `Set-Cookie`, replay encrypted account-scoped Cookies, record account usage when usage appears in SSE, and write a `v1.response` event log with `requestId`, `accountId`, `route`, `model`, `statusCode`, `latencyMs`, and non-secret metadata.

If Codex returns `401` and the account has a stored refresh token, the route refreshes the account through the OpenAI OAuth token endpoint once, persists the rotated access token, preserves the old refresh token when the server omits a new one, updates the in-process account pool, and retries the same request once.

`previous_response_id` and explicit WebSocket-only requests are rejected until the WebSocket transport is implemented and verified.

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

### `GET /admin/logs`

Returns cursor-paginated event logs for admin troubleshooting. The list uses the standard admin page envelope and lower camelCase fields.

Example item:

```json
{
  "id": "log_01",
  "requestId": "req_01",
  "kind": "v1.response",
  "level": "info",
  "accountId": "acct_01",
  "route": "/v1/responses",
  "model": "gpt-5.5",
  "statusCode": 200,
  "latencyMs": 123,
  "message": "v1 responses completed",
  "metadata": {
    "stream": false,
    "usage": {
      "inputTokens": 7,
      "outputTokens": 4,
      "cachedTokens": 2,
      "totalTokens": 11
    }
  },
  "createdAt": "2026-06-11T12:00:00Z"
}
```

### `GET /admin/usage-stats`

Returns cursor-paginated account usage counters recorded after `/v1/*` calls. The endpoint never returns account access tokens, refresh tokens, Cookie values, or client API keys.

Example item:

```json
{
  "accountId": "acct_01",
  "email": "user@example.com",
  "label": "primary",
  "planType": "plus",
  "requestCount": 2,
  "inputTokens": 14,
  "outputTokens": 8,
  "cachedTokens": 3,
  "lastUsedAt": "2026-06-11T12:00:00Z"
}
```

### `GET /admin/usage-stats/summary`

Returns global usage totals across accounts.

Success response:

```json
{
  "code": 200,
  "message": "OK",
  "data": {
    "accountCount": 1,
    "requestCount": 2,
    "inputTokens": 14,
    "outputTokens": 8,
    "cachedTokens": 3
  },
  "requestId": "req_01"
}
```

### `GET /admin/api-keys`

Returns local client API keys with cursor pagination. The plaintext key and hash are never returned by list endpoints.

### `POST /admin/api-keys`

Creates a local client API key for `/v1/*`. The plaintext value is returned only in this response; store it client-side and use it as `Authorization: Bearer <plaintext>`.

Request:

```json
{
  "name": "cursor"
}
```

Success response:

```json
{
  "code": 200,
  "message": "OK",
  "data": {
    "id": "key_01",
    "name": "cursor",
    "prefix": "cpr_xxxxxxxx",
    "enabled": true,
    "createdAt": "2026-06-11T12:00:00Z",
    "lastUsedAt": null,
    "plaintext": "cpr_full_key_returned_once"
  },
  "requestId": "req_01"
}
```

### `GET /admin/accounts`

Returns stored Codex accounts with cursor pagination. Access tokens and refresh tokens are never decrypted or returned.

### `POST /admin/accounts/import`

Imports accounts into encrypted SQLite storage. The request body uses the Rust import format, which accepts the exported account object fields needed by this service and ignores unrelated fields.

Request:

```json
{
  "accounts": [
    {
      "id": "acct_01",
      "email": "user@example.com",
      "accountId": "chatgpt-account",
      "userId": "chatgpt-user",
      "label": "primary",
      "planType": "plus",
      "token": "access-token",
      "refreshToken": "refresh-token",
      "status": "active"
    }
  ]
}
```

Success response:

```json
{
  "code": 200,
  "message": "OK",
  "data": {
    "imported": 1,
    "skipped": 0
  },
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
