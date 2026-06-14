# Status Codes

HTTP status codes are not replaced by body codes. Admin/frontend APIs return both an accurate HTTP status and a JSON body `code` unless the status is `204 No Content`.

## Health

| Status | Meaning |
| --- | --- |
| `200` | Service is running. |

## Admin and Frontend APIs

| HTTP status | Body code range | Meaning |
| --- | --- | --- |
| `200` | `200` | Successful read, login, or lifecycle with response body. |
| `201` | `201` | Resource created. |
| `204` | `204` | Successful lifecycle with no response body. No JSON envelope is sent. |
| `400` | `40000`-`40099` | Malformed JSON, invalid parameters, or invalid cursor. |
| `401` | `40100`-`40199` | Missing/expired admin session or bad admin password. |
| `403` | `40300`-`40399` | Authenticated admin lacks permission for a local-only/bootstrap action. |
| `404` | `40400`-`40499` | Resource not found. |
| `409` | `40900`-`40999` | Duplicate resource or stale state transition. |
| `422` | `42200`-`42299` | Well-formed request failed domain validation. |
| `429` | `42900`-`42999` | Login or operation rate limit. |
| `500` | `50000`-`50099` | Internal service error. |

Recommended initial body codes:

| Body code | HTTP status | Meaning |
| --- | --- | --- |
| `40001` | `400` | Validation failed. |
| `40002` | `400` | Invalid cursor. |
| `40101` | `401` | Admin session required. |
| `40102` | `401` | Admin password invalid. |
| `40301` | `403` | Bootstrap action denied. |
| `40401` | `404` | Resource not found. |
| `40901` | `409` | Duplicate resource. |
| `42201` | `422` | Domain validation failed. |
| `42901` | `429` | Login rate limited. |
| `50001` | `500` | Internal service error. |

## OpenAI-Compatible `/v1/*`

| Status | Meaning |
| --- | --- |
| `200` | Successful response or SSE stream accepted. |
| `400` | Invalid OpenAI/Responses request body. |
| `401` | Missing or invalid client API key. |
| `404` | Requested model is not a supported Codex model. |
| `413` | Request payload too large for safe replay or forwarding. |
| `429` | Codex quota or rate limit surfaced to client. |
| `499` | Client aborted; log internally only. |
| `502` | Upstream Codex transport or protocol failure. |
| `503` | No usable Codex account is available. |
| `504` | Upstream timeout. |
