# Backend Naming Architecture Design

## Goal

将后端目录整理成一名一义的结构，为 `/admin` Web 控制台预留承载面，并把现有管理 JSON API 收束到 `/api/admin` 的后端契约层。

## Scope

本次设计只覆盖 Rust 后端目录、模块命名、路由边界和测试命名。前端工程由独立目录和独立约定承载，后端只提供 `src/web` 作为构建产物的挂载位置。

## Route Boundaries

```text
/admin/*              Web 控制台入口，由 src/web 承载
/api/admin/*          管理 API，由 src/admin/api 承载
/v1/*                 OpenAI-compatible API，由 src/codex/serving 承载
/auth/openai/callback OpenAI OAuth 回调
/debug/*              本地诊断
/health               存活检查
```

`admin` 是管理域，`web` 是浏览器承载面，`codex` 是 Codex 上游内核，`platform` 是跨领域系统能力。任何目录名都应表达唯一责任，避免 `http`、`auth`、`api_key` 这类多义名泄漏到领域边界。

## Target Source Tree

```text
src/
  main.rs
  lib.rs

  runtime/{mod.rs, bootstrap.rs, router.rs, state.rs}
  runtime/tasks/{mod.rs, coordinator.rs, types.rs}

  web/{mod.rs, router.rs, assets.rs, shell.rs, security.rs}

  admin/{mod.rs, settings.rs}
  admin/api/{mod.rs, router.rs, response.rs, session.rs, diagnostics.rs, settings.rs, models.rs, usage.rs}
  admin/api/accounts/{mod.rs, list.rs, create.rs, import.rs, export.rs, lifecycle.rs, quota.rs, cookies.rs, oauth.rs, health.rs}
  admin/api/client_keys/{mod.rs, list.rs, create.rs, import.rs, export.rs, lifecycle.rs}
  admin/api/logs/{mod.rs, query.rs, detail.rs, state.rs}
  admin/session/{mod.rs, service.rs, repository.rs}
  admin/client_keys/{mod.rs, service.rs}
  admin/tasks/{mod.rs, session_cleanup.rs}

  codex/{mod.rs}
  codex/accounts/{mod.rs, model.rs, jwt.rs, pool.rs, lifecycle.rs, cloudflare_challenge.rs, usage_snapshots.rs}
  codex/accounts/cookies/{mod.rs, jar.rs, repository.rs}
  codex/accounts/repository/{mod.rs, accounts.rs, tokens.rs, leases.rs, quotas.rs, usage.rs}
  codex/accounts/service/{mod.rs, cookies.rs, health.rs, import.rs, lifecycle.rs, quota.rs, refresh.rs, pool_sync.rs}

  codex/models/{mod.rs, catalog.rs, repository.rs, service.rs}

  codex/gateway/{mod.rs, conversation_identity.rs, installation_id.rs}
  codex/gateway/fingerprint/{mod.rs, model.rs, repository.rs, update_checker.rs, updater.rs}
  codex/gateway/oauth/{mod.rs, client.rs, codex_cli.rs, refresh.rs, token.rs}
  codex/gateway/protocol/{mod.rs, schema.rs, error.rs, openai_to_codex.rs, codex_to_openai.rs}
  codex/gateway/transport/{mod.rs, http_client.rs, headers.rs, rate_limits.rs, sse.rs, usage_events.rs}
  codex/gateway/transport/websocket/{mod.rs, codec.rs, pool.rs}

  codex/events/{mod.rs, event.rs, repository.rs, service.rs}
  codex/usage/{mod.rs, service.rs}
  codex/tasks/{mod.rs, model_refresh.rs, quota_refresh.rs, token_refresh.rs}

  codex/serving/{mod.rs, chat.rs, responses.rs, diagnostics.rs}
  codex/serving/http/{mod.rs, router.rs, auth.rs, chat.rs, responses.rs, models.rs, diagnostics.rs, errors.rs}
  codex/serving/dispatch/{mod.rs, affinity.rs, routing.rs, fallback.rs, limits.rs, account_refresh.rs, stream.rs, stream_audit.rs, usage.rs}

  platform/{mod.rs}
  platform/crypto/{mod.rs, secret_box.rs}
  platform/http/{mod.rs, auth.rs, health.rs, request_id.rs}
  platform/identity/{mod.rs, admin_session.rs, client_key.rs, client_key_repository.rs, error.rs}
  platform/storage/{mod.rs, db.rs, paths.rs, schema.sql}
  platform/logging/{mod.rs, rotation.rs}

  config/{mod.rs, loader.rs, types.rs}
  utils/{mod.rs, json.rs, pagination.rs}
```

## Naming Decisions

`admin/http` becomes `admin/api` because the module exposes the management API contract, not generic HTTP utilities.

`admin/auth` splits into `admin/session` and `admin/client_keys`. Admin login is session management; local `cpr_` keys are client credentials for `/v1/*`, not upstream provider keys.

`api_keys` becomes `client_keys` where the code manages local client credentials. The route can remain `/api/admin/api-keys` for product compatibility, but Rust modules should state the stronger domain meaning.

`codex/accounts/models` moves to `codex/models`. Model catalog and plan snapshots are Codex catalog concerns, not account-owned state.

`codex/logs` becomes `codex/events`. The SQLite event stream is a business event store used by the admin console; process file logging belongs under `platform/logging`.

`platform/http/middleware.rs` becomes `platform/http/request_id.rs` because it has one stable responsibility.

## Migration Policy

Implementation should be mechanical first: move files, update module declarations, update imports, then run formatting and tests. Business logic changes are out of scope unless required to preserve existing behavior under the new route boundary.

The migration must keep `/v1/*` OpenAI-compatible behavior unchanged and keep admin session auth separate from local client API keys.
