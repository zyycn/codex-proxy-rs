# Codex Proxy RS 协议保真度审计

审计日期：2026-07-03

## 定位

本文档聚焦 **OpenAI 兼容协议保真度**：`codex-proxy-rs` 把 Codex/ChatGPT 上游翻译成 OpenAI 兼容格式，翻译的完整度决定客户端 SDK 会不会异常。与安全向 `audit-2026-07-03.md`、可维护性向 `maintainability-audit-2026-07-03.md` 分开维护。

覆盖四个面：Chat Completions、Responses API、Models 与错误体、工具调用专项。

## 方法与基准

审计为**纯静态对拍**，未做真实上游抓包。基准有两类，可信度不同：

1. **desktop bundle 基准（强）**：从当日最新官方 `Codex.dmg`（`persistent.oaistatic.com/codex-app-prod/Codex.dmg`，构建时间 2026-07-03）解包出的 Electron `app.asar`，提取 `.vite/build/{main,src,worker}.js` 中的协议字符串。`worker.js` 是核心流处理器。
2. **OpenAI 官方契约基准（中）**：我知识内的 OpenAI API 契约，用于错误体、Models 端点等 bundle 未覆盖处。
3. **开源实现对照（辅助）**：本轮补充对照 `/home/zyy/桌面/Codes/sub2api` 与 `/home/zyy/桌面/Codes/CLIProxyAPI` 的 codegraph 结果。二者不能替代官方抓包，但可用于验证社区常见的 Chat Completions ⇄ Responses/Codex 转换形状。

### 基准的关键局限（务必先读）

**官方桌面端的上游请求体不由这些 JS bundle 构造**。JS 层只做两件事：

- SSE / 流解析（`worker.js` 的 `nh` 事件白名单）
- OpenTelemetry 埋点（`gen_ai.request.*` / `gen_ai.usage.*` 是遥测键，不是发给上游的请求字段）

真正的 Responses 请求体由桌面端内嵌 spawn 的 Rust `codex` CLI 组装，不在解包产物内。因此：

- **SSE 事件面**：bundle 基准硬，结论可信。
- **请求字段面**（`store` / `include` / `max_output_tokens` / `service_tier` / 工具 schema 等）：官方真实取值**基准不确定**，只能核对"结构是否合理"，无法核对"字段级取值是否与官方一致"。要彻底核对这几项需对官方 `codex` CLI 做真实上游抓包。

行号为审计时快照，复核时以实际代码为准。

## 确认的上游 SSE 事件全集

`worker.js` 的 `nh` 数组（事件白名单，官方原样透传给下游，仅用于遥测采集）：

```
response.created          response.in_progress      response.failed
response.completed         response.incomplete       response.queued
response.output_text.delta response.output_item.added
response.function_call_arguments.delta
response.function_call_arguments.done
response.output_item.done
```

附加已知事件（reasoning / content 系列）：`response.reasoning_summary_text.delta/done`、`response.reasoning_text.delta`、`response.reasoning_summary_part.added`、`response.output_text.done`、`response.content_part.added`、`response.custom_tool_call_input.delta`。

端点分类：`responses` / `chat.completions` / `embeddings`。Provider 枚举：`Codex` / `ChatGPT`。

## 分级结论

本项目有两条语义完全不同的路径，保真度差异大：

- **`/v1/responses` 直通**（`TupleSseEventTransformer`）：无 schema 时逐字节透传，接近官方透传语义，保真度高。
- **`/v1/chat/completions` 转换**（`ChatCompletionStreamTranslator`）：主动把 Codex Responses SSE 转成 `chat.completion.chunk`，`match` 未命中的事件 `_ => {}` 丢弃。**偏差集中在此路径。**

官方 11 个核心事件中本项目当前已识别 11 个；`response.incomplete` 已在本轮补齐。

### 🔴 严重（已读代码核实）

**1. `response.incomplete` 全路径终止处理（已修复）** — 可信度最高（基准硬 + 已核实）

- 基准：`response.incomplete` 在官方 `nh` 事件全集内，是合法终止事件（命中 `max_output_tokens`、内容策略等），官方按 `response.status` 采集 finishReasons 并原样透传。
- 项目自身 `upstream/protocol/websocket.rs:235,725` 已在 **WebSocket 路径**完整识别该事件（能取 `incomplete_details/reason`），修复前 **HTTP SSE 路径漏了它**：
  - `proxy/openai/chat.rs` push_event 的 match 无 `response.incomplete` 分支 → 落入 `_ => {}` 丢弃。唯一发 `finish_reason` 和 `[DONE]` 的 `push_completed` 只在 `response.completed` 触发。
  - `proxy/dispatch/responses.rs:2389` `sse_body_has_terminal_event` 只认 `completed|failed|error`。
- 触发场景：任意"未完成但正常终止"的响应（尤其 `max_output_tokens` 截断）。
- 修复前后果：
  - chat 客户端：流无终止 chunk、无 `[DONE]`，流悬挂到超时。
  - responses 客户端：在合法 `response.incomplete` 后被追加一个伪造的 `response.failed`(stream_disconnected)，出现双终止事件；后台把成功的截断响应误记为 502，污染用量/账号健康统计。
- 修复状态（2026-07-03）：已将 `response.incomplete` 纳入 Responses SSE 终止事件集；Chat Completions 流式/非流式转换会把 `max_output_tokens` 映射为 `finish_reason=length`，并正常输出终止 chunk / `[DONE]`；Responses live stream 不再为合法 incomplete 追加伪造 `response.failed`。

**2. Chat Completions 工具定义未从嵌套转扁平（已修复）** — 方向可信，已用开源实现辅助确认

- 基准：Codex 上游是 Responses API，函数工具应为扁平 `{type:"function", name, description, parameters}`；Chat 输入是嵌套 `{type:"function", function:{...}}`。二者不可互换。**注意**：工具 schema 的官方真实取值属"基准不确定"（不在 bundle 内），此条依据"Codex = Responses API"和开源实现对照修复，后续仍可用真实抓包复核。
- 开源对照：
  - `sub2api` `convertChatToolsToResponses()`：把 Chat `tools[].function` 和 legacy `functions[]` 转为 Responses 扁平 `{type:"function", name, description, parameters, strict}`。
  - `CLIProxyAPI` `ConvertOpenAIRequestToCodex()`：把 Chat 函数工具扁平化，内置工具原样透传；函数型 `tool_choice` 从 `{type:"function", function:{name}}` 转为 `{type:"function", name}`。
- 项目修复前：`proxy/openai/chat.rs` `codex_tools()` 的 `tools` 分支原样透传嵌套结构；legacy `functions` 还额外包一层 `{type:"function","function":function}`。
- 修复状态（2026-07-03）：Chat `tools[].function`、legacy `functions[]` 已转为 Responses 扁平函数工具，非 function 内置工具继续透传；函数型 `tool_choice` 已转为 `{type:"function", name}`。

### 🟡 中等

错误体：

- **quota 耗尽 → HTTP 402**（`dispatch/responses.rs` `http_status_code`、`chat.rs` 同）：OpenAI 用 429 + `insufficient_quota`。**判为设计权衡而非 bug** — 402 Payment Required 语义上对应配额耗尽。但若目标是兼容 OpenAI SDK 的限流退避/重试，SDK 的 status→异常映射无 402 分支，会落到通用 `APIStatusError` 而非 `RateLimitError`，退避逻辑失效。取决于产品意图，此处仅标注权衡点。
- 聚合错误 type/code 语义映射（已修复）：quota/rate/auth/model unsupported 经账户池聚合路径时已分别映射为 `insufficient_quota`、`rate_limit_error/rate_limit_exceeded`、`invalid_request_error/invalid_api_key`、`invalid_request_error/model_not_found`。
- 错误体缺 `param` 字段（`errors.rs:47-63`）；多数 SDK 容忍。
- Responses 非流式错误多包一层顶层 `"type":"error"`（`errors.rs:114-141`），偏离 `{"error":{...}}`。
- 401(expired/disabled) 聚合路径 status=401 但 type=`server_error`，body 与状态码不自洽。
- Responses stream 聚合错误 `response.failed` 已复用 dispatch 语义 code，不再仅按 HTTP status 粗映射；chat 流式中途失败帧仍是 type=`stream_error` 且无 `code`（`chat.rs:1001`），OpenAI SDK 不识别，等价于坏流。

SSE / chat 转换：

- chat `response.incomplete` 已映射 `length`/`content_filter`；其它终止状态仍只有 `stop`/`tool_calls`，若上游未来在 `response.completed` 中携带非 stop 终止语义，需要继续扩展。
- chat 转换 `response.reasoning_text.delta` 与 `response.custom_tool_call_input.delta` 已补齐；custom tool 增量按普通 function tool arguments delta 转为 Chat `tool_calls`。
- chat 路径函数型 `tool_choice` 命名工具格式已转换为 Responses 期望的 `{type:function, name}`。
- legacy `function_call` 回传 call_id 用 `fc_{name}` 伪造（`chat.rs:595`），与真实 call_id 对不上，多轮工具对话无法关联结果。
- 流式 `output_item.done` 已补 function/custom tool 兜底；若上游没有单独发送 arguments delta，也能从完整 output item 生成 Chat `tool_calls` 参数。

Models 端点（结构层面与 OpenAI 契约高度一致）：

- 列表纯动态且首拉前为空（`upstream/models/mod.rs:292`）：进程刚启动、未成功拉取任一 plan 模型快照时，`/v1/models` 返回空 data，依赖枚举模型的客户端受影响。
- `/v1/models/{id}` 不解析别名（`upstream/models/mod.rs:381`）：用聊天可用的别名查详情返回 404。
- `created` 全模型硬编码同一常量（`models.rs:15`），契约合法但无真实语义。

### 请求字段面（基准不确定，仅记录待抓包核对）

以下项结构合理，但官方真实取值不在 bundle 内，无法静态核对，留待真实抓包：

- `store` 默认 false 且始终序列化（`responses.rs:624`）。
- `include` 自动注入 `reasoning.encrypted_content`（`responses.rs:585`）— 符合公开 Codex 惯例但 bundle 无字面量命中。
- 不构造 `max_output_tokens`：客户端传入会被静默丢弃。本轮对照结论分裂：`sub2api` 会把 Chat `max_tokens`/`max_completion_tokens` 映射到 Responses `max_output_tokens`；`CLIProxyAPI` 明确注释 Codex 不支持并禁用该映射。暂不改，仍需真实抓包或产品决策。
- `service_tier` 的 `fast`→`priority` 归一化（`responses.rs:597`）为本项目自定义。
- 两条 reasoning 构造路径（`proxy/openai/responses.rs:358` vs `responses.rs:544`）在 effort 缺失时行为不完全一致。
- `function_call_output` 无专门净化分支，靠 `_ => Some(object)` 原样透传。

## 优先级建议

1. **修 `response.incomplete`（🔴，基准硬，已完成）** — 两条 SSE 路径已纳入终止事件集，Chat Completions 已补 `length` finish reason 和 `[DONE]`。
2. **chat 工具定义/`tool_choice` 转扁平（🔴，已完成）** — 已参考 sub2api 与 CLIProxyAPI 的 common path，并补充请求转换测试。
3. **chat reasoning/custom tool delta 与 `output_item.done` 兜底（🟡，已完成）** — 已补 Chat 流式/非流式转换测试。
4. **聚合错误 type/code 与 Responses stream code 映射（🟡，已完成）** — Chat/Responses JSON 与 Responses `response.failed` 已共享语义化错误 code；HTTP status 策略未在本项中改变。
5. **决策 quota→402 vs 429（🟡，产品权衡）** — 明确是要 OpenAI SDK 兼容还是语义化 HTTP 码。
6. 处理 chat stream 中途失败帧、Models 首拉为空/别名详情等中等项。

## 复现方法（解包基准）

```
# 1. 下载当日 DMG
curl -fL https://persistent.oaistatic.com/codex-app-prod/Codex.dmg -o Codex.dmg

# 2. 7z 解包（需 7-Zip 25.x，旧 p7zip 无法解 APFS DMG）
7z x Codex.dmg -o dmg-extract   # 注意路径含空格 "Codex Installer/"

# 3. 提取 app.asar（纯 node，@electron/asar 格式）
#    协议逻辑在 dmg-extract/*/Codex.app/Contents/Resources/app.asar
#    解出后核心文件：.vite/build/{main,src,worker}.js（webview/assets 全是 UI/i18n，无关）
```
