# OpenAI 真实链路请求测试文档

日期：2026-06-20

本文档用于真实账号、真实 OpenAI/Codex 上游链路测试。它不是迁移记录，也不是单元测试说明；目标是把“客户端发起请求 -> 本项目选择账号和构造上游请求 -> OpenAI/Codex 返回 -> 本项目翻译响应和记录日志”这一整条链路跑通、可观察、可复盘。

## 本轮执行进度

### 运行上下文

- 执行时间：2026-06-20
- 执行目录：`/home/zyy/Codes/codex-proxy-rs`
- 文档维护方式：每完成一个阶段立即回写本文档
- 2026-06-20 19:xx 补充执行：继续补跑 `/v1/responses/compact` 真实成功链路，并同步复核管理端 / SQLite 证据
- 2026-06-20 20:5x 补充执行：继续补测 `previous_response_id` 强语义续链，并同步复核 SQLite / session affinity 证据

### 进度状态

| 阶段 | 状态 | 备注 |
| --- | --- | --- |
| 文档审阅与仓库核对 | 已完成 | 已确认 `/home/zyy/桌面/Codes/codex-proxy-rs` 与当前工作目录是同一路径 |
| 账号池与运行前检查 | 已完成 | 已确认 `active=1`、`disabled=3`；服务、管理端登录、临时 client key、日志清理均已完成 |
| 诊断与 Models | 已完成 | `/debug/diagnostics`、`/debug/fingerprint`、`/debug/upstream`、`/debug/models`、`/v1/models*` 已执行 |
| Responses HTTP/WS | 已完成 | HTTP/WS 已执行；已修复非流式 Responses success event 缺失 `response_id` 的问题；补充复核了 1 次 WebSocket fallback 与 1 次成功复验 |
| Chat / Review / Compact | 已完成 | Chat JSON/SSE、Review、Compact 已执行；已确认普通 `reasoning.encrypted_content` 不能充当 compact token，且已补齐 compact 真实成功链路 |
| 日志、SQLite、WS audit 复核 | 已完成 | 已复核管理端、SQLite、文件日志、WS audit；当前累计确认并修复 4 个问题 |
| 结论汇总 | 已完成 | 所有文档内场景已完成；`previous_response_id` 强语义续链补测已通过 |

### 当前已知事实

- 当前数据库中账号状态汇总：`active=1`、`disabled=3`
- 本轮运行目录：`.runtime/real-chain-openai-20260620T185310`
- 当前服务进程已重新启动，WebSocket audit 输出目录：`.runtime/real-chain-openai-20260620T185310/ws-audit`
- 当前 `.runtime/data/codex-proxy-rs.sqlite` 已存在，可用于复核账号、事件和用量
- `/debug/diagnostics`：`status=ok`，账号池 `total=4`、`active=1`、`disabled=3`
- `/debug/fingerprint`：当前运行时指纹 `originator=Codex Desktop`、`appVersion=26.616.41845`、`platform=darwin`、`userAgent=Codex Desktop/26.616.41845 (darwin; arm64)`
- `/debug/upstream`：请求 ID `b4d9f10c-6a75-4fde-a9d0-135144383e65`，`reachable=true`、`statusCode=401`、`authorization=rejected`
- `/debug/models` 已修复为“本地可直接访问 + remote forwarded 403”：
  - 本地复验请求 ID `f7acda14-c9ce-4d62-a0f8-191ea7443c41`：HTTP 200，返回 `totalModels=3`
  - 远端头复验请求 ID `cecae701-7af7-41e3-9fa2-4ae2a30065d8`：HTTP 403，`{"error":"debug endpoint is local-only"}`
- `/v1/models`：请求 ID `08e2e09d-9a4a-4f04-80a7-9a68a137231d`，HTTP 200，返回 3 个模型，包含 `gpt-5.5`
- `/v1/models/catalog`：请求 ID `8078cfc1-58c0-4366-a326-2ba734fb5231`，HTTP 200，返回 3 个模型完整目录
- `/v1/models/gpt-5.5`：请求 ID `58c0fa71-3571-4940-9d4c-1510941cbbe4`，HTTP 200，返回 OpenAI model object
- `/v1/models/gpt-5.5/info`：请求 ID `52940d68-46fe-45a5-8d0b-c478712d2233`，HTTP 200，返回 reasoning / context metadata
- `/v1/chat/completions`：
  - 非流式请求 ID `68e67afd-b3c0-42f0-b9fa-d79393654171`，HTTP 200，返回 OpenAI chat completion JSON
  - 流式请求 ID `0369e603-dfa2-4ff5-b122-e22924855aed`，HTTP 200，SSE 连续输出 `chat.completion.chunk` 并以 `[DONE]` 结束
- `/v1/responses/review`：请求 ID `084407a8-46db-4f06-a123-5c99063c8248`，HTTP 200，事件日志 route 保持 `/v1/responses/review`
- Responses WebSocket 非流式额外复核：
  - 请求 ID `9c4ec365-7290-46f0-a4ea-a4855b8a60f2`：实际降级为 `transport=http_sse`
  - 请求 ID `347aff87-201a-4422-b7db-2cad69acac2e`：真实走通 `transport=websocket`，且 `responseId` 已正确落库
- `previous_response_id` 强语义续链补测：
  - 种子请求 ID `f8712160-7d82-4437-94c6-d396b3d2cfc9`：HTTP 200，`id=resp_05e522fee6913a14016a368d87ac8081959106dadbab8a6b00`，输出 `READY`
  - 续链请求 ID `1af98a40-972f-4622-8915-135cfdca881e`：HTTP 200，`id=resp_05e522fee6913a14016a368d89c7d0819590bc328748fee837`，准确输出暗号 `azure-lantern-4821`
  - SQLite `event_logs` 两条均为 `route=/v1/responses`、`transport=websocket`、`status_code=200`
  - SQLite `session_affinities` 两个 response id 均绑定同一账号和同一 `conversation_id=prev-semantic-deep-1781960065`

### 已执行场景记录

| 场景 | 请求 ID | 结果 | 关键观察 |
| --- | --- | --- | --- |
| 场景 1：`GET /debug/diagnostics` | `req_0886c19f-d564-45e2-9455-4c7df828fd43` | 通过 | `status=ok`；账号池 `total=4 / active=1 / disabled=3` |
| 场景 1：`GET /debug/fingerprint` | `req_e80b52dc-c5b6-4e3c-9b77-4cac525ab365` | 通过 | 指纹来自当前运行时数据库；`originator=Codex Desktop` |
| 场景 1：`GET /debug/upstream` | `b4d9f10c-6a75-4fde-a9d0-135144383e65` | 通过 | 传输可达，`reachable=true`，空 token 被上游拒绝，`statusCode=401` |
| 场景 1：`GET /debug/models` 本地复验 | `f7acda14-c9ce-4d62-a0f8-191ea7443c41` | 通过 | 修复后无需 client key；HTTP 200；返回 `totalModels=3` |
| 场景 1：`GET /debug/models` remote forwarded 复验 | `cecae701-7af7-41e3-9fa2-4ae2a30065d8` | 通过 | HTTP 403；拒绝远端转发头访问 |
| 场景 1：`GET /v1/models` | `08e2e09d-9a4a-4f04-80a7-9a68a137231d` | 通过 | HTTP 200，返回 3 个模型，包含 `gpt-5.5` |
| 场景 1：`GET /v1/models/catalog` | `8078cfc1-58c0-4366-a326-2ba734fb5231` | 通过 | HTTP 200；返回 `codex-auto-review`、`gpt-5.4-mini`、`gpt-5.5` |
| 场景 1：`GET /v1/models/gpt-5.5` | `58c0fa71-3571-4940-9d4c-1510941cbbe4` | 通过 | HTTP 200；返回 OpenAI model object |
| 场景 1：`GET /v1/models/gpt-5.5/info` | `52940d68-46fe-45a5-8d0b-c478712d2233` | 通过 | HTTP 200；返回模型扩展信息 |
| 场景 2：Responses HTTP JSON | `577806d4-bec2-4513-8d39-08a17fc253d3` | 通过 | HTTP 200；`route=/v1/responses`；`transport=http_sse`；usage=`input 32 / output 65 / reasoning 37 / total 97` |
| 场景 3：Responses HTTP SSE | `9faabf38-04ac-4b30-b84d-093eb4721112` | 通过 | HTTP 200；SSE 含 `response.output_text.delta`、`response.completed`、`[DONE]`；`transport=http_sse`；usage=`input 35 / output 62 / reasoning 11 / total 97` |
| 场景 4：Responses WebSocket JSON | `b2838e65-d770-4e1b-b0e7-19ec79e16bd8` | 通过 | HTTP 200；`route=/v1/responses`；`transport=websocket`；usage=`input 33 / output 32 / reasoning 16 / total 65` |
| 场景 5：Responses WebSocket SSE | `41c618ef-45f7-47a8-8598-0cf1ffa9c6cf` | 通过 | HTTP 200；SSE 已返回 `response.output_text.delta`；管理端与 SQLite 记录 `transport=websocket`；已生成 2 个 WS audit 文件 |
| 场景 4 复核：Responses WebSocket JSON fallback | `9c4ec365-7290-46f0-a4ea-a4855b8a60f2` | 通过 | 最终 HTTP 200；事件日志 `transport=http_sse`；文件日志明确记录 WebSocket idle timeout 后降级 |
| 场景 4 复核：Responses WebSocket JSON success | `347aff87-201a-4422-b7db-2cad69acac2e` | 通过 | 最终 HTTP 200；事件日志与 SQLite 记录 `transport=websocket` 且 `responseId` 正常 |
| 场景 6：Responses 续链 | `471e8ff8-cdc9-48ec-9018-f51ae5437d95`、`23c00292-81c3-4347-a5b0-8d17f3c741e9`、`f8712160-7d82-4437-94c6-d396b3d2cfc9`、`1af98a40-972f-4622-8915-135cfdca881e` | 通过 | 已确认 `previous_response_id` 请求真实走 `transport=websocket`；WS audit 记录 `transport_mode=websocket_required`、`fallback_allowed=false`，且 payload 中 `previous_response_id` 已被红acted；新增暗号续链补测准确返回 `azure-lantern-4821`，确认语义上下文可复用 |
| 场景 7：Chat JSON | `68e67afd-b3c0-42f0-b9fa-d79393654171` | 通过 | HTTP 200；返回 OpenAI chat completion JSON；usage 已入库 |
| 场景 8：Chat SSE | `0369e603-dfa2-4ff5-b122-e22924855aed` | 通过 | HTTP 200；SSE 连续输出 `chat.completion.chunk`；结尾 `[DONE]` |
| 场景 9：Responses Review | `084407a8-46db-4f06-a123-5c99063c8248` | 通过 | HTTP 200；事件日志 `route=/v1/responses/review` 保持正确 |
| 场景 10：Responses Compact 第 1 次补跑 | `0b8d5618-7300-4e6e-9de1-7835472c678e` | 失败 | 将真实 `reasoning.encrypted_content` 作为 `type=compaction` 输入后，上游返回 `400 invalid_encrypted_content`；证明普通 reasoning token 不能直接充当 compact token |
| 场景 10：Responses Compact 4xx 复核 | `61e9f0bf-f52f-49f2-969d-4d3a489efb36` | 通过 | 修复后客户端已正确返回 HTTP 400；SQLite / 文件日志同步记录 `upstreamStatus=400` |
| 场景 10：Responses Compact 成功复核 | `194c0c57-3f1a-4abb-b501-4aa062669f2b` | 通过 | 直接使用真实文本输入即可返回 `response.compaction`；输出包含 `compaction_summary.encrypted_content`；事件日志 `route=/v1/responses/compact`、`compact=true`、usage 正常 |

### 中途观察

- Responses HTTP JSON 在首次测试时暴露了一条已确认问题：成功事件已写入管理端和 SQLite，但该条 `event_logs.response_id` 为空；客户端 body 中存在 `id=resp_0e6061ff9a9bd681016a367203cffc81959c4bbeeeb6c334e2`
- Responses HTTP SSE 成功事件已写入管理端和 SQLite，`response_id=resp_0848ed9d2a7f9595016a367208416481909cb074a53c4c884a`
- Responses WebSocket JSON 在首次测试时也存在相同问题：成功事件已写入管理端和 SQLite，但该条 `event_logs.response_id` 为空；客户端 body 中存在 `id=resp_03c42d1119ff8823016a367246f840819a96be25bc06b2707a`
- Responses WebSocket SSE 成功事件已写入管理端和 SQLite，`response_id=resp_0478ff82f70ef79c016a367249113c819992a2d486af76547c`
- Chat 非流式首次真实测试时也暴露了已确认问题：成功事件存在，但 `event_logs.response_id` 为空；后续已修复并复验
- `/debug/models` 首次真实测试在无 client key 下返回 `401 invalid_api_key`，且该路由未执行 local-only 检查；后续已确认为实现不一致并修复
- WebSocket preferred 非流式请求 `9c4ec365-7290-46f0-a4ea-a4855b8a60f2` 出现过一次可复现降级：文件日志记录 `websocket receive idle timeout after 20s`，最终 fallback 到 HTTP SSE
- 之后的 WebSocket 非流式复核请求 `347aff87-201a-4422-b7db-2cad69acac2e` 已确认可真实走通 `transport=websocket`，且 `responseId` 正常写入管理端和 SQLite
- WebSocket audit 目录当前已有 6 个 artifact；opening / payload 中的 `input`、`instructions`、`previous_response_id` 已确认被红acted
- `account_usage` 已对 active 账号累计请求与 token，用量账号 ID 为 `acct_be7f5c37f60b44ff8058b1b9b164fd42`
- 2026-06-20 真实补测请求 `6d72d0a3-f535-4170-9a85-69ecadbb9ff4` 已确认当前 Rust `/v1/responses` 返回体能携带真实 `reasoning.encrypted_content`
- 2026-06-20 真实补测请求 `0b8d5618-7300-4e6e-9de1-7835472c678e` 已确认 `/v1/responses/compact` 不接受上述 reasoning token 作为 `type=compaction.encrypted_content`
- 2026-06-20 真实补测请求 `194c0c57-3f1a-4abb-b501-4aa062669f2b` 已确认 `/v1/responses/compact` 当前真实成功链路可直接使用纯文本 `input`，返回 `object=response.compaction` 与真实 `compaction_summary.encrypted_content`
- 2026-06-20 真实补测请求 `f8712160-7d82-4437-94c6-d396b3d2cfc9` / `1af98a40-972f-4622-8915-135cfdca881e` 已确认 `previous_response_id` 强语义续链可用：第一轮要求记住暗号并返回 `READY`，第二轮只传 `previous_response_id` 追问时准确返回 `azure-lantern-4821`

### 已确认并修复的问题

#### 问题 1：非流式 Responses success event 未写入 `response_id`

- 现象：
  - 首轮真实链路里，非流式 `POST /v1/responses` HTTP/WS 都成功返回客户端 `body.id`
  - 但管理端 `/api/admin/logs.responseId` 与 SQLite `event_logs.response_id` 为空
- 影响：
  - 非流式成功请求无法完整满足“客户端响应、管理端事件、SQLite 通过 response id 串联”的验收要求
- 定位：
  - Rust 实现中，流式成功路径会写 `metadata.responseId`
  - 非流式成功路径 `record_response_event(...)` 只写了 `stream/transport/usage`，未写 `responseId`
  - TS 基线在 non-streaming success 路径会保留 `responseId`
- 修复：
  - 文件：`crates/runtime/src/services.rs`
  - 变更：在 `CollectedResponse::Completed(body)` 成功路径提取 `body["id"]` 并写入 `metadata.responseId`
  - 测试：`crates/server/tests/openai_chat_upstream/openai_usage_logging.rs`
  - 测试辅助同步补全：`crates/server/tests/openai_chat_upstream.rs`
- 回归验证：
  - 定向测试通过：`cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_use_imported_account_record_usage_cookie_and_event_log`
  - 真实链路复验请求 ID：`1896eea2-6e02-4772-80c2-c18f6922df7b`
  - 复验结果：
    - 客户端 `id=resp_0c59e65071bd5704016a36747a5370819ab8aef48b404d26a7`
  - 管理端 `responseId=resp_0c59e65071bd5704016a36747a5370819ab8aef48b404d26a7`
  - SQLite `event_logs.response_id=resp_0c59e65071bd5704016a36747a5370819ab8aef48b404d26a7`

#### 问题 2：非流式 Chat success event 未写入 `response_id`

- 现象：
  - `POST /v1/chat/completions` 非流式成功时，客户端返回 `chatcmpl-*`
  - 但管理端 `/api/admin/logs.responseId` 与 SQLite `event_logs.response_id` 为空
  - 同轮流式 Chat 成功事件 `responseId` 正常
- 影响：
  - Chat 非流式成功请求同样无法完整满足 `x-request-id + response_id` 串联要求
- 定位：
  - Rust Chat 非流式成功路径 `record_response_event(...)` 未写入 `responseId`
  - TS 基线非流式成功路径保留 `result.responseId`
- 修复：
  - 文件：`crates/runtime/src/services.rs`
  - 变更：在非流式 Chat 成功路径从最终 OpenAI chat body 提取 `id` 并写入 `metadata.responseId`
  - 测试：`crates/server/tests/openai_chat_upstream/openai_chat_routes.rs`
- 回归验证：
  - 定向测试通过：`cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_dispatch_to_codex_and_return_openai_response`
  - 真实链路复验请求 ID：`0b05bbef-040c-4f92-b570-903abe7371bc`
  - 复验结果：
    - 客户端 `id=chatcmpl-ebf7ff07190b40ebb37ca227bca14715`
    - 管理端 `responseId=chatcmpl-ebf7ff07190b40ebb37ca227bca14715`
    - SQLite `event_logs.response_id=chatcmpl-ebf7ff07190b40ebb37ca227bca14715`

#### 问题 3：`/debug/models` 访问语义与同组 debug 路由不一致

- 现象：
  - 首次无鉴权真实请求 `f4fa6e07-4d7d-49fa-afd4-0a953adac6ab` 返回 `401 invalid_api_key`
  - 代码同时显示该路由未执行 local-only 校验，这意味着它与 `/debug/diagnostics`、`/debug/fingerprint`、`/debug/upstream` 的安全语义不一致
- 影响：
  - 同组本地诊断入口需要不同访问方式，真实排查时不符合预期
  - 路由边界不自洽：本地请求被要求 client key，而带 client key 的远端请求理论上又能访问诊断信息
- 定位：
  - Rust 实现 `crates/server/src/openai_api/models.rs::debug_models` 复用了 client key 鉴权
  - TS 基线 `/debug/models` 不要求 client key
  - Rust 其余 `/debug/*` 已有 `local-only` 约束
- 修复决策：
  - 本轮不把它改成“远端持 key 可访问”，而是统一为“本地可直接访问 + remote forwarded 拒绝”
  - 这样与 Rust 现有 debug 组语义一致，也符合本文档对“本地诊断入口”的定义
- 修复：
  - 文件：`crates/server/src/openai_api/models.rs`
  - 变更：`/debug/models` 改用与其它 `/debug/*` 相同的 `is_local_debug_request(...)` 判定，不再要求 client key
  - 辅助导出：`crates/server/src/openai_api/diagnostics.rs`
  - 测试：`crates/server/tests/openai_diagnostics_routes.rs`
- 回归验证：
  - 定向测试通过：
    - `cargo test -p codex-proxy-server --test openai_diagnostics_routes`
    - `cargo test -p codex-proxy-server --test openai_models_auth`
  - 真实链路复验：
    - 本地请求 ID `f7acda14-c9ce-4d62-a0f8-191ea7443c41`：HTTP 200，返回 `{"aliasCount":0,"staticModels":3,"totalModels":3,...}`
    - 远端头请求 ID `cecae701-7af7-41e3-9fa2-4ae2a30065d8`：HTTP 403，返回 `{"error":"debug endpoint is local-only"}`

#### 问题 4：Compact 上游 4xx 被错误映射成客户端 502

- 现象：
  - 真实补测请求 `0b8d5618-7300-4e6e-9de1-7835472c678e` 使用真实 `reasoning.encrypted_content` 伪装成 compact 输入
  - 上游真实返回 `400 invalid_encrypted_content`
  - 但客户端 HTTP 状态为 `502`，与 TS 基线“保留真实 4xx/5xx”不一致
- 影响：
  - 客户端无法区分“输入无效”与“代理/上游网关故障”
  - Compact 真实失败链路的状态码语义失真，排障成本升高
- 定位：
  - Rust `crates/server/src/openai_api/responses.rs::response_dispatch_compact_error_response(...)`
  - `ResponseDispatchError::Upstream(_)` 分支固定返回 `502 Bad Gateway`
  - TS 基线 `routes/shared/proxy-error-handler.ts::toErrorStatus(...)` 会保留上游 4xx/5xx
- 修复：
  - 文件：`crates/runtime/src/services.rs`
  - 变更：为 `ResponseDispatchError` 增加 `http_status_code()`，统一暴露实际客户端状态
  - 文件：`crates/server/src/openai_api/responses.rs`
  - 变更：Compact 的 `Upstream` 分支改用 `ResponseDispatchError::http_status_code()`，不再固定 502
  - 测试：`crates/server/tests/openai_chat_upstream/openai_compact_routes.rs`
  - 回归验证：
  - 定向测试通过：`cargo test -p codex-proxy-server --test openai_chat_upstream openai_compact_routes`
  - 新增用例：`responses_compact_should_preserve_upstream_client_error_status`
  - 真实链路复验：
    - 修复前请求 ID `0b8d5618-7300-4e6e-9de1-7835472c678e`：客户端错误返回 HTTP `502`
    - 修复后请求 ID `61e9f0bf-f52f-49f2-969d-4d3a489efb36`：客户端正确返回 HTTP `400`
    - SQLite / 管理端事件同步保留 `status_code=400`、`metadata.upstreamStatus=400`

### 补充确认

- `previous_response_id` 真实上游续链已经完成强语义补测：
  - Rust WebSocket audit 已确认 payload 携带 `previous_response_id`
  - `transport_mode=websocket_required`
  - `fallback_allowed=false`
  - `prompt_cache_key/session_id` 构造方式与 TS 基线一致
- 早期弱语义追问证据：
  - 请求 ID `471e8ff8-cdc9-48ec-9018-f51ae5437d95`
  - 请求 ID `23c00292-81c3-4347-a5b0-8d17f3c741e9`
- 强语义补测证据：
  - 种子请求 ID `f8712160-7d82-4437-94c6-d396b3d2cfc9`，输出 `READY`
  - 续链请求 ID `1af98a40-972f-4622-8915-135cfdca881e`，准确输出暗号 `azure-lantern-4821`
  - 两条事件日志均为 `transport=websocket`、`status_code=200`
  - 两个 response id 在 `session_affinities` 中绑定同一账号和同一 conversation id
- 当前结论：
  - 代理传输层、账号亲和性、日志层和真实语义复用均已成立
  - `previous_response_id` 不再保留为待确认项

## 本轮结论

- 已完成真实链路验收的入口：
  - `/debug/diagnostics`
  - `/debug/fingerprint`
  - `/debug/upstream`
  - `/debug/models`
  - `/v1/models`
  - `/v1/models/catalog`
  - `/v1/models/{model_id}`
  - `/v1/models/{model_id}/info`
  - `/v1/responses` HTTP JSON / HTTP SSE / WebSocket JSON / WebSocket SSE
  - `/v1/chat/completions` JSON / SSE
  - `/v1/responses/review`
- 本轮确认并修复了 4 个真实链路问题：
  - 非流式 Responses success event 缺失 `response_id`
  - 非流式 Chat success event 缺失 `response_id`
  - `/debug/models` 访问语义与同组 debug 路由不一致
  - Compact 上游 4xx 被错误映射成客户端 502
- `/v1/responses/compact` 真实链路已补齐：
  - 首次补测确认普通 `reasoning.encrypted_content` 不能伪装为 compact token
  - 当前实现下，compact 成功链路可直接使用真实文本 `input`，返回 `response.compaction` 与 `compaction_summary.encrypted_content`
- `/v1/responses` 的 `previous_response_id` 真实语义续链已补测通过：
  - 新增暗号续链测试确认第二轮能准确复用第一轮上下文
  - SQLite `event_logs` 与 `session_affinities` 均可串联本次种子响应和续链响应
- 当前无剩余待确认项阻塞本文档验收

## 范围

本轮只覆盖 OpenAI / Codex 相关入口：

- `GET /v1/models`
- `GET /v1/models/catalog`
- `GET /v1/models/{model_id}`
- `GET /v1/models/{model_id}/info`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `POST /v1/responses/review`
- `POST /v1/responses/compact`
- 本地诊断入口：`/debug/models`、`/debug/diagnostics`、`/debug/fingerprint`、`/debug/upstream`
- 管理端观察入口：`/api/admin/diagnostics`、`/api/admin/accounts`、`/api/admin/logs`

不覆盖通用 proxy、VPN、非 OpenAI provider，也不做任何绕过上游风控的测试。这里的“风控”只指本项目必须正确处理的账号状态、请求指纹、quota、session affinity、Cloudflare challenge/path-block、token refresh 和错误恢复。

## 审计结论

当前 OpenAI 真实链路分成四层：

```text
客户端 OpenAI 兼容请求
  -> server/openai_api 做认证、解析、模型校验、OpenAI/Codex 协议转换
  -> runtime/services 做账号选择、quota 校验、session affinity、Cloudflare/token 状态处理
  -> adapters/codex 生成 ChatGPT/Codex 上游 HTTP/SSE 或 WebSocket 请求
  -> core/protocol 把 Codex SSE/WebSocket 事件翻译回 OpenAI Responses / Chat 响应
  -> runtime 写 event_logs、account_usage、session_affinity、reasoning_replay、cookies
```

真实测试必须同时验证三件事：

- 客户端入口是否按 OpenAI 兼容协议返回。
- 上游请求是否按数据库指纹、账号、session identity 和 request metadata 构造。
- 成功/失败结果是否可用 `x-request-id` 串起响应、管理端事件、SQLite 和文件日志。

## 源码锚点

| 责任 | 代码位置 | 真实测试关注点 |
| --- | --- | --- |
| OpenAI 路由 | `crates/server/src/openai_api/router.rs` | 所有 `/v1/*` 与 `/debug/*` 入口是否覆盖 |
| Responses handler | `crates/server/src/openai_api/responses.rs` | client key、模型校验、review/compact route、stream/error 返回 |
| Chat handler | `crates/server/src/openai_api/chat.rs` | Chat -> Codex Responses 转换、Chat SSE chunk 转换 |
| Responses 协议转换 | `crates/core/src/protocol/openai/responses.rs` | `use_websocket`、`previous_response_id`、metadata/header 字段提取 |
| Chat 协议转换 | `crates/core/src/protocol/openai/chat.rs` | Chat 强制 HTTP SSE、reasoning/service tier/model suffix |
| 传输选择 | `crates/core/src/serving/responses.rs` | HTTP SSE、WebSocket preferred、WebSocket required |
| 调度/风控 | `crates/runtime/src/services.rs` | account acquire、quota verify、session affinity、fallback、错误分类、event log |
| Token 刷新 | `crates/runtime/src/tasks/token_refresh.rs` | `next_refresh_at` 持久化、disabled/banned 跳过刷新 |
| 上游 HTTP/WS | `crates/adapters/src/codex/client.rs` | HTTP/SSE header、usage、compact、reqwest TLS 设置 |
| WebSocket 握手 | `crates/adapters/src/codex/websocket/connect.rs` | fork tungstenite 握手、header 投影、pool、turn-state |
| WebSocket audit | `crates/adapters/src/codex/websocket/opening.rs` | opening/payload 红acted artifact |
| 事件日志 | `crates/adapters/src/sqlite/events.rs` | `event_logs` 字段是否可排查 |
| 账号/用量入库 | `crates/adapters/src/sqlite/accounts.rs` | status、quota、usage、refresh 时间、导入更新 |

## 代码链路审计

### HTTP 入口

OpenAI 兼容路由定义在 `crates/server/src/openai_api/router.rs`：

```text
/v1/responses
/v1/responses/review
/v1/responses/compact
/v1/chat/completions
/v1/models
/v1/models/catalog
/v1/models/{model_id}
/v1/models/{model_id}/info
/debug/models
/debug/diagnostics
/debug/fingerprint
/debug/upstream
```

请求 ID 由 `x-request-id` 指定；未指定时服务生成 `req_<uuid>`。真实链路测试必须显式传 `x-request-id`，这样客户端响应、管理端事件、SQLite 和文件日志能串起来。

### 客户端认证

OpenAI 兼容入口只接受本项目客户端 API key：

```text
Authorization: Bearer <client_api_key>
```

不要把 OpenAI 官方 API Key 发给本项目；本项目上游认证使用已导入账号的 ChatGPT access token。

### Responses 链路

`POST /v1/responses` 流程：

```text
server/openai_api/responses.rs
  -> translate_response_to_codex
  -> runtime ResponseDispatchService
  -> account_pool.acquire_with
  -> verify_acquired_quota_if_required
  -> apply_cascading_ban_defense
  -> create_response_with_account
  -> adapters CodexBackendClient
  -> chatgpt.com/backend-api/codex/responses
  -> response_from_codex_sse / live stream
  -> event_logs + usage + session_affinity
```

传输选择规则在 `crates/core/src/serving/responses.rs`：

- `use_websocket: false` -> 强制 HTTP SSE。
- `previous_response_id` 存在 -> WebSocket required，不允许 HTTP SSE fallback。
- 其它 Responses 请求 -> WebSocket preferred，WebSocket 非上游状态错误时允许 fallback 到 HTTP SSE。

注意：如果请求体省略 `use_websocket`，默认不是“纯 HTTP”，而是 WebSocket preferred。测试 HTTP SSE 必须显式写 `"use_websocket": false`。

### Chat 链路

`POST /v1/chat/completions` 会翻译成 Codex Responses 请求：

```text
server/openai_api/chat.rs
  -> translate_chat_to_codex
  -> force_http_sse = true
  -> runtime ChatDispatchService 或 Responses stream
  -> chatgpt.com/backend-api/codex/responses
  -> OpenAI chat completion JSON/SSE
```

Chat 链路始终强制 HTTP SSE 上游，不走 WebSocket。`stream: true` 会把 Codex SSE 转换为 OpenAI `chat.completion.chunk` SSE。

### Review 链路

`POST /v1/responses/review` 与 `/v1/responses` 共用调度链路，但 server 层强制写入：

```text
client_metadata["x-openai-subagent"] = "review"
```

事件日志的 `route` 必须保留 `/v1/responses/review`，不能退化成 `/v1/responses`。

### Compact 链路

`POST /v1/responses/compact` 走独立上游：

```text
chatgpt.com/backend-api/codex/responses/compact
```

该入口不走 WebSocket。事件日志需要包含 `route=/v1/responses/compact`，失败 metadata 需要包含 `compact=true`。

### Models 链路

`GET /v1/models` 返回运行时模型目录，不是每次都打上游。真实上游可达性用：

```text
GET /debug/upstream
POST /api/admin/refresh-models
```

`/debug/upstream` 会用空 token 探测 `/codex/models?client_version=...` 的传输可达性，正常情况下可能返回 reachable 且 authorization 为 rejected。

## 上游请求构造

### HTTP/SSE 请求头

Responses HTTP/SSE 上游请求由 `CodexBackendClient::request_headers` 构造，关键头包括：

```text
authorization: Bearer <account access token>
chatgpt-account-id: <ChatGPT account id>
originator: Codex Desktop
user-agent: <runtime fingerprint UA>
content-type: application/json
accept: text/event-stream
openai-beta: responses_websockets=2026-02-06
x-openai-internal-codex-residency: us
x-client-request-id: <session_id or x-request-id>
x-codex-installation-id: <runtime installation id>
session_id: <account-scoped conversation id>
x-codex-window-id: <account-scoped window id>
x-codex-turn-state: <turn state, when present>
x-codex-turn-metadata: <metadata, when present>
x-codex-beta-features: <metadata, when present>
x-responsesapi-include-timing-metrics: <metadata, when present>
version: <client version, when present>
x-codex-parent-thread-id: <parent thread id, when present>
x-openai-subagent: <review/compact/memory_consolidation/collab_spawn, when present>
cookie: <captured cf_clearance, when present>
```

真实链路测试不要从客户端手动传这些上游头。客户端只传本项目需要的 `Authorization`、`Content-Type`、`x-request-id`，其它由服务按数据库 fingerprint、账号、session identity 和 request metadata 生成。

字段来源：

| 上游字段 | 来源 | 代码点 |
| --- | --- | --- |
| `authorization` | 当前调度账号的 access token | `CodexRequestContext.access_token` |
| `chatgpt-account-id` | 当前调度账号的 ChatGPT account id | `CodexRequestContext.account_id` |
| `originator`、`user-agent`、`sec-ch-ua`、默认 browser headers、header order | 数据库当前 fingerprint | `build_codex_base_headers` |
| `content-type`、`accept`、`openai-beta`、`x-openai-internal-codex-residency` | adapter 固定追加 | `request_headers` |
| `x-client-request-id` | `session_id` 优先，否则客户端 `x-request-id` | `request_headers` |
| `x-codex-installation-id` | 运行时 installation id | `Services.installation_id` |
| `session_id`、`x-codex-window-id`、`prompt_cache_key` | 账号作用域 conversation identity | `build_conversation_identity` / `response_upstream_request` |
| `x-codex-turn-state` | 请求字段或 session affinity 恢复 | `prepare_response_session` |
| `x-codex-turn-metadata`、`x-codex-beta-features`、`version`、`x-responsesapi-include-timing-metrics`、`x-codex-parent-thread-id` | OpenAI 请求直接字段优先，`client_metadata` 兜底 | `translate_response_to_codex` |
| `x-openai-subagent` | review 强制，或请求 metadata/header 合法值 | `review_responses` / `request_headers_for_http_response` |
| `cookie` | 当前账号可用于目标 path 的 `cf_clearance` | `CloudflareRecovery.cookie_header_for_request` |

TLS/HTTP client 当前由 `build_reqwest_client` 生成：`rustls`、`no_proxy`、连接池、keepalive、gzip/brotli/zstd/deflate。这里不是 OpenAI 官方 API Key 代理，也不使用系统代理链路。

### WebSocket 请求

WebSocket 使用 OpenAI fork 的 `tokio-tungstenite` 完整握手，不手写 raw opening。关键点：

- endpoint：`/backend-api/codex/responses`
- payload：`type=response.create`
- deflate：`WebSocketConfig.extensions.permessage_deflate`
- idle timeout：20 秒
- pool：按 backend/account/conversation 复用，默认开启
- audit：设置 `CODEX_PROXY_WS_AUDIT_DIR` 后会输出 opening/payload 红acted 快照

WebSocket header 从 HTTP/SSE header map 投影，过滤 `content-type` 和 `accept`，其它业务/指纹头保持 TS 语义。

### Usage / quota 请求

当账号 `quota_verify_required=true` 时，业务请求发出前会先拉 usage：

```text
/wham/usage
/codex/usage
```

校验仍限流时会排除该账号并重新 acquire，单个用户请求最多 5 次 quota verification。usage 请求同样携带账号 token、`chatgpt-account-id`、fingerprint 基础头和可用 cookie。

## 风控检查点

### 请求发出前

每个真实业务请求发出前必须满足：

- 账号状态是 `active`。
- 账号不在 `quota_cooldown_until` 或 `cloudflare_cooldown_until` 内。
- 账号并发槽位未超过 `auth.max_concurrent_per_account`。
- 同账号请求间隔遵守 `auth.request_interval_ms`。
- 如果账号标记 `quota_verify_required=true`，先用 `/usage` 验证。
- 如果有 `previous_response_id`，优先按 session affinity 选择原账号。
- 如果原账号已 `banned` 或 `disabled` 且本次降级到其它账号，发送前剥离 `previous_response_id` 和 `turn_state`，避免封禁关联扩散。
- `prompt_cache_key` 和 `x-codex-window-id` 必须按账号作用域重写，不能跨账号复用原始客户端值。

### 启动和刷新调度

Token refresh 不属于单次 OpenAI 请求体，但会直接影响真实链路是否稳定：

- `disabled` / `banned` 账号在后台刷新扫描和启动后的账号定时器调度里必须被跳过。
- `next_refresh_at` 是持久化字段；刷新成功后按 access token 过期时间减 `refresh_margin_seconds` 写入，下次启动必须沿用。
- 当前时间早于 `next_refresh_at` 时，即使服务重启也不应提前刷新。
- access token 在请求链路被上游判定 invalid/revoked 时，账号先落 `expired`，让 refresh token 有自救机会。
- refresh token 自身被确认永久失败后，账号落 `disabled` 或 `banned`，同时清空 `next_refresh_at`，后续不再调度。
- refresh 传输失败不是永久失败；账号保持可恢复状态，并写入 recovery 时间。

### 上游错误分类

真实链路遇到错误时按以下规则验收：

| 上游信号 | 本项目行为 | 客户端状态 |
| --- | --- | --- |
| `401 token_invalid/token_revoked` | 账号先落 `expired`，允许 refresh token 自救 | 401 或 fallback 后成功 |
| refresh `invalid_grant/invalid_token/access_denied` | refresh token 确认不可用后落 `disabled`，不再调度刷新 | 管理端可见 disabled |
| `account_deactivated/banned` | 账号落 `banned` | 保留上游触发状态，401 deactivated 不强行改 403 |
| 402/quota | 账号落 quota exhausted 或进入 quota cooldown | 402 或 fallback 后成功 |
| 429/rate limit | 写 cooldown，排除账号重试 | 429 或 fallback 后成功 |
| 5xx | 同账号最多重试 2 次，再按错误处理 | 502 或 fallback 后成功 |
| Cloudflare challenge | 删除账号 cookie，写 10/30/90/120 秒递增 cooldown | 502 或 fallback 后成功 |
| Cloudflare 空 body 404 path-block | 删除 cookie，计数，3 次后 disable | 502 或 fallback 后成功 |
| `model_not_supported` | 排除账号并尝试其它账号，耗尽后 400 | 400 |
| `previous_response_not_found` / unanswered function call / invalid encrypted reasoning | 同账号剥离历史后重试一次 | 成功或记录恢复失败 |

### 响应返回后

响应解析和落库路径：

```text
Codex HTTP/SSE 或 WebSocket event
  -> adapters/codex/client.rs 返回 body/stream、transport、usage、turn_state、set-cookie、rate-limit headers
  -> core/protocol/openai/responses.rs::response_from_codex_sse 生成 OpenAI Responses JSON
  -> server/openai_api/chat.rs::ChatCompletionStreamTranslator 生成 OpenAI Chat SSE
  -> runtime/services.rs 记录 usage、session affinity、reasoning replay、Cloudflare recovery、event_logs
```

字段来源：

| 结果字段 | 来源 | 验收方式 |
| --- | --- | --- |
| `transport` | adapter 实际成功传输 | `event_logs.transport` / `metadata.transport` |
| `usage` | Codex SSE `response.completed.usage` 和 `tool_usage.image_gen` | `account_usage` 累加 |
| `reasoning_tokens` / `total_tokens` | `usage.output_tokens_details.reasoning_tokens` / `usage.total_tokens` | `account_usage` 查询 |
| `turn_state` | 上游响应 header 或 WebSocket turn update | 后续请求是否带上 `x-codex-turn-state` |
| `cf_clearance` | 上游 `Set-Cookie` | `account_cookies` 中只应持久化允许 cookie |
| `rate-limit headers` | 上游响应头 | `event_logs.metadata` 和账号 quota/window 状态 |
| `session_affinity` | `response.completed.id` + `prompt_cache_key` + account id | 续链请求是否回到原账号 |
| `reasoning_replay` | completed response 中可 replay 的 reasoning item | previous/implicit resume 是否能恢复 |

成功响应必须检查：

- `event_logs.level=info`
- `route` 是真实入口，不写死
- `transport` 是 `http_sse` 或 `websocket`
- `statusCode=200`
- usage 写入 `account_usage`，包含 `input_tokens`、`output_tokens`、`reasoning_tokens`、`total_tokens`
- rate-limit headers 被保存到事件 metadata，并被动同步账号 quota 状态
- `Set-Cookie` 中只自动持久化 `cf_clearance`
- Responses 成功后记录 session affinity 和 reasoning replay

失败响应必须检查：

- `event_logs.level=error`
- `failureClass` 可过滤
- `upstreamStatusCode` 或 `metadata.upstreamStatus` 尽量保留
- `metadata.upstreamBody` / `metadata.upstreamError` 保留上游真实原因，但不暴露 token
- 账号状态、cooldown、cookie 清理和 refresh 调度符合上面的分类

## 测试准备

### 启动服务

建议每次真实链路测试建立独立运行目录，只保存 headers/body/log 摘要，不保存 token：

```bash
export BASE=http://127.0.0.1:8080
export RUN_ID=$(date +%Y%m%dT%H%M%S)
export RUN_DIR=.runtime/real-chain-openai-$RUN_ID
export ADMIN_COOKIE=$RUN_DIR/admin.cookie
mkdir -p "$RUN_DIR/ws-audit"
```

启动服务时开启 WebSocket audit：

```bash
CODEX_PROXY_WS_AUDIT_DIR="$RUN_DIR/ws-audit" target/debug/codex-proxy-server
```

如果还没有构建：

```bash
cargo build -p codex-proxy-server
```

### 管理端登录

```bash
curl -sS -c "$ADMIN_COOKIE" \
  -H 'content-type: application/json' \
  -d '{"username":"admin","password":"admin"}' \
  "$BASE/api/admin/login" | jq .
```

### 创建客户端 key

```bash
export CLIENT_KEY=$(
  curl -sS -b "$ADMIN_COOKIE" \
    -H 'content-type: application/json' \
    -d "{\"name\":\"real-chain-$RUN_ID\"}" \
    "$BASE/api/admin/api-keys" \
  | jq -r '.data.plaintext'
)
```

不要把 `CLIENT_KEY` 写进文档或日志；只存在当前 shell 环境。

### 导入账号

导入 JSON 文件：

```bash
curl -sS -b "$ADMIN_COOKIE" \
  -H 'content-type: application/json' \
  --data-binary @/path/to/accounts.json \
  "$BASE/api/admin/accounts/import" | jq .
```

从 Codex CLI 导入：

```bash
curl -sS -b "$ADMIN_COOKIE" \
  -H 'content-type: application/json' \
  -d '{"codexHome":"/home/zyy/.codex"}' \
  "$BASE/api/admin/accounts/import-cli" | jq .
```

导入后先看账号池：

```bash
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/diagnostics" \
  | jq '.data.accounts.pool, .data.accounts.capacity'

curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/accounts?limit=50" \
  | jq '.data[] | {id,status,email,accountId,userId,planType,accessTokenExpiresAt}'
```

最低准入：

- 至少 1 个 `active`。
- `disabled` / `banned` 不应在启动后进入刷新调度。
- `expired` 且有 refresh token 的账号可以等待 recovery，但不要作为成功链路前置条件。

### 清理事件日志

每轮测试前可以清空管理端事件日志：

```bash
curl -sS -X DELETE -b "$ADMIN_COOKIE" "$BASE/api/admin/logs" | jq .
```

## 通用执行模板

每个请求都生成新的 UUID，不要使用带 `test`、`fake`、`mock` 的请求 ID 或正文。

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  ...

curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?requestId=$REQ&limit=20" \
  | jq '.data[] | {kind,level,route,model,statusCode,transport,failureClass,responseId,latencyMs,message,metadata}'
```

SSE 请求使用：

```bash
curl -sS -N --max-time 180 \
  -D "$RUN_DIR/$REQ.headers" \
  -o "$RUN_DIR/$REQ.body" \
  ...
```

SQLite 快速核对：

```bash
sqlite3 .runtime/data/codex-proxy-rs.sqlite \
  "select request_id, route, model, status_code, transport, failure_class, response_id from event_logs where request_id='$REQ';"
```

### 查询字段口径

管理端 `/api/admin/logs` 查询参数使用 camelCase；SQLite 表字段使用 snake_case。真实排查时不要混用：

| 语义 | 管理端查询参数 | SQLite 字段 |
| --- | --- | --- |
| 请求 ID | `requestId` | `request_id` |
| 状态码 | `statusCode` | `status_code` |
| 上游状态码 | `upstreamStatusCode` | `upstream_status_code` |
| 失败分类 | `failureClass` | `failure_class` |
| 响应 ID | `responseId` | `response_id` |
| 上游请求 ID | `upstreamRequestId` | `upstream_request_id` |
| 尝试序号 | `attemptIndex` | `attempt_index` |

日志响应体是管理端分页信封：

```text
{ code, message, data: [...], page, requestId }
```

创建客户端 key 的明文只返回一次，路径是 `.data.plaintext`。

## 场景 1：诊断和上游可达

### Runtime diagnostics

```bash
curl -sS "$BASE/debug/diagnostics" | jq .
curl -sS "$BASE/debug/fingerprint" | jq .
curl -sS "$BASE/debug/models" | jq .
curl -sS -H "x-request-id: $(uuidgen)" "$BASE/debug/upstream" | jq .
```

验收：

- `debug/diagnostics.status=ok`
- fingerprint UA、版本、平台来自数据库当前指纹
- `debug/models` 本地访问应返回 200；带远端转发头时应返回 403
- `debug/upstream.reachable=true` 表示传输可达；空 token 下 `authorization=rejected` 是可接受结果

### Models

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "x-request-id: $REQ" \
  "$BASE/v1/models"

jq '.data[].id' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- 返回包含 `gpt-5.5`
- 该入口不要求每次都产生上游 event log

## 场景 2：Responses HTTP JSON

强制 HTTP SSE 上游，非流式返回 OpenAI Responses JSON：

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "instructions": "请自然简洁地回答用户的问题。",
    "input": "请用一句中文说明当前工程请求链路的状态。",
    "stream": false,
    "use_websocket": false
  }' \
  "$BASE/v1/responses"

jq '{id,status,output_text,usage}' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- body 有 `id` 和 usage
- 日志 `route=/v1/responses`
- 日志 `transport=http_sse`
- `account_usage` 对应账号累加

## 场景 3：Responses HTTP SSE

```bash
REQ=$(uuidgen)
curl -sS -N --max-time 180 \
  -D "$RUN_DIR/$REQ.headers" \
  -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "instructions": "请自然简洁地回答用户的问题。",
    "input": "请用两句话描述一次请求从发出到收到响应的过程。",
    "stream": true,
    "use_websocket": false
  }' \
  "$BASE/v1/responses"

grep -E 'response.output_text.delta|response.completed|\[DONE\]' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- SSE 有 `response.output_text.delta`
- SSE 有 `response.completed`
- 结尾有 `[DONE]`
- 日志 `stream=true`
- 日志 `transport=http_sse`

## 场景 4：Responses WebSocket JSON

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "instructions": "请自然简洁地回答用户的问题。",
    "input": "请用一句中文说明 WebSocket 链路已经连通。",
    "stream": false,
    "use_websocket": true,
    "prompt_cache_key": "daily-engineering-session"
  }' \
  "$BASE/v1/responses"

jq '{id,status,output_text,usage}' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- 日志 `transport=websocket`
- 如果日志为 `http_sse`，需要检查文件日志是否出现 WebSocket fallback；fallback 可接受但必须记录原因
- `$RUN_DIR/ws-audit` 下应有 WebSocket audit artifact
- audit payload 不应包含用户原文

## 场景 5：Responses WebSocket SSE

```bash
REQ=$(uuidgen)
curl -sS -N --max-time 180 \
  -D "$RUN_DIR/$REQ.headers" \
  -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "instructions": "请自然简洁地回答用户的问题。",
    "input": "请用两句话描述 WebSocket 流式响应的用途。",
    "stream": true,
    "use_websocket": true,
    "prompt_cache_key": "daily-engineering-session"
  }' \
  "$BASE/v1/responses"

grep -E 'response.output_text.delta|response.completed|\[DONE\]' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- `metadata.transport=websocket`
- live stream 失败时必须有 `event_logs.level=error`
- 失败 metadata 至少包含 `transport`、`failureClass`、`upstreamStatus` 或 `upstreamError`

## 场景 6：Responses 续链

先从场景 4 或场景 2 取 response id：

```bash
PREV_RESPONSE_ID=$(jq -r '.id // empty' "$RUN_DIR/$REQ.body")
```

再发续链请求：

```bash
REQ=$(uuidgen)
jq -n --arg prev "$PREV_RESPONSE_ID" '{
  model: "gpt-5.5",
  instructions: "请自然简洁地回答用户的问题。",
  input: "请基于上一轮继续补充一句说明。",
  stream: false,
  previous_response_id: $prev,
  prompt_cache_key: "daily-engineering-session"
}' > "$RUN_DIR/$REQ.request.json"

curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  --data-binary @"$RUN_DIR/$REQ.request.json" \
  "$BASE/v1/responses"
```

验收：

- previous_response 链路必须走 WebSocket required
- 不允许因为 WebSocket 失败静默 fallback 到 HTTP SSE
- 如果 preferred 账号不可用且是 `disabled/banned`，应剥离历史并记录 session affinity 处理
- 如果是普通 quota rotation，不应预剥离历史

## 场景 7：Chat JSON

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "messages": [
      {"role": "system", "content": "请自然简洁地回答用户的问题。"},
      {"role": "user", "content": "请用一句中文说明聊天接口的请求链路。"}
    ],
    "stream": false
  }' \
  "$BASE/v1/chat/completions"

jq '{id,object,model,choices,usage}' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- `object=chat.completion`
- 日志 `route=/v1/chat/completions`
- 日志 `transport=http_sse`

## 场景 8：Chat SSE

```bash
REQ=$(uuidgen)
curl -sS -N --max-time 180 \
  -D "$RUN_DIR/$REQ.headers" \
  -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "messages": [
      {"role": "system", "content": "请自然简洁地回答用户的问题。"},
      {"role": "user", "content": "请用两句话说明流式聊天响应。"}
    ],
    "stream": true
  }' \
  "$BASE/v1/chat/completions"

grep -E 'chat.completion.chunk|\[DONE\]|stream_error' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- SSE body 是 OpenAI chat chunk 形态
- 正常结束包含 `[DONE]`
- 出错时仍是 SSE error frame，且管理端有错误事件

## 场景 9：Review

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "instructions": "请以审阅者视角给出简短判断。",
    "input": "这段工程说明是否清晰：请求链路需要覆盖发起、上游响应和日志排查。",
    "stream": false,
    "use_websocket": false
  }' \
  "$BASE/v1/responses/review"

jq '{id,status,output_text,usage}' "$RUN_DIR/$REQ.body"
```

验收：

- HTTP 200
- 日志 `route=/v1/responses/review`
- 上游请求 metadata/header 语义包含 `x-openai-subagent=review`
- 不能把 review 事件记成普通 `/v1/responses`

## 场景 10：Compact

已确认当前真实成功链路不要求先提供 `type=compaction` 的前序 token。直接使用纯文本 `input` 即可返回 `response.compaction`。另已确认：普通 `/v1/responses` 返回体里的 `reasoning.encrypted_content` 不能替代 compact 输入；若将其伪装成 `{"type":"compaction"}` 提交，上游会返回 `400 invalid_encrypted_content`。

请求模板：

```bash
REQ=$(uuidgen)
curl -sS -D "$RUN_DIR/$REQ.headers" -o "$RUN_DIR/$REQ.body" \
  -H "authorization: Bearer $CLIENT_KEY" \
  -H "content-type: application/json" \
  -H "x-request-id: $REQ" \
  -d '{
    "model": "gpt-5.5",
    "input": "请把下面的上下文压缩为三条要点：账号状态需要持久化；请求失败要分类；刷新调度不能反复消耗无效凭据。",
    "stream": false,
    "use_websocket": false,
    "max_output_tokens": 160
  }' \
  "$BASE/v1/responses/compact"
```

验收：

- HTTP 200 时应返回 compaction 相关 output 和 usage
- 响应体 `object=response.compaction`，且 output 中包含 `type=compaction_summary`
- 日志 `route=/v1/responses/compact`
- 日志 metadata `compact=true`
- 失败时 `failureClass` 和 `upstreamError` 必须保留真实原因

## 失败链路专项

这些场景不要求主动制造上游风控，只在真实遇到时按下面标准记录。

### Token invalid

观察点：

```bash
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/accounts?limit=50" \
  | jq '.data[] | {id,status,accessTokenExpiresAt}'
```

验收：

- 请求链路遇到 access token invalid 时，账号先变为 `expired`
- refresh token 确认 invalid 后，账号变为 `disabled`
- 重启后 disabled 账号不进入 refresh 调度

### Quota / rate limit

查询：

```bash
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?failureClass=rate_limited&limit=20" | jq .
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?failureClass=quota_exhausted&limit=20" | jq .
```

验收：

- rate-limit 有 `upstreamStatus=429` 或 `retryAfterSeconds`
- quota exhausted 有 `statusCode=402`
- 账号 cooldown 或 quota 状态入库
- 后续请求不应立即再次选择 cooldown 账号

### Cloudflare challenge / path-block

查询：

```bash
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?failureClass=cloudflare_challenge&limit=20" | jq .
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?failureClass=cloudflare_path_block&limit=20" | jq .
```

验收：

- challenge 后删除该账号 cookie
- cooldown 递增 10/30/90/120 秒
- path-block 空 404 累计 3 次后 disabled
- 成功响应后 recovery 状态清理

### WebSocket fallback

查询：

```bash
grep -R "falling back to HTTP SSE\\|websocket request failed" .runtime/logs || true
curl -sS -b "$ADMIN_COOKIE" "$BASE/api/admin/logs?transport=websocket&level=error&limit=20" | jq .
```

验收：

- WebSocket preferred 可以 fallback，但必须能从文件日志或事件日志看到原因
- WebSocket required 不允许 fallback
- WebSocket audit artifact 可用于对比 opening header 和 payload keys

## 每轮结束审计

### 汇总事件

```bash
sqlite3 .runtime/data/codex-proxy-rs.sqlite "
select route, model, status_code, transport, failure_class, count(*)
from event_logs
group by route, model, status_code, transport, failure_class
order by route, status_code, transport;
"
```

### 汇总账号

```bash
sqlite3 .runtime/data/codex-proxy-rs.sqlite "
select status, count(*) from accounts group by status order by status;
"
```

### 汇总用量

```bash
sqlite3 .runtime/data/codex-proxy-rs.sqlite "
select
  count(*) as accounts,
  sum(request_count) as requests,
  sum(input_tokens) as input_tokens,
  sum(output_tokens) as output_tokens,
  sum(reasoning_tokens) as reasoning_tokens,
  sum(total_tokens) as total_tokens
from account_usage;
"
```

### 必须保存的证据

每个真实请求至少保存：

- request id
- endpoint
- HTTP status
- body 摘要，不保存 token
- event_logs 查询结果
- 账号状态变化
- WebSocket audit artifact，只有 WebSocket 场景需要
- 如有失败，保留文件日志中的同 request id 片段

## 通过标准

一轮真实链路验证通过需要满足：

- `gpt-5.5` 下 `/v1/models`、Responses HTTP JSON、Responses HTTP SSE、Responses WebSocket JSON、Responses WebSocket SSE、Chat JSON、Chat SSE、Review 全部可用。
- Compact 要么使用真实前序 compaction 输入通过，要么明确记录缺少真实 encrypted content，不能用伪造输入宣称通过。
- 每个业务请求都能用 `x-request-id` 查到对应管理端事件或明确说明该入口不写事件。
- 成功事件包含 route、model、accountId、statusCode、transport、usage。
- 失败事件包含 route、model、accountId、statusCode、failureClass、upstreamStatus/upstreamError。
- 没有 token、refresh token、cookie 明文进入文档、控制台记录或提交内容。
- disabled/banned 账号不会被请求链路选择，也不会在重启后继续 refresh。
- quota/cooldown/session affinity/reasoning replay/WebSocket required/fallback 行为与本文风控检查点一致。

## 不通过时的处理顺序

1. 先按 `x-request-id` 查管理端日志和 SQLite `event_logs`。
2. 再查 `.runtime/logs` 同 request id 片段。
3. 如果是 WebSocket，查 `CODEX_PROXY_WS_AUDIT_DIR` artifact。
4. 对比 `docs/upstream-request-chain-audit.md` 和 `docs/risk-control-audit.md` 的 TS/OpenAI 原版基线。
5. 只有确认是本项目实现问题后再改代码；改完更新本文或追加到 `docs/real-chain-audit.md`。
