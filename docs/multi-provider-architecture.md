# 多平台统一 AI 网关目标架构

本文定义 Codex Proxy RS 向多平台统一 AI 网关演进的目标边界。它是目标设计，不表示仓库当前已经具备这些能力；当前生效结构仍以 [architecture.md](architecture.md) 为准，终态数据模型见 [multi-provider-database.md](multi-provider-database.md)。

目标实现保持一个产品和一个默认部署单元。Cargo workspace 用于约束业务依赖，Provider adapter 使用静态链接，不把不同平台拆成微服务，也不引入动态 Rust 插件 ABI。

## 目标

网关最终允许客户端使用 OpenAI、Anthropic 或网关原生协议，通过同一执行链访问 OpenAI、Anthropic、Google、xAI、Azure OpenAI、Bedrock、Codex、本地模型及其他兼容平台。

架构必须满足：

1. 客户端协议、上游平台、部署端点、凭据、模型和路由是独立概念。
2. API handler 不识别具体 Provider，Provider adapter 不拥有调用方预算和跨平台路由规则。
3. 新增 Provider 不修改核心 dispatch、已有客户端协议或通用存储结构。
4. 新增客户端协议不修改任何 Provider。
5. 只类型化网关需要理解的稳定语义，不建立包含所有厂商字段的万能请求结构。
6. 流式与非流式请求共享同一执行和结算路径。
7. 一个规则只有一个 owner；retry、commit、fallback 和 accounting 不在 adapter 中重复实现。

不以动态加载第三方二进制插件、跨进程 Provider 调用或将每个平台拆成独立服务为默认目标。只有出现独立扩缩容、故障隔离或组织所有权需求时，才评估进程拆分。

## 核心概念

| 概念 | 责任 | 示例 |
| --- | --- | --- |
| Client protocol | 客户端请求和响应的 wire contract | OpenAI Responses、Anthropic Messages |
| Provider | 一个上游平台的行为 adapter | OpenAI、Anthropic、xAI、Codex |
| Provider instance | 一个可配置的上游部署或端点 | Azure 香港区、公司 OpenRouter |
| Credential | Provider instance 使用的认证资源 | API Key、OAuth 账号、AWS 凭据 |
| Model target | 一个 instance 上的真实模型 | `claude-sonnet-4`、`grok-4` |
| Model route | 暴露给客户端的模型及候选目标 | `smart-code` → Codex/OpenAI/Anthropic |
| Logical request | 客户端发起的一次完整业务请求 | 一次 `/v1/responses` 调用 |
| Attempt | Logical request 的一次真实上游调用 | OpenAI key-1 返回 429 |

这些概念不能通过模型名称前缀或一个通用 `account` 结构隐式绑定。Router 选择 Provider instance 和 model target；Provider 在自己的资源池中选择 credential。

## 总体结构

```text
                            Control Plane
             Admin API / Catalog / Routes / Credentials
                                  |
                          RuntimeSnapshot
                                  |
                                  v
Client protocol -> Protocol adapter -> Gateway Engine -> Route Planner
                                             |
                                      Attempt Coordinator
                                             |
                                      Provider Registry
                                  /          |          \
                             OpenAI      Anthropic      Codex ...
                                  \          |          /
                                    Canonical Events
                                             |
Client response <- Protocol encoder <--------+
```

控制面管理配置和后台状态，数据面处理请求热路径。它们默认运行在同一进程，但通过不可变运行时快照隔离，避免每次请求临时查询和拼装数据库配置。

## 数据面

每个请求按固定顺序执行：

1. 协议 adapter 解析外部请求并生成内部 operation。
2. 验证下游 API Key，建立调用方、request ID、deadline 和 cancellation context。
3. 从 operation 推导模型能力要求。
4. Model Router 冻结本请求的 Route Plan。
5. 预留调用方速率、并发和预算。
6. Attempt Coordinator 依次执行允许的目标、凭据轮换和重试。
7. Provider 将上游响应规范化为 canonical event stream。
8. 协议 adapter 将事件编码为目标客户端协议；非流式响应由相同事件流聚合得到。
9. 终结请求结果、用量、目录价格估算和每次 attempt 事实。

只有协议 adapter 处理 OpenAI、Anthropic 等 wire casing 和事件名称。Gateway Engine 不接触 Axum body、SSE 原始字节、WebSocket frame 或 Provider SDK 类型。

### Operation 边界

内部按业务能力定义 operation，而不是建立全平台字段并集：

```rust
enum Operation {
    Generate(GenerateRequest),
    Embed(EmbedRequest),
    Rerank(RerankRequest),
    GenerateImage(ImageRequest),
    Speech(SpeechRequest),
}
```

只类型化网关确实需要解释、路由或结算的稳定字段，例如内容、工具、输出格式、推理要求、token 限制和 usage。平台专属参数通过按 Provider 命名的 `provider_options` 传递，并由对应 adapter 校验。

已知但不支持的语义必须返回稳定的 unsupported 错误，不能静默删除字段。Realtime 和异步 batch 具有不同生命周期，只有实际实现时才增加各自边界，不提前塞入普通 request/response trait。

### Canonical event

所有上游输出转换为少量稳定事件：

```rust
enum GatewayEvent {
    Started(ResponseMeta),
    ContentAdded(ContentItem),
    TextDelta(TextDelta),
    ReasoningDelta(ReasoningDelta),
    ToolCallDelta(ToolCallDelta),
    Usage(Usage),
    Completed(ResponseMeta),
}
```

OpenAI SSE、Anthropic SSE、Gemini stream 和 Codex WebSocket 都只解码一次。协议 encoder 从 canonical events 生成 OpenAI SSE、Anthropic SSE 或普通 JSON，避免按 Provider 和客户端协议形成 N × M 条转换路径。

### Provider 执行边界

Provider 热路径只有一个执行接口：

```rust
type EventStream =
    Pin<Box<dyn Stream<Item = Result<GatewayEvent, ProviderError>> + Send>>;

#[async_trait]
trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    async fn execute(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError>;
}
```

`ProviderStream` 同时返回 Provider instance、真实模型、匿名 resource ID、上游 request ID 和 transport。凭据租约与并发 permit 由 stream 持有，结束、取消或 drop 时释放。

Provider Registry 是唯一需要 `Arc<dyn Provider>` 的异构边界，event stream 也只在该边界 boxed。Provider 内部继续使用具体类型和静态分发，避免泛型与 trait object 扩散。

模型同步、OAuth 刷新、额度抓取和健康探测不是所有平台共享的热路径能力，不进入主 Provider trait。它们由 Provider crate 内的后台任务实现并在 bootstrap 注册。

## 模型路由与资源选择

### Route Plan

客户端模型先解析为 route group：

```text
smart-code
|- codex-prod / gpt-5-codex
|- anthropic-prod / claude-sonnet
`- openai-prod / gpt-5
```

Router 禁止通过 `starts_with("grok")` 一类字符串启发式选择平台。每个 model target 维护真实 operation、feature、context、输出、地区和价格能力。

Router 依次执行：

1. 调用方 allowlist、数据驻留和协议策略过滤。
2. operation、tools、vision、reasoning、JSON Schema、continuation 等能力过滤。
3. context window、输出限制和预算过滤。
4. Provider instance 健康、熔断和可用容量过滤。
5. 按固定优先级、权重、延迟、成本或 sticky 策略排序。

结果是请求级不可变 Route Plan。配置更新不改变在途请求的候选集合。

### Credential 归 Provider 管理

Credential 不是统一 API Key：

```text
Codex       -> OAuth account / Cookie / quota / WebSocket session
OpenAI/xAI  -> API key
Azure       -> endpoint / deployment / API key
Bedrock     -> AWS credential / region / SigV4
Gemini      -> API key or service account
Ollama      -> endpoint, optionally no credential
```

Core 只传递 Provider instance、model target 和本次排除的 resource ID。Provider 自己选择凭据并返回匿名 attempt metadata。Secret 明文只存在于 Provider 内部，不能进入日志、错误、Debug 输出或 RuntimeSnapshot 的公开部分。

## Retry、commit 与 fallback

Attempt Coordinator 是 retry 和跨目标 fallback 的唯一 owner：

- 每次可能到达上游的 payload 都对应一个独立 attempt；Provider adapter 不得隐藏 credential retry。
- HTTP/SDK transport 的自动业务 retry 和携带 payload 的自动 redirect 默认关闭，不能让底层产生 telemetry 看不见的第二次调用。
- Logical request 与 attempt 必须先持久化，之后才能发送可能产生费用或副作用的上游 payload。
- Upstream `not_sent/sent/ambiguous` 与 downstream commit 是两个独立状态；ambiguous upstream send 默认禁止自动重放。
- Request 和每个 attempt 都冻结并持久化 deadline；进程重启后的 recovery 不依赖已经变化的运行配置推断超时。
- 首个可交付事件之前，可以切换同平台凭据或 Route Plan 中的下一个目标。
- 响应一旦向客户端 commit，禁止重试或切换 Provider。
- 请求语义错误和 capability mismatch 不重试。
- 认证失败禁用或冷却当前 credential。
- 429 按 `Retry-After` 和 Provider policy 冷却。
- 网络错误和 5xx 只允许在 commit 前重试。
- 非幂等 operation 默认不做跨平台 fallback。
- 跨平台 fallback 必须由 model route 显式声明，系统不能自行猜测模型等价性。

Logical request 和 attempt 分开记录：

```text
Logical request
|- Attempt 1: OpenAI / key-1 / 429
|- Attempt 2: OpenAI / key-2 / 503
`- Attempt 3: Anthropic / key-7 / success
```

Provider error 先映射为稳定的 `ProviderError`，Gateway Engine 再映射为 `GatewayError`，最后由客户端协议 adapter 编码。Provider 原始 status、code、request ID 和安全诊断可以保留，但不能直接决定客户端 contract。

## 对话延续

网关明确支持两种不同模式：

### Native continuation

使用上游 `previous_response_id`、conversation ID 或 connection-local state。Continuation binding 必须固定 Provider、Provider instance 和必要的 credential scope，不能把一个平台的资源 ID 发送给另一个平台。

Provider 必须声明 native state 是否可安全并发重用；“可重复但只能串行”的平台要提供自己的持久状态边界，不能伪装成通用 binding。单次消费 handle 的 claim/consume/ambiguous 状态由 PostgreSQL CAS 持久化，Redis 只协助短期串行化；Redis 丢失不能让已经可能到达上游的 handle 被再次发送。

### Portable continuation

网关保存加密 canonical transcript，每次重新构建完整上下文。该模式允许跨 Provider 路由，但成本、上下文限制和部分平台专属语义损失必须显式可见。

Transcript 以不可变 snapshot 链保存：每次响应绑定一个新叶子，分支不会读到彼此未来内容；过深时可以 materialize，但不得延长正文 retention。

失败时不能把 native continuation 暗中降级成 portable continuation。

## 控制面

控制面负责：

- Provider instance 与 endpoint 配置。
- 上游 credential 配置、secret revision 与独立的高频运行状态。
- Provider model catalog 与能力覆盖。
- 对外模型、route group 和 route target。
- 调用方 allowlist、速率、并发和预算策略。
- 价格版本、健康状态和 Provider cooldown。
- OAuth 刷新、额度同步、模型同步和健康探测任务。

管理修改携带调用方读到的 expected revision，先在同一事务中持久化、完成 Provider 专属校验并推进单调 config revision，再从一致性读取编译为不可变 RuntimeSnapshot，避免并发编辑静默覆盖。OAuth token 刷新和 credential availability 使用各自 revision 更新，不触发全量配置重载。无效配置不能进入热路径。单进程可以使用进程内 snapshot；多实例部署用 Redis 通知缩短传播时间，同时周期性从 PostgreSQL 对账 revision，因此丢失通知不会永久保留旧配置。

## Cargo workspace 终态

```text
backend/
|- Cargo.toml
|- apps/
|  `- gateway/
|     `- src/
|        |- main.rs
|        |- bootstrap.rs
|        `- workers.rs
|- crates/
|  |- gateway-core/
|  |  `- src/
|  |     |- operation/
|  |     |- engine/
|  |     |- routing/
|  |     |- policy/
|  |     |- accounting/
|  |     |- event.rs
|  |     `- error.rs
|  |- gateway-api/
|  |  `- src/
|  |     |- openai/
|  |     |- anthropic/
|  |     |- native/
|  |     `- admin/
|  |- gateway-protocol/
|  |  `- src/
|  |     |- openai/
|  |     |- anthropic/
|  |     `- gemini/
|  |- gateway-store/
|  |  `- src/
|  |     |- postgres/
|  |     `- redis/
|  `- providers/
|     |- codex/
|     |- openai/
|     |- anthropic/
|     |- google/
|     |- xai/
|     |- azure-openai/
|     |- bedrock/
|     |- openrouter/
|     `- ollama/
`- migrations/
```

依赖方向固定为：

```text
gateway binary
|- gateway-api --------> gateway-core
|- gateway-store ------> gateway-core
`- provider-* ---------> gateway-core

gateway-core 不依赖 Axum、SQLx、reqwest、Redis、具体 Provider 或客户端协议。
```

Provider crate 是业务编译边界，不是独立进程或准备发布的通用 SDK。Provider 之间禁止相互依赖；相同 wire contract 放在 `gateway-protocol` 复用，平台认证、错误、额度和 transport quirks 仍由各自 adapter 管理。

对于真正只需要 base URL、认证 header 和少量兼容开关的平台，可以提供配置驱动的 OpenAI-compatible adapter。Codex、Azure、Bedrock 等行为明显不同的平台不能因为 JSON 相似而塞入同一个 OpenAI client。

## 数据、安全与遥测边界

PostgreSQL 保存 Provider instance、credential metadata、模型路由、调用方策略、逻辑请求、attempt、usage、价格和 continuation binding 等持久事实，具体表结构见 [multi-provider-database.md](multi-provider-database.md)。Redis 只保存多实例限流、并发租约、短期 cooldown、affinity 和配置通知等可恢复状态。

下游 client API key 与上游 credential 必须使用不同存储和权限边界。Credential secret 使用环境变量、外部 secret manager 或独立加密密钥保护；身份伪名或 HMAC 密钥不能兼作 secret encryption key。

遥测以 logical request 和 attempt 为两个层级：

- Logical request 记录客户端协议、公开模型、调用方结果和最终 usage。
- Attempt 记录 Provider、instance、真实模型、匿名 resource、transport、status、延迟、commit 和 retry reason。
- 标准化 usage 用于跨平台统计；token 使用普通列，image、audio 等非 token metric 使用版本化 registry 中的名称和固定单位，不能把 Provider 原始 usage body 当作通用 schema。
- 价格按 Provider instance、模型、service tier 和生效时间保存不可变版本，每个版本原子包含完整 metric rates；目录价格估算区分 known、partial 和 unknown，不能把缺失费率按零费用处理或跨货币直接相加，也不能冒充 Provider invoice。

## 当前代码的目标归位

| 当前边界 | 目标边界 |
| --- | --- |
| `api/client/responses` | `gateway-api/openai/responses` |
| `dispatch` | 去除 Codex 类型后的 `gateway-core/engine` |
| `fleet` 账号池 | `providers/codex/account_pool` |
| `upstream/openai` | Codex 专属部分进入 `providers/codex`，通用 wire codec 进入 `gateway-protocol/openai` |
| `models` | 控制面 model catalog 与 routing snapshot |
| `telemetry/usage` | 通用 logical request、attempt 和 usage 事实 |
| `bootstrap` | Provider 注册、store 实现和后台任务的唯一 composition root |

迁移期间不保留两套生产执行路径。每个阶段先用当前 Codex Provider 包住既有行为，再逐步把协议、调度和遥测边界中残留的 Codex 类型移回 adapter。

## 验收标准

目标边界完成后应满足：

1. 新增 Anthropic 等 Provider 时，只新增 Provider crate、配置校验和 bootstrap 注册；核心 engine、OpenAI handler 和通用数据库结构不变。
2. 新增 Anthropic 等客户端协议时，只新增 protocol adapter；Provider 不变。
3. 新增普通 OpenAI-compatible instance 时只增加运行时配置，不增加重复业务 service。
4. API、routing、accounting 和 telemetry 中没有 `if provider == ...` 的平台业务分支。
5. Provider contract suite 覆盖非流式、流式事件顺序、工具、usage、错误、取消、commit 前后 retry 和 secret redaction。
6. 跨 Provider continuation、fallback 和未知 capability 不会发生隐式语义降级。

如果新增一个 Provider 仍需要修改 dispatch match、给统一请求增加大量可选字段、复制一套 `create` 流程或改变通用存储结构，说明边界尚未达到本文目标。
