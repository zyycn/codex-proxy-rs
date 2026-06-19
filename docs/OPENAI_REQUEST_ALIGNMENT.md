# OpenAI 请求链路一致性审查报告

本文档记录了 `codex-proxy-rs` (Rust) 与 `codex-proxy` (Node.js) 在 OpenAI 请求链路上的对比审查和修复工作。

## 审查日期
2026-06-19

## 当前结论

Rust 版已经补齐 OpenAI/Codex 主请求链路的核心路径：默认 WebSocket 上游、`previous_response_id` 账号亲和性、稳定 `prompt_cache_key`、关键安全 header、`client_metadata`、WS frame 到 SSE 的响应转换、usage/rate-limit 记录、HTTP SSE fallback、implicit resume、strip-and-retry、reasoning replay。

本轮改造后，直接影响 OpenAI 交互语义和请求发送行为的差异已补齐：自动 Cookie 捕获白名单、`Max-Age` 优先级、账号生命周期后的 WS pool 驱逐、`request_interval_ms` 发送前 stagger、`least_used` reset 缺失排序。

IP 代理/VPN、本地代理探测和 `HttpsProxyAgent` 不属于 Rust 版迁移目标。除此之外，当前审计没有保留的 OpenAI/Codex 请求链路迁移阻塞项。

## 本轮已对齐项

### 自动捕获 Cookie 策略

原版只允许自动捕获 `cf_clearance`，避免重放 `__cf_bm` 这类与 IP、UA、TLS 指纹和时序强绑定的 Bot Management session cookie。Rust 此前会自动持久化所有 `Set-Cookie`，包括 `__cf_bm`。

已完成：
- 自动 `Set-Cookie` 捕获只允许 `cf_clearance`
- 管理端手动注入 Cookie 不受白名单限制
- `Max-Age` 优先级高于 `Expires`

### WebSocket Pool 生命周期驱逐

原版在账号 refreshed、banned、disabled、rate-limited 等状态变化后会按账号驱逐 WS pool，避免旧 token、旧风控状态或旧后端粘滞连接继续复用。Rust 此前只有被动 rate-limit 和部分 fallback 状态会驱逐，账号刷新、管理端禁用/删除、批量状态更新等生命周期路径没有统一驱逐同一个共享 pool。

已完成：
- `AccountService` 和 `CodexUpstreamService` 共享同一个 `CodexWebSocketPool`
- refresh 成功后驱逐旧连接
- refresh 失败导致 Expired/QuotaExhausted/Banned/Disabled 后驱逐
- 管理端 disable/delete/batch lifecycle 后驱逐

### 请求间隔发送节流

原版在账号池返回 `prevSlotMs` 后，发送上游请求前会按 `request_interval_ms` 做 stagger。Rust 的 `AccountPool` 已返回 `previous_slot_at`，但此前服务层丢弃了该字段，实际发送上游请求前没有等待。

已完成：
- acquire 返回值保留 `previous_slot_at`
- 普通请求、stream 请求、fallback 账号请求在发 OpenAI/Codex 上游前统一执行 stagger
- 只延迟同账号连续请求，不影响账号选择

### `least_used` 中 `window_reset_at` 的比较语义

原版只有当两个账号都有 `window_reset_at` 且值不同时才比较 reset 时间；否则继续比较 `request_count` 和 LRU。Rust 此前把 `Some(window_reset_at)` 永远排在 `None` 前面，会让缺少 reset 数据但请求更少的账号被不合理降权。

已完成：
- `(Some(a), Some(b))` 且不相等时比较 reset
- 任一侧缺失 reset 时继续比较 `request_count`

## 历史未对齐项（已关闭）

### Responses 隐式续接和 reasoning replay 状态机

原版 shared handler 包含 implicit resume、strip-and-retry、reasoning replay 等更细的 Responses 状态机。这个缺口已经在当前 Rust workspace 中关闭：

- `crates/core/src/serving/implicit_resume.rs` 保存纯策略；
- `crates/core/src/serving/reasoning_replay.rs` 保存 replay 缓存策略；
- `crates/runtime/src/services.rs` 在 Responses 非流式、HTTP SSE 流式、WebSocket 路径中接入恢复、驱逐和历史还原；
- `crates/server/tests/openai_chat_upstream.rs` 覆盖 WebSocket implicit resume、SQLite 恢复后续接、跨 window 拒绝、自包含 function-call replay 拒绝、未匹配 function-call 输出拒绝、invalid encrypted reasoning replay 后驱逐、previous_response_not_found / unanswered_function_call strip-and-retry、SSE/WebSocket invalid reasoning replay strip-and-retry。

## 已修复的问题

#### 1. 缺失的关键 HTTP Headers

**问题：** Rust 项目缺少 Node.js 项目中的关键请求头
- `x-codex-installation-id` - 设备唯一标识
- `session_id` - 会话 ID（用于会话关联和提示缓存）

**修复：**
- 新增 `src/codex/gateway/installation.rs` 模块
  - 实现 Installation ID 管理逻辑
  - 优先读取 `~/.codex/installation_id`（兼容真实 Codex Desktop）
  - 次优从 `<database_dir>/installation_id` 读取或生成新 UUID
  - 使用 `OnceLock` 实现单例缓存

- 新增 `src/codex/gateway/identity.rs` 模块
  - 实现 `build_conversation_identity()` 函数
  - 从 `prompt_cache_key` 派生账号作用域的会话 ID
  - 使用 SHA256 哈希生成确定性标识符
  - 格式：`cp_{hash[..32]}` (conversation) 或 `cw_{hash[..32]}` (window)

- 更新 `src/codex/gateway/transport/http_client.rs`
  - 在 `CodexRequestContext` 添加 `installation_id` 和 `session_id` 字段
  - 在 `request_headers()` 方法中添加这两个 header

- 更新 `src/codex/serving/dispatch/mod.rs`
  - 在发送请求前构建 conversation identity
  - 获取 installation ID 并传递到请求上下文
  - 自动将派生的 `conversation_id` 设置为 `session_id` header

#### 2. 会话亲和性 (Session Affinity)

**Node.js 实现：**
```typescript
const identity = this.buildConversationIdentity(request);
if (identity.conversationId) {
  headers["session_id"] = identity.conversationId;
  headers["x-client-request-id"] = identity.conversationId;
}
```

**Rust 实现（已修复）：**
```rust
let identity = build_conversation_identity(
    request.prompt_cache_key.as_deref(),
    request.codex_window_id.as_deref(),
    account_scope,
);
// ...
session_id: identity.conversation_id.as_deref(),
```

## 对比矩阵

| 功能 | Node.js | Rust (修复后) | 状态 |
|------|---------|---------------|------|
| `x-codex-installation-id` | ✓ | ✓ | ✅ 一致 |
| `session_id` header | ✓ | ✓ | ✅ 一致 |
| Conversation identity 派生 | ✓ | ✓ | ✅ 一致 |
| SHA256 哈希算法 | ✓ | ✓ | ✅ 一致 |
| 账号作用域绑定 | ✓ | ✓ | ✅ 一致 |
| Window ID fallback | ✓ | ✓ | ✅ 一致 |
| Installation ID 持久化 | ✓ | ✓ | ✅ 一致 |
| `~/.codex/installation_id` 兼容 | ✓ | ✓ | ✅ 一致 |

## 已验证的一致性

### 请求头对比

**Node.js (`codex-api.ts:320-392`):**
```typescript
headers["x-codex-installation-id"] = installationId;
headers["session_id"] = identity.conversationId;
headers["x-codex-window-id"] = identity.windowId;
headers["OpenAI-Beta"] = "responses_websockets=2026-02-06";
headers["x-openai-internal-codex-residency"] = "us";
```

**Rust (`transport/client.rs:296-337`):**
```rust
insert_optional_header(&mut headers, "x-codex-installation-id", context.installation_id)?;
insert_optional_header(&mut headers, "session_id", context.session_id)?;
insert_optional_header(&mut headers, "x-codex-window-id", context.codex_window_id)?;
headers.insert(HeaderName::from_static("openai-beta"),
               HeaderValue::from_static("responses_websockets=2026-02-06"));
headers.insert(HeaderName::from_static("x-openai-internal-codex-residency"),
               HeaderValue::from_static("us"));
```

### 身份派生逻辑

**算法一致性：**
```
SHA256(kind + "\0" + account_scope + "\0" + client_value)[..32]
```

**前缀映射：**
- `"conversation"` → `"cp_"`
- `"window"` → `"cw_"`

**Fallback 行为：**
- 如果没有显式 `window_id`，则派生为 `{conversation_id}:0`

## 测试验证

所有新增测试通过：
```
test codex::gateway::identity::tests::test_build_account_scoped_identity_deterministic ... ok
test codex::gateway::identity::tests::test_build_account_scoped_identity_different_accounts ... ok
test codex::gateway::identity::tests::test_build_account_scoped_identity_window_prefix ... ok
test codex::gateway::identity::tests::test_build_conversation_identity_empty ... ok
test codex::gateway::identity::tests::test_build_conversation_identity_fallback_window ... ok
test codex::gateway::identity::tests::test_build_conversation_identity_full ... ok
test codex::gateway::installation::tests::test_read_from_codex_home ... ok
test codex::gateway::installation::tests::test_generate_and_persist ... ok
```

## 文件变更清单

### 新增文件
1. `src/codex/gateway/installation.rs` - Installation ID 管理
2. `src/codex/gateway/identity.rs` - Conversation identity 派生

### 修改文件
1. `src/codex/gateway/mod.rs` - 导出新模块
2. `src/codex/gateway/transport/http_client.rs` - 添加新 header 字段
3. `src/codex/serving/dispatch/mod.rs` - 集成身份派生逻辑
4. `src/codex/accounts/service/health.rs` - 更新 context 构造
5. `src/codex/models/service.rs` - 更新 context 构造
6. `src/codex/serving/diagnostics.rs` - 更新 context 构造
7. `Cargo.toml` - 添加 `dirs = "5.0.1"` 依赖

## 其他保持一致的部分

以下功能在审查前已经与 Node.js 项目一致：

### 1. 协议转换 (OpenAI ↔ Codex)
- ✅ System/developer messages → `instructions`
- ✅ User/assistant/tool messages → `input` array
- ✅ Tool calls → `function_call` items
- ✅ Reasoning effort 解析和映射
- ✅ Service tier 归一化
- ✅ Structured output (`json_schema`)

### 2. SSE 流处理
- ✅ Codex SSE events → OpenAI chat completion chunks
- ✅ `response.output_text.delta` → content delta
- ✅ `response.function_call_arguments.delta` → tool_calls delta
- ✅ `response.completed` → finish_reason + usage + `[DONE]`

### 3. 错误处理与重试
- ✅ 429 → 账号冷却 + 备用账号
- ✅ 401 → OAuth 刷新 + 重试
- ✅ 403 Cloudflare → 删除 cookies + 冷却
- ✅ 402 → 标记配额耗尽 + 备用账号

### 4. Cookie 管理
- ✅ 自动捕获 `Set-Cookie` 时只持久化 `cf_clearance`
- ✅ `Max-Age` 优先于 `Expires`
- ✅ 持久化到数据库
- ✅ 构建 `Cookie` header

### 5. WebSocket 支持
- ✅ `previous_response_id` 时自动使用 WebSocket
- ✅ 相同的 header 传递逻辑

## 修复进度

已完成：
1. ✅ 添加 `x-codex-installation-id` header
2. ✅ 添加 `session_id` header
3. ✅ 实现 conversation identity 派生逻辑
4. ✅ 实现 installation ID 持久化
5. ✅ 所有测试通过
6. ✅ 编译无错误
7. ✅ implicit resume / strip-and-retry / reasoning replay 状态机已迁移并由 crate-local 测试覆盖

## 下一步建议

1. **集成测试**：使用真实账号进行端到端测试，验证会话关联是否正常工作
2. **监控**：观察 `session_id` 和 `installation_id` 在日志中的出现，确保正确传递
3. **性能验证**：确认 prompt cache 命中率是否提升（session affinity 的主要效果）
4. **兼容性测试**：验证与真实 Codex Desktop 的 `~/.codex/installation_id` 共享是否正常

## 参考

- Node.js 实现：`/home/zyy/桌面/Codes/codex-proxy/src/proxy/codex-api.ts`
- Rust 实现：`/home/zyy/Codes/codex-proxy-rs/crates/{core,adapters,runtime,server}/`
- Installation ID 规范：Node.js `src/proxy/installation-id.ts`
- Identity 派生规范：Node.js `buildAccountScopedIdentity()` 方法
