# Utils Module Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move cross-domain helpers out of crate root into `src/utils/` without changing runtime behavior.

**Architecture:** Keep the project as one Rust crate and tighten internal module boundaries. `utils` will hold reusable primitives that do not depend on HTTP, services, or domain modules; callers update to the new explicit paths.

**Tech Stack:** Rust, Cargo, Axum integration tests, `thiserror`, `serde_json`, `base64`, `aes-gcm`, `secrecy`.

---

### Task 1: Document The Utils Boundary

**Files:**
- Modify: `docs/architecture-reorganization.md`

- [x] **Step 1: Replace the target source layout with the accepted single-crate layout**

The architecture document should show `app/`, `config/`, `utils/`, `http/`, `service/`, and the existing domain modules.

- [x] **Step 2: Add boundary rules**

The document should state that `utils` may contain `pagination`, `json`, and `crypto`, and that it must not depend on `http`, `service`, `accounts`, `auth`, or `codex`.

### Task 2: Create `src/utils`

**Files:**
- Create: `src/utils/mod.rs`
- Create: `src/utils/pagination.rs`
- Create: `src/utils/crypto.rs`
- Create: `src/utils/json.rs`
- Modify: `src/lib.rs`
- Delete: `src/pagination.rs`
- Delete: `src/crypto.rs`

- [x] **Step 1: Add the utils module root**

```rust
pub mod crypto;
pub mod json;
pub mod pagination;
```

- [x] **Step 2: Move pagination helpers**

Move `Page<T>`, `encode_cursor`, `decode_cursor`, and `clamp_limit` into `src/utils/pagination.rs` unchanged.

- [x] **Step 3: Move crypto helpers**

Move `CryptoError`, `CryptoResult<T>`, and `SecretBox` into `src/utils/crypto.rs` unchanged.

- [x] **Step 4: Add JSON string helpers**

```rust
use serde_json::Value;

pub fn first_string(value: &Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| string_at(value, path))
}

pub fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
```

- [x] **Step 5: Expose `utils` from the library**

```rust
pub mod utils;
```

Remove the old top-level `pub mod crypto;` and `pub mod pagination;` declarations after all imports are updated.

### Task 3: Update Callers

**Files:**
- Modify: `src/codex/accounts/repository.rs`
- Modify: `src/auth/api_key_repository.rs`
- Modify: `src/codex/cookies/repository.rs`
- Modify: `src/http/admin/*.rs`
- Modify: `src/service/*.rs`
- Modify: `src/main.rs`
- Modify: `src/state.rs`
- Modify: `tests/*.rs`
- Modify: `tests/common/*.rs`

- [x] **Step 1: Update source imports**

Use `crate::utils::crypto::{CryptoError, SecretBox}` and
`crate::utils::pagination::{clamp_limit, decode_cursor, encode_cursor, Page}`.

- [x] **Step 2: Update integration test imports**

Use `codex_proxy_rs::utils::crypto::SecretBox` and
`codex_proxy_rs::utils::pagination::Page`.

- [x] **Step 3: Remove duplicate JSON helper functions**

Import `crate::utils::json::first_string` in `src/service/api_key_service.rs` and
`src/http/admin/accounts.rs`, then delete the local `first_string` and `string_at`
definitions.

### Task 4: Verify

**Files:**
- No source edits after this task unless verification fails.

- [x] **Step 1: Format**

Run:

```bash
cargo fmt
```

- [x] **Step 2: Run focused tests**

Run:

```bash
cargo test --test crypto_test --test account_repository_test --test admin_accounts_import_export_test --test admin_api_keys_route_test
```

- [x] **Step 3: Run full gate**

Run:

```bash
cargo fmt --check && cargo test && cargo clippy --all-targets --all-features --locked -- -D warnings
```

Expected: all commands exit successfully.
