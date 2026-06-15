# Backend Naming Audit

## Verdict

当前后端命名可以作为第一版完整架构定稿。主边界已经清楚：`runtime` 负责装配，`web` 负责未来 Web 控制台承载，`admin` 负责管理域，`codex` 负责 Codex 业务内核，`platform` 负责跨领域基础设施。

命名的艺术不在于词汇华丽，而在于每个名字都能回答三个问题：

- 这个目录属于哪个领域。
- 这个文件对外暴露什么责任。
- 这个名字是否会让后来的人误以为它还承担了别的职责。

本轮审计结论：源码目录已经达到“可长期演进”的层级。剩余问题主要是少数兼容性术语和偏大的聚合文件，不影响当前架构定稿。

## Naming Principles

- 路由表面用 `api`、`http`、`web` 表达协议和承载面。
- 业务能力用领域名表达，例如 `accounts`、`models`、`events`、`gateway`。
- 基础设施只放在 `platform`，避免业务目录承担系统能力。
- 测试目录跟随生产目录命名，测试文件表达场景而不是实现细节。
- 对外兼容词可以保留在 route 和 DTO 中，内部模块优先使用更准确的领域词。

## Complete Architecture Tree

以下目录排除了生成物和本地状态：`.git`、`target`、`data`、`logs`、`designs`。

```text
.
├── docs
│   ├── superpowers
│   │   ├── plans
│   │   │   ├── 2026-06-11-codex-proxy-rs.md
│   │   │   ├── 2026-06-12-openai-gpt-codex-parity.md
│   │   │   ├── 2026-06-13-utils-module-migration.md
│   │   │   ├── 2026-06-14-backend-naming-architecture.md
│   │   │   └── 2026-06-14-logging-refactor.md
│   │   └── specs
│   │       ├── 2026-06-12-openai-gpt-codex-parity-design.md
│   │       ├── 2026-06-13-architecture-refactor-design.md
│   │       ├── 2026-06-14-backend-naming-architecture-design.md
│   │       └── 2026-06-14-logging-refactor-design.md
│   ├── ADMIN_INITIALIZATION_FIX.md
│   ├── FINGERPRINT_AUTO_UPDATE.md
│   ├── FINGERPRINT_FIX.md
│   ├── FINGERPRINT_PROPAGATION_FIX.md
│   ├── FINGERPRINT_UPDATE_TEST_REPORT.md
│   ├── IMPLEMENTATION_COMPARISON.md
│   ├── IMPLEMENTATION_COMPLETE.md
│   ├── OPENAI_CODEX_PARITY_AUDIT.md
│   ├── OPENAI_REQUEST_ALIGNMENT.md
│   ├── RELIABILITY_ENHANCEMENTS.md
│   ├── RESPONSE_CHAIN_ALIGNMENT.md
│   ├── api.md
│   ├── architecture-audit.md
│   ├── architecture-capability-plan.md
│   ├── architecture-reorganization.md
│   ├── backend-naming-audit.md
│   ├── code-quality-audit.md
│   ├── database-storage-audit.md
│   ├── dependency-policy.md
│   ├── implementation-status.md
│   ├── optimization-roadmap.md
│   ├── rust-community-review.md
│   ├── scheduler-completion-report.md
│   ├── scheduler-implementation.md
│   ├── scheduler-usage.md
│   └── status-codes.md
├── src
│   ├── admin
│   │   ├── api
│   │   │   ├── accounts
│   │   │   │   ├── cookies.rs
│   │   │   │   ├── create.rs
│   │   │   │   ├── delete.rs
│   │   │   │   ├── export.rs
│   │   │   │   ├── health.rs
│   │   │   │   ├── import.rs
│   │   │   │   ├── lifecycle.rs
│   │   │   │   ├── list.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── oauth.rs
│   │   │   │   └── quota.rs
│   │   │   ├── client_keys
│   │   │   │   ├── create.rs
│   │   │   │   ├── export.rs
│   │   │   │   ├── import.rs
│   │   │   │   ├── lifecycle.rs
│   │   │   │   ├── list.rs
│   │   │   │   └── mod.rs
│   │   │   ├── logs
│   │   │   │   ├── detail.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── query.rs
│   │   │   │   └── state.rs
│   │   │   ├── diagnostics.rs
│   │   │   ├── mod.rs
│   │   │   ├── models.rs
│   │   │   ├── response.rs
│   │   │   ├── router.rs
│   │   │   ├── session.rs
│   │   │   ├── settings.rs
│   │   │   └── usage.rs
│   │   ├── client_keys
│   │   │   ├── mod.rs
│   │   │   └── service.rs
│   │   ├── session
│   │   │   ├── mod.rs
│   │   │   ├── repository.rs
│   │   │   └── service.rs
│   │   ├── tasks
│   │   │   ├── mod.rs
│   │   │   └── session_cleanup.rs
│   │   ├── mod.rs
│   │   └── settings.rs
│   ├── codex
│   │   ├── accounts
│   │   │   ├── cookies
│   │   │   │   ├── jar.rs
│   │   │   │   ├── mod.rs
│   │   │   │   └── repository.rs
│   │   │   ├── repository
│   │   │   │   ├── accounts.rs
│   │   │   │   ├── leases.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── quotas.rs
│   │   │   │   ├── tokens.rs
│   │   │   │   └── usage.rs
│   │   │   ├── service
│   │   │   │   ├── cookies.rs
│   │   │   │   ├── health.rs
│   │   │   │   ├── import.rs
│   │   │   │   ├── lifecycle.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── pool_sync.rs
│   │   │   │   ├── quota.rs
│   │   │   │   └── refresh.rs
│   │   │   ├── cloudflare_challenge.rs
│   │   │   ├── jwt.rs
│   │   │   ├── lifecycle.rs
│   │   │   ├── mod.rs
│   │   │   ├── model.rs
│   │   │   ├── pool.rs
│   │   │   └── usage_snapshots.rs
│   │   ├── events
│   │   │   ├── event.rs
│   │   │   ├── mod.rs
│   │   │   ├── repository.rs
│   │   │   └── service.rs
│   │   ├── gateway
│   │   │   ├── fingerprint
│   │   │   │   ├── mod.rs
│   │   │   │   ├── model.rs
│   │   │   │   ├── repository.rs
│   │   │   │   ├── update_checker.rs
│   │   │   │   └── updater.rs
│   │   │   ├── oauth
│   │   │   │   ├── client.rs
│   │   │   │   ├── codex_cli.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── refresh.rs
│   │   │   │   └── token.rs
│   │   │   ├── protocol
│   │   │   │   ├── codex_to_openai.rs
│   │   │   │   ├── error.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── openai_to_codex.rs
│   │   │   │   └── schema.rs
│   │   │   ├── transport
│   │   │   │   ├── websocket
│   │   │   │   │   ├── codec.rs
│   │   │   │   │   ├── mod.rs
│   │   │   │   │   └── pool.rs
│   │   │   │   ├── headers.rs
│   │   │   │   ├── http_client.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── rate_limits.rs
│   │   │   │   ├── sse.rs
│   │   │   │   ├── types.rs
│   │   │   │   └── usage_events.rs
│   │   │   ├── conversation_identity.rs
│   │   │   ├── installation_id.rs
│   │   │   └── mod.rs
│   │   ├── models
│   │   │   ├── catalog.rs
│   │   │   ├── mod.rs
│   │   │   ├── repository.rs
│   │   │   └── service.rs
│   │   ├── serving
│   │   │   ├── dispatch
│   │   │   │   ├── account_refresh.rs
│   │   │   │   ├── affinity.rs
│   │   │   │   ├── fallback.rs
│   │   │   │   ├── limits.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── routing.rs
│   │   │   │   ├── stream.rs
│   │   │   │   ├── stream_audit.rs
│   │   │   │   └── usage.rs
│   │   │   ├── http
│   │   │   │   ├── auth.rs
│   │   │   │   ├── chat.rs
│   │   │   │   ├── diagnostics.rs
│   │   │   │   ├── errors.rs
│   │   │   │   ├── mod.rs
│   │   │   │   ├── models.rs
│   │   │   │   ├── responses.rs
│   │   │   │   └── router.rs
│   │   │   ├── chat.rs
│   │   │   ├── diagnostics.rs
│   │   │   ├── mod.rs
│   │   │   └── responses.rs
│   │   ├── tasks
│   │   │   ├── mod.rs
│   │   │   ├── model_refresh.rs
│   │   │   ├── quota_refresh.rs
│   │   │   └── token_refresh.rs
│   │   ├── usage
│   │   │   ├── mod.rs
│   │   │   └── service.rs
│   │   └── mod.rs
│   ├── config
│   │   ├── loader.rs
│   │   ├── mod.rs
│   │   └── types.rs
│   ├── platform
│   │   ├── crypto
│   │   │   ├── mod.rs
│   │   │   └── secret_box.rs
│   │   ├── http
│   │   │   ├── auth.rs
│   │   │   ├── health.rs
│   │   │   ├── mod.rs
│   │   │   └── request_id.rs
│   │   ├── identity
│   │   │   ├── admin_session.rs
│   │   │   ├── client_key.rs
│   │   │   ├── client_key_repository.rs
│   │   │   ├── error.rs
│   │   │   └── mod.rs
│   │   ├── logging
│   │   │   ├── mod.rs
│   │   │   └── rotation.rs
│   │   ├── storage
│   │   │   ├── db.rs
│   │   │   ├── mod.rs
│   │   │   ├── paths.rs
│   │   │   └── schema.sql
│   │   └── mod.rs
│   ├── runtime
│   │   ├── tasks
│   │   │   ├── coordinator.rs
│   │   │   ├── mod.rs
│   │   │   └── types.rs
│   │   ├── bootstrap.rs
│   │   ├── mod.rs
│   │   ├── router.rs
│   │   └── state.rs
│   ├── utils
│   │   ├── json.rs
│   │   ├── mod.rs
│   │   └── pagination.rs
│   ├── web
│   │   ├── assets.rs
│   │   ├── mod.rs
│   │   ├── router.rs
│   │   ├── security.rs
│   │   └── shell.rs
│   ├── lib.rs
│   └── main.rs
├── tests
│   ├── admin
│   │   ├── accounts
│   │   │   ├── cookies_quota.rs
│   │   │   ├── import_export.rs
│   │   │   ├── lifecycle.rs
│   │   │   ├── list.rs
│   │   │   └── oauth.rs
│   │   ├── api_contract.rs
│   │   ├── client_keys_route.rs
│   │   ├── logs_route.rs
│   │   ├── models_route.rs
│   │   ├── session.rs
│   │   ├── session_login_route.rs
│   │   ├── session_repository.rs
│   │   ├── settings_route.rs
│   │   └── usage_stats_route.rs
│   ├── architecture
│   │   ├── accounts_boundary.rs
│   │   ├── admin_accounts_split.rs
│   │   ├── admin_boundary.rs
│   │   ├── codex_activity_boundary.rs
│   │   ├── gateway_boundary.rs
│   │   ├── platform_boundary.rs
│   │   ├── runtime_tasks_boundary.rs
│   │   └── serving_boundary.rs
│   ├── codex_accounts
│   │   ├── cookie_store.rs
│   │   ├── pool_scheduling.rs
│   │   ├── refresh.rs
│   │   ├── refresh_scheduler.rs
│   │   ├── repository.rs
│   │   └── service_refresh.rs
│   ├── codex_events
│   │   └── logs_pagination.rs
│   ├── codex_gateway
│   │   ├── websocket
│   │   │   └── pool.rs
│   │   ├── cli_auth_import.rs
│   │   ├── fingerprint_update.rs
│   │   ├── headers.rs
│   │   ├── http_client.rs
│   │   ├── oauth_refresh.rs
│   │   ├── usage_events.rs
│   │   └── websocket.rs
│   ├── codex_models
│   │   └── catalog.rs
│   ├── codex_serving
│   │   ├── chat_completions.rs
│   │   ├── diagnostics_route.rs
│   │   ├── responses_http_sse.rs
│   │   ├── responses_websocket.rs
│   │   ├── routes_chat.rs
│   │   ├── routes_responses.rs
│   │   ├── upstream_errors.rs
│   │   └── upstream_fallback.rs
│   ├── fixtures
│   │   ├── chat
│   │   │   ├── non_stream_text.sse
│   │   │   ├── parallel_tools_success.sse
│   │   │   ├── retry_after_success.sse
│   │   │   ├── stream_text.sse
│   │   │   ├── tool_reasoning_complete.sse
│   │   │   └── tool_stream.sse
│   │   └── responses
│   │       ├── http_sse
│   │       │   ├── after_402.sse
│   │       │   ├── after_403.sse
│   │       │   ├── after_429.sse
│   │       │   ├── after_cloudflare_403.sse
│   │       │   ├── completed_fields.sse
│   │       │   ├── completed_reasoning_include.sse
│   │       │   ├── completed_usage.sse
│   │       │   ├── default_stream.sse
│   │       │   ├── done_item_completed.sse
│   │       │   ├── error_event.sse
│   │       │   ├── failed_event.sse
│   │       │   ├── stream_after_429.sse
│   │       │   ├── stream_error_event.sse
│   │       │   ├── stream_failed_event.sse
│   │       │   ├── stream_usage.sse
│   │       │   └── text_deltas_completed.sse
│   │       └── websocket
│   │           ├── completed.json
│   │           ├── first_account_limited.json
│   │           ├── history_rate_limited.json
│   │           ├── rate_limited.json
│   │           ├── second_account_limited.json
│   │           └── token_revoked.json
│   ├── platform
│   │   ├── client_key_auth.rs
│   │   ├── crypto.rs
│   │   ├── http_auth.rs
│   │   ├── log_rotation.rs
│   │   └── storage_schema.rs
│   ├── runtime
│   │   ├── http_trace.rs
│   │   └── startup.rs
│   ├── support
│   │   ├── admin_accounts.rs
│   │   ├── mod.rs
│   │   └── upstream.rs
│   ├── admin.rs
│   ├── architecture.rs
│   ├── codex_accounts.rs
│   ├── codex_events.rs
│   ├── codex_gateway.rs
│   ├── codex_models.rs
│   ├── codex_serving.rs
│   ├── config.rs
│   ├── platform.rs
│   └── runtime.rs
├── .gitignore
├── Cargo.lock
├── Cargo.toml
├── Dockerfile
├── README.md
├── config.yaml
├── docker-compose.yml
└── rust-toolchain.toml
```

## Domain Audit

| Area | Verdict | Notes |
| --- | --- | --- |
| `src/runtime` | Good | `bootstrap`、`router`、`state`、`tasks` 表达装配层职责，适合单 crate 起步阶段。 |
| `src/web` | Good | 只作为未来 Admin Web 控制台的后端挂载面，没有前端历史包袱。 |
| `src/admin/api` | Good | 管理 JSON API 从 `http` 收束为 `api`，比协议名更能表达契约层。 |
| `src/admin/session` | Good | 管理员登录态与 `client_keys` 拆开，避免 `auth` 泛化。 |
| `src/admin/client_keys` | Good | 内部语义是本地客户端凭据，不是上游 provider key。 |
| `src/codex/accounts` | Good | 账号、cookies、仓储、服务、调度池都在账号域内。 |
| `src/codex/models` | Good | model catalog 从账号域剥离，边界正确。 |
| `src/codex/events` | Good | 业务事件从进程日志中独立出来，命名比 `logs` 更准确。 |
| `src/codex/gateway` | Good | `protocol`、`transport`、`oauth`、`fingerprint` 是上游链路的自然分层。 |
| `src/codex/serving` | Good | 对外 OpenAI-compatible 服务面与上游 gateway 分离。 |
| `src/platform` | Good | `crypto`、`http`、`identity`、`logging`、`storage` 都是跨领域能力。 |
| `src/config` | Good | 配置加载与类型独立，暂不需要进一步艺术化命名。 |
| `src/utils` | Acceptable | 当前只有 `json` 和 `pagination`，还没变成杂物间；后续如果扩张，应优先迁回领域目录。 |

## File Naming Audit

### Names To Keep

- `admin/api/accounts/lifecycle.rs`：覆盖状态、标签、删除、批量变更，比 `mutate.rs` 更准确。
- `admin/api/client_keys/*`：内部使用 `client_keys` 是正确选择，路由保留 `/api-keys` 是对外兼容。
- `codex/accounts/cloudflare_challenge.rs`：比 `cf_path_block.rs` 明确，表达真实安全链路问题。
- `codex/accounts/service/pool_sync.rs`：表达账号仓储与运行时池同步，比 `runtime_pool.rs` 更有边界。
- `codex/gateway/conversation_identity.rs`：比泛化 `identity.rs` 更准确。
- `codex/gateway/installation_id.rs`：表达 ChatGPT/Codex 安装标识，不混入一般 installation 概念。
- `codex/gateway/transport/http_client.rs`：比 `client.rs` 清楚，因为 transport 下还有 WebSocket。
- `codex/gateway/transport/usage_events.rs`：表达从上游事件流中提取 usage，而不是通用 usage 领域。
- `codex/tasks/{model_refresh,quota_refresh,token_refresh}.rs`：任务以动作命名，职责清晰。
- `platform/http/request_id.rs`：比 `middleware.rs` 精确。
- `platform/crypto/secret_box.rs`：表达加密封装能力，适合稳定复用。

### Names That Are Acceptable But Can Be Refined Later

- `src/codex/accounts/model.rs`：目前承载 `Account` 和 `AccountStatus`，与 `codex/models` 有轻微视觉冲突。若继续追求极致，可改成 `account.rs`，让 `accounts/account.rs` 与 `models/catalog.rs` 完全错开。
- `src/admin/api/client_keys/*` 内部 handler 名仍使用 `api_key`，例如 `create_api_key`。这是对外路由 `/api/admin/api-keys` 的兼容词，不是架构错误。若后续精修，可把 handler 改为 `create_client_key`，DTO 保留 `ClientApiKey*`。
- `tests/codex_events/logs_pagination.rs`：目录已经叫 `codex_events`，文件还叫 `logs_pagination`。如果按领域纯度，可改为 `event_log_pagination.rs`。
- `tests/admin/*_route.rs`：测试文件显式带 `route`，表达 HTTP 路由测试，可以保留。若未来测试分层更细，可迁到 `tests/admin/api/*`。
- `src/codex/gateway/transport/types.rs`：`types` 是弱名，但当前范围在 `transport` 下仍可接受；如果继续增长，应按 request/response/error 拆出更具体文件。

### Names To Avoid Reintroducing

- `admin/http`：会把管理 API 误导为协议工具层。
- `admin/auth`：同时包含 session 和 client key 时语义过宽。
- `codex/accounts/models`：会让 model catalog 看起来像账号私有数据。
- `codex/logs`：会混淆业务事件和进程日志。
- `platform/http/middleware.rs`：middleware 是技术位置，不是责任名。
- `gateway/identity.rs`：identity 太泛，会掩盖 conversation identity 的具体含义。
- `transport/client.rs`：在有 HTTP、SSE、WebSocket 多传输时过于笼统。

## Size And Organization Audit

命名已经过关，但少数文件仍偏大，后续可在不改变逻辑的前提下继续整理：

- `src/codex/serving/dispatch/mod.rs`：约 1000 行，是下一轮最值得拆的聚合点。建议按 request context、response assembly、fallback decision、event recording 继续切分。
- `src/codex/gateway/transport/http_client.rs`：约 780 行。可拆为 request building、response parsing、model fetching、probe 四块。
- `src/codex/accounts/repository/accounts.rs`：约 700 行。可按 load/list/write/status 或 token metadata 继续切分。
- `src/codex/accounts/pool.rs`、`src/codex/serving/dispatch/affinity.rs`、`src/codex/gateway/transport/websocket/*` 当前偏大但仍有领域凝聚力，暂不强拆。

这些不是命名阻断项。继续优化时应遵守“只拆大文件，不改逻辑”，每次拆分后跑 `cargo fmt --check`、`cargo test --locked`、`cargo clippy --all-targets --all-features --locked -- -D warnings`。

## Final Recommendation

当前架构可以定为：

```text
runtime -> web/admin/codex/platform/config/utils
admin   -> api/session/client_keys/tasks/settings
codex   -> accounts/models/gateway/serving/events/usage/tasks
platform-> crypto/http/identity/logging/storage
```

这是一个适合项目起步阶段的完整体：没有过度抽象，没有强行 workspace 化，也没有把 Web 前端塞进后端。后续前端可以独立演进，后端只保留 `src/web` 作为运行时挂载点。
