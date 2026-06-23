# OpenAI/Codex 真实链路测试记录

## 本轮上下文

- 开始时间：2026-06-23 19:32:16 Asia/Shanghai
- 目标：继续真实数据全链路测试，重点覆盖边界情况、封禁链路剥离、防止上下文污染、重试边界。
- 运行目录：`.runtime/real-chain-openai-20260623-193216`
- 规则：确认问题后先修复并复测，再继续后续场景。

## 场景矩阵

| 场景 | 状态 | 证据 |
| --- | --- | --- |
| 本地编译检查 | 通过 | `cargo check` |
| 封禁/禁用亲和剥离集成覆盖 | 通过 | `cargo test --test main proxy::dispatch::chat_upstream::responses_http:: -- --nocapture`；`responses_websocket::` |
| 5xx/429/403/402/Cloudflare 重试集成覆盖 | 通过 | `responses_recovery::` 37 个用例；`chat_routes::` 15 个用例；`responses_websocket::` 30 个用例 |
| 真实链路基础健康检查 | 通过 | `/debug/diagnostics`：5 accounts，1 active，3 disabled，1 banned |
| 真实 Responses 基线请求 | 通过 | `req_e29e505e-4623-4502-9676-922a27c1d247`，`resp_09b95050e9f8ff4b016a3a703bc2f8819a8e248aa2fc31ff4d` |
| 真实 previous_response_id 正常续接 | 通过 | `req_d8d28aed-6711-42fa-9ef5-8e1152d20bf3`，返回体保留 previous response |
| 真实 banned 账号亲和剥离 | 通过 | `req_e7d5625a-543b-4a64-9038-98e4afc84b57`，返回体 `previous_response_id=null` |
| 真实 disabled 账号亲和剥离 | 通过 | `req_9b52e5e8-eb57-471d-8825-0510a7e4017b`，返回体 `previous_response_id=null` |
| 真实 all accounts unavailable 边界 | 通过 | `req_63812d11-c9f9-4eac-afde-3d2837ba7754`，SSE `response.failed` |
| 真实 quota verify 边界 | 通过 | `req_faf18f0b-1e35-4dc5-8f47-632eb03d2c9e`，`quota_verify_required` 从 1 清回 0 |
| 误置 active 的 banned/disabled 账号状态回写 | 通过 | 发现并修复无有效 token exp 时只内存过期、不落库的问题 |
| 真实 5xx/429/Cloudflare 重试边界 | 受限 | 真实上游无法稳定强制触发，已由集成夹具覆盖 |

## 本地验证记录

- `cargo check`：通过。
- `cargo test --test main proxy::dispatch::chat_upstream::responses_recovery:: -- --nocapture`：37 passed，覆盖 Responses HTTP/SSE 的 401/402/403/429、Cloudflare challenge/path-block、5xx same-account retry、模型不支持 fallback、previous_response_id/函数调用/加密 replay 历史剥离。
- `cargo test --test main proxy::dispatch::chat_upstream::responses_websocket:: -- --nocapture`：30 passed，覆盖 WebSocket 连接复用、复用连接断开后 fresh retry、previous_response_id fallback、隐式 banned 亲和剥离、quota/rate limit/path-block 边界。
- `cargo test --test main proxy::dispatch::chat_upstream::chat_routes:: -- --nocapture`：15 passed，覆盖 Chat Completions 的 401/402/403/429、Cloudflare、模型不支持和 SSE 翻译。
- `cargo test --test main proxy::dispatch::chat_upstream::responses_http:: -- --nocapture`：23 passed，覆盖 HTTP Responses 参数转换、cookie path 作用域、banned 亲和剥离、quota 亲和保留和下游断开。
- `cargo test --test main upstream::accounts::token_refresh:: -- --nocapture`：13 passed，覆盖 transient retry、ambiguous transport 不重试、invalid_grant 二次确认、retry exhaustion 延后恢复、stale refresh token 防复用。
- `cargo test --test main upstream::accounts::account_pool::quota:: -- --nocapture`：9 passed，覆盖 quota/cooldown 选择边界。
- `cargo test --test main upstream::accounts::cloudflare:: -- --nocapture`：2 passed，覆盖 Cloudflare cooldown 升级和过期重置。
- `cargo test --test main mark_banned -- --nocapture`：5 passed，覆盖 401 `account_deactivated` 和 403 banned 信号将账号标记为 banned。
- `cargo test --test main mark_expired_after_401 -- --nocapture`：3 passed，覆盖普通 401 将账号标记为 expired。
- `cargo test --test main runtime_account_pool_should_persist_expired_status_when_jwt_expiry_is_discovered -- --nocapture`：通过，覆盖运行时调度发现 active 账号 token 已过期时持久化为 expired。
- `cargo test --test main upstream::accounts::account_pool:: -- --nocapture`：27 passed，覆盖账号池调度、quota/cooldown、运行时状态回写。

## 真实链路记录

### 环境

- 服务：`CODEX_PROXY_WS_AUDIT_DIR=.runtime/real-chain-openai-20260623-193216/ws-audit target/debug/codex-proxy-rs`
- 模型：`gpt-5.4-mini`
- `/v1/models`：HTTP 200，返回 `codex-auto-review`、`gpt-5.4-mini`、`gpt-5.5`。
- 初始账号池：5 total，1 active，3 disabled，1 banned。
- 结束账号池：5 total，1 active，3 disabled，1 banned；active 账号 `quota_verify_required=0`，`quota_cooldown_until=null`，`cloudflare_cooldown_until=null`。

### 结果

- 基线 Responses JSON：`req_e29e505e-4623-4502-9676-922a27c1d247`，HTTP 200，`response_id=resp_09b95050e9f8ff4b016a3a703bc2f8819a8e248aa2fc31ff4d`，输出 `EDGE-BASELINE-OK`，event log 记录 `transport=websocket`、`account_id=a18bcfa9ae932857`。
- 正常 previous_response_id 续接：`req_d8d28aed-6711-42fa-9ef5-8e1152d20bf3`，HTTP 200，输出 `EDGE-CONTINUE-OK`，返回体 `previous_response_id=resp_09b95050e9f8ff4b016a3a703bc2f8819a8e248aa2fc31ff4d`，prompt cache key 与基线一致。
- banned 亲和剥离：预置 `resp_real_edge_seed_banned_20260623 -> acct_be7f5c37f60b44ff8058b1b9b164fd42(banned)`，重启加载后请求 `req_e7d5625a-543b-4a64-9038-98e4afc84b57`，HTTP 200，输出 `EDGE-BANNED-STRIPPED-OK`，返回体 `previous_response_id=null`，event log 落到 active 账号 `a18bcfa9ae932857`。
- disabled 亲和剥离：预置 `resp_real_edge_seed_disabled_20260623 -> 10e584b410a5f1ab(disabled)`，重启加载后请求 `req_9b52e5e8-eb57-471d-8825-0510a7e4017b`，HTTP 200，输出 `EDGE-DISABLED-STRIPPED-OK`，返回体 `previous_response_id=null`，event log 落到 active 账号 `a18bcfa9ae932857`。
- 防止历史污染：两条 seed affinity 在剥离后已不存在；WS audit 中正常续接请求包含 `previous_response_id`，banned/disabled 剥离请求的上游 payload body 不包含 `previous_response_id` 或 `turn_state`。
- all accounts unavailable：临时通过 admin API 将唯一 active 账号置为 disabled，请求 `req_63812d11-c9f9-4eac-afde-3d2837ba7754`，HTTP 200 SSE，返回 `event: response.failed`，message 为 `No active upstream account is available`；随后通过 admin API 恢复 active。
- quota verify：停止服务后将 active 账号 `quota_verify_required=1`，重启后请求 `req_faf18f0b-1e35-4dc5-8f47-632eb03d2c9e`，HTTP 200，输出 `EDGE-QUOTA-VERIFY-OK`；请求后 DB 中 `quota_verify_required=0` 且 `quota_fetched_at=2026-06-23T11:44:04.888488741+00:00`。

### 误置 active 的禁用/封禁账号状态回写

- 测试方法：临时将原 active 账号 `a18bcfa9ae932857` 置为 disabled，逐个将目标 disabled/banned 账号置为 active，确保请求只会命中目标账号；请求后先读取 DB 状态，再恢复原状态。
- banned 账号 `acct_be7f5c37f60b44ff8058b1b9b164fd42`：请求 `req_116f4b30-c8fa-4718-a69a-ccd22b66d562`，真实上游返回 401 `token_invalidated`，请求后状态从 active 变为 expired。该响应不是 banned 信号，所以不会标记为 banned。
- disabled 账号 `64e49d697f15dd02`：请求 `req_c2514f79-e429-49c8-9635-2383a2cfe470`，真实上游返回 401 `token_invalidated`，请求后状态从 active 变为 expired。
- disabled 账号 `10e584b410a5f1ab` 和 `38907867f4c6c36e`：首次测试时响应为 503 `no_available_accounts`，状态仍为 active。原因是两者 `access_token_expires_at=null` 且 token 无有效 JWT exp，账号池在调度阶段内存判定 expired，没有持久化到 DB。
- 修复：`RuntimeAccountPoolService::acquire_with` 持久化账号池调度阶段发现的 runtime-expired 账号状态。
- 修复后复测：
  - `10e584b410a5f1ab`：请求 `req_25d53384-1165-4da5-91f9-918b1709f8bc`，HTTP 503 `no_available_accounts`，请求返回前 DB 状态已从 active 变为 expired。
  - `38907867f4c6c36e`：请求 `req_4b0181b9-a248-4402-ad7f-b533c8669907`，HTTP 503 `no_available_accounts`，请求返回前 DB 状态已从 active 变为 expired。
- 结论：真实打到上游并收到 401/403 时会按信号及时回写；不打上游、但运行时已经能判定 token 过期/无效时，修复后也会及时回写 expired。

### 未真实强制触发的边界

- 真实上游 5xx、429、Cloudflare challenge/path-block 不能稳定人工触发，本轮没有对真实 OpenAI 强行制造这些状态。
- 对应重试和状态传播已通过集成测试覆盖：HTTP/SSE 5xx same-account retry、429 fallback/exhausted、402 quota fallback/exhausted、403 banned fallback、Cloudflare challenge/path-block cooldown/清 cookie/三次 path-block disable，以及 WebSocket previous_response_id fallback、quota/rate limit/path-block stream error。
