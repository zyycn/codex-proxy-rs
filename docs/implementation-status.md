# Implementation Status

Update this file before each feature commit.

| Area | Status | Commit | Verification | Notes |
| --- | --- | --- | --- | --- |
| Scaffold | Completed | initial scaffold commit | `cargo fmt --check`; `cargo test`; `cargo clippy --all-targets --all-features --locked -- -D warnings` | Rust crate, lint policy, pinned TLS-sensitive dependencies, `.gitignore`, and minimal entry points are in place. |
| API contract docs | Completed | initial scaffold commit | `cargo fmt --check`; `cargo test`; `cargo clippy --all-targets --all-features --locked -- -D warnings` | `/v1/*` OpenAI-compatible body, `/admin/*` frontend envelope, body codes, camelCase, and request ID policy are documented. |
| Configuration | Completed | pending config commit | `cargo test default_config_keeps_only_codex_backend`; full `fmt/test/clippy` before commit | Defines Codex-only config, default YAML, and `Arc<AppServices>` state shell. |
| SQLite storage | Completed | pending storage commit | `cargo test migrations_create_accounts_and_event_tables`; full `fmt/test/clippy` before commit | Creates SQLite WAL connector, migrations for accounts/API keys/sessions/cookies/fingerprints/events, and event/account indexes. |
| Admin auth and client API keys | Completed | pending auth commit | `cargo test client_api_key_has_proxy_prefix_and_verifies_against_hash`; `cargo test admin_password_hash_is_not_a_client_api_key`; full `fmt/test/clippy` before commit | Admin passwords use Argon2id, client API keys use `cpr_` prefix plus HMAC-SHA256 with server-side pepper. |
| Secret encryption | Completed | pending crypto commit | `cargo test secret_box_encrypts_and_decrypts_without_storing_plaintext`; full `fmt/test/clippy` before commit | AES-256-GCM `SecretBox` stores upstream secrets as `v1:<nonce>:<ciphertext>` without plaintext. |
| Logging and pagination | Completed | pending logs commit | `cargo test event_logs_are_cursor_paginated`; full `fmt/test/clippy` before commit | Adds cursor pagination, `Page<T>` camelCase serialization, event log repository, and rotation config shell. |
| TLS headers and fingerprint | Completed | pending codex headers commit | `cargo test codex_headers_include_desktop_identity_and_turn_state`; `cargo tree \| rg 'reqwest\|rustls'`; full `fmt/test/clippy` before commit | Adds Codex Desktop fingerprint model, exact identity/request headers, and pinned reqwest/rustls client builder. |
| Cookie persistence | Completed | pending cookies commit | `cargo test cookie_jar_captures_and_replays_account_scoped_cookies`; full `fmt/test/clippy` before commit | Adds account-scoped in-memory Cookie jar and repository shell; database persistence follows storage integration. |
| Account pool and refresh | Completed | pending accounts commit | `cargo test account_pool_skips_expired_disabled_banned_and_quota_exhausted_accounts`; full `fmt/test/clippy` before commit | Adds account statuses, acquisition filter, lifecycle events, token pair, OAuth config, and refresh policy shell. |
| Translation | Completed | pending translation commit | `cargo test chat_completion_translates_to_codex_response_request`; full `fmt/test/clippy` before commit | Adds OpenAI Chat request types, Codex Responses request type, translation, and OpenAI-compatible error body helper. |
| HTTP routes | Completed | pending router commit | `cargo test v1_requires_client_api_key_not_admin_cookie`; full `fmt/test/clippy` before commit | Adds Axum router, health route, `/v1` unauthorized OpenAI error body, and binary listener. |
| Upstream lifecycle | Completed | pending upstream boundary commit | `cargo test responses_route_rejects_non_codex_provider_models`; `cargo test v1_requires_client_api_key_not_admin_cookie`; full `fmt/test/clippy` before commit | `/v1` accepts only `Bearer cpr_...`, rejects non-Codex provider models, and returns OpenAI-compatible errors. |
| Fingerprint updates | Completed | pending fingerprint updater commit | `cargo test update_manifest_updates_app_version_and_build_number`; full `fmt/test/clippy` before commit | Parses Codex Desktop update manifests into app version and build number updates. |
| Runtime docs and packaging | Planned |  |  |  |
