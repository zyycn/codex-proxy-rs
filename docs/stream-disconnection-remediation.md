# Responses 流式断连与历史续接失败排查

日期：2026-07-10
状态：已完成只读排查，未修改业务代码或线上配置

## 一、排查过程

### 1. 确认实际运行版本与基础设施

- Docker 镜像 tag 仍是旧版本，但在线升级状态确认当前进程运行 `v1.0.11`；本次故障发生在升级完成之后。
- Nginx 已关闭请求和响应缓冲，读写超时为 7200 秒。
- 故障窗口内应用 healthy，无 OOM、panic、重启或 Nginx upstream error。

因此，本次故障不是镜像版本判断错误、常规反代超时或服务进程崩溃。

### 2. 反查 `resp_0af2da5fcb22adab016a508f8f35a481958ce857bc359c73b9`

| 北京时间 | 请求 | 服务端事实 |
| --- | --- | --- |
| 14:22:17 | `req_6b1fa095...` | 原响应正常完成，`completed=true`、`store=false`；账号 `acct_0b5ed92a...`；WebSocket 为 `reuse` |
| 14:23:51 | `req_a085bb96...` | 同账号、同 conversation hash、同 pool key，再次 `reuse`；HTTP 200 已提交，但之后没有 `live response stream finalized` |
| 14:24:20 | `req_4f265b8e...` | 客户端仍携带原 response ID 重试；账号和 pool key 不变，但 WebSocket 已变为 `new` |
| 14:24:20.755 | 同上 | 服务先向客户端提交 HTTP 200 |
| 14:24:20.830 | 同上 | 75 ms 后上游返回 `previous_response_not_found`；没有真实首 token |
| 14:24:20–14:25:56 | 17 个请求 | 同一个失效 response ID 被连续重试，全部失败 |

14:23:51 的请求没有流终结记录，且服务进程没有重启。结合代码中的下游取消路径，可以确定该次下游流被丢弃：响应 body 被 drop 后触发 cancel，随后上游 WebSocket 被 discard。下一次请求因此只能创建新连接。

### 3. 核对智能调度与会话亲和性

- 原响应和 17 次失败全部选择同一个上游账号 `acct_0b5ed92a...`。
- 调度日志虽然显示 `rotation_strategy=smart`，但选择来源为 `preferred`。
- 原请求和失败请求的 conversation hash、WebSocket pool key 均一致。
- 如果真的切换了账号，`strip_history_if_account_changed` 会在发送上游前移除旧 history。

结论：智能调度和账号亲和性工作正常，不是本次 `previous_response_not_found` 的原因。

### 4. 核对 Responses WebSocket 续接语义

原响应使用 `store=false`，不会作为持久化 Response 保存。OpenAI 官方说明：WebSocket 模式只在连接本地缓存最近的 previous response；连接缓存无法解析该 ID 时，应将 `previous_response_id` 设为 `null`，并重新提交完整输入上下文。

参考：[OpenAI Conversation state / WebSocket mode](https://developers.openai.com/api/docs/guides/conversation-state#previous_response_id-in-websocket-mode)

因此，“仍是同一账号”不足以保证旧 ID 可用；对于未持久化响应，还依赖原 WebSocket 的连接级缓存。

### 5. 核对 1.0.11 的恢复路径

代码已经实现 `previous_response_not_found` 恢复：

1. `recover_request_history` 忘记失效 ID，并移除 `previous_response_id` 与 turn state；
2. 将下一次尝试固定到原账号；
3. 在同一个入站请求内重新调用上游。

但该恢复只在 HTTP 响应尚未提交时有效。本次请求在真正的 400 到达前 75 ms 已提交 HTTP 200。

原因是 `event_is_first_content` 只排除 `response.created` 和 `response.in_progress`，其余可转发事件都会结束预取。日志同时显示没有真实首 token，说明某个非输出结构事件提前越过了预取边界。400 随后进入 live stream，只能被包装为 `response.failed`，不能再回到调度循环重试。

现有测试 `responses_stream_should_strip_history_after_previous_response_not_found` 让失败帧作为 WebSocket 第一帧返回，因此能够通过，但没有覆盖“非输出结构帧在前、失败帧稍后到达”的真实时序。

## 二、问题原因

本次故障是以下链路叠加造成的：

```text
客户端/TUN 下游断流
  -> 服务取消 live stream 并销毁原 WebSocket
  -> store=false 的连接级 previous response 缓存随连接消失
  -> 客户端在新 WebSocket 上继续发送旧 previous_response_id
  -> 上游正确返回 previous_response_not_found
  -> 代理预取过早提交 HTTP 200，错过已有的 history recovery
  -> 客户端不断重试同一个失效 ID
```

根因分层如下：

1. **触发条件**：下游连接中断导致原 WebSocket 被 discard。该现象与此前客户端侧 `error decoding response body`、TUN/fake-IP 路径一致。
2. **上游行为**：对 `store=false` 且连接缓存已丢失的 response ID 返回 `previous_response_not_found`，符合 WebSocket 续接语义。
3. **服务端缺陷**：WebSocket “首内容”判定与真实首 token 判定不一致，导致可恢复错误在 HTTP 200 之后才暴露。
4. **重试放大**：Codex 继续在原线程发送相同 ID；第一次失败又会销毁新 WebSocket，随后每次请求都从 `new` 连接开始。
5. **非根因**：智能调度、账号轮换、Nginx 超时、应用重启以及运行版本均已排除。

外层错误中的 `stream_disconnected` 是代理在 live stream 阶段合成的包装；真正的上游原因是内部的 HTTP 400 `previous_response_not_found`。

## 三、修复方案

### 1. 立即止损

- 受影响客户端应开启新线程，或者移除 `previous_response_id` 后重新提交完整上下文。
- 不要在原线程连续执行“继续”；它只会重复发送同一个失效 ID。
- 对客户端到 `codex-proxy.aivify.cc` 的路径做 DIRECT/TUN 绕过 A/B，先解决导致原 WebSocket 被销毁的下游断流。

### 2. 修复 WebSocket 预取边界

WebSocket 预取和 `firstTokenMs` 必须共用同一套语义判定：

- 真实文本、reasoning、工具参数 delta 或 output item：视为首输出，可以提交下游响应；
- `response.completed`、`response.failed`、`error`：视为终态，在提交前完成分类；
- created、in-progress、part-added 等没有实际输出的结构事件：只缓存，不结束预取。

这样 `previous_response_not_found` 会在 HTTP 200 提交前进入现有 `recover_request_history` 分支。

### 3. 保证恢复语义安全

- 如果代理保留了完整输入快照，清除旧 ID 后用完整上下文重试。
- 如果是客户端显式传入的 ID，而代理没有完整历史，不能假装无损恢复；应返回明确错误，要求客户端新建线程或重发完整上下文。
- 不应静默把所有请求从 `store=false` 改成 `store=true`，否则会改变数据留存语义。若要启用持久化，必须作为显式配置。

### 4. 补回归测试

- `response.created -> 非输出结构事件 -> previous_response_not_found`：断言 HTTP 200 前完成恢复，第二次上游 payload 不含旧 ID。
- 下游取消后 pooled WebSocket 被 discard，再用旧 ID 续接：断言不会形成重试风暴。
- 真实 delta 之后再出现错误：断言禁止透明重试，避免重复输出、工具调用和计费。
- 显式 previous ID 且没有完整上下文：断言不会以残缺输入伪装成功恢复。

### 5. 调度与部署建议

- 保持智能调度和会话亲和性；切换成轮询不能解决连接级缓存问题，反而更容易跨账号。
- 不要通过禁用 WebSocket pool 规避；`store=false + previous_response_id` 在每次新建连接时更容易失败。
- 修复后先在 next 实例验证上述时序，再切换 Nginx upstream，并保留旧实例回滚。
- Nginx access log 补充 request ID、请求时长和协议；应用补充 `client_cancelled`、`downstream_send_failed` 与 pool discard 原因，便于后续直接串联同一事件。

## 四、`gpt-5.6-sol` 的 `ultra` 记录问题

### 1. 排查过程

- 查询线上最近 80 条 `gpt-5.6-sol` 使用记录，`reasoningEffort` 全部为 `max`。
- 核对代理代码：请求体中的 `reasoning` 原样透传；使用记录直接读取 `reasoning.effort`；管理端直接展示该记录值，没有 `ultra -> max` 转换。
- 使用 Codex 0.144.1 和本地 mock 端点分别抓取 `ultra`、`max` 两种配置的实际请求：

```text
客户端选择 ultra -> reasoning.effort = max
客户端选择 max   -> reasoning.effort = max
```

- 两份请求的实际差异在协作模式：`ultra` 注入“主动多 Agent 委派”，`max` 注入“仅在明确要求时委派”。
- 因此该现象与智能调度、账号选择和代理归一化无关。

### 2. 问题原因

Codex 0.144.1 将 `ultra` 实现为组合预设：

```text
ultra = max 推理强度 + 主动多 Agent 委派
```

发往 Responses API 的请求中没有字面值 `ultra`，只有 `reasoning.effort = max` 和单独的多 Agent 模式指令。代理目前只记录前者，因此管理端显示 `max`。这是记录维度不足，不是记录值被错误改写，也不是上游降级。

### 3. 修复方案

- 保留 `reasoningEffort = max`，因为这是上游实际收到的推理强度。
- 从请求中的 `<multi_agent_mode>` 识别主动委派，新增 `multiAgentMode = proactive` 或 `reasoningPreset = ultra` 元数据。
- 管理端组合展示为 `ultra（max + 自动委派）`，不要直接把原始 `reasoningEffort` 覆盖为 `ultra`。
- 长期方案是由客户端在 `client_metadata` 中显式携带原始预设；在此之前，服务端只能根据多 Agent 模式指令推断。

## 五、Codex 0.144.1 与 WebSocket 依赖更新核对

### 1. 排查过程

截至 2026-07-10，OpenAI Codex 最新稳定版本为 `rust-v0.144.1`。本次核对了该版本、`main` 分支、两个 OpenAI fork 的提交历史以及本项目 `v1.0.11` 的实际依赖锁定。

本项目当前锁定：

```toml
tokio-tungstenite = { git = "https://github.com/openai-oss-forks/tokio-tungstenite", rev = "132f5b39c862e3a970f731d709608b3e6276d5f6" }
tungstenite = { git = "https://github.com/openai-oss-forks/tungstenite-rs", rev = "9200079d3b54a1ff51072e24d81fd354f085156f" }
```

Codex `0.144.1` 和当前 `main` 均已更新为：

```toml
tokio-tungstenite = { git = "https://github.com/openai-oss-forks/tokio-tungstenite", rev = "0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186" }
tungstenite = { git = "https://github.com/openai-oss-forks/tungstenite-rs", rev = "4fffad30fe373adbdcffab9545e9e9bf4f2fc19f" }
```

参考：

- [Codex 0.144.1 Cargo.toml](https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/Cargo.toml#L559-L568)
- [`tokio-tungstenite` 两个版本的完整差异](https://github.com/openai-oss-forks/tokio-tungstenite/compare/132f5b39c862e3a970f731d709608b3e6276d5f6...0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186)
- [`tungstenite` 两个版本的完整差异](https://github.com/openai-oss-forks/tungstenite-rs/compare/9200079d3b54a1ff51072e24d81fd354f085156f...4fffad30fe373adbdcffab9545e9e9bf4f2fc19f)

版本变化如下：

| 项目 | 实际变化 | 对当前服务的影响 |
| --- | --- | --- |
| `tokio-tungstenite` | 新增 Happy Eyeballs：交错尝试 IPv4/IPv6，首个地址连接停滞 250 ms 后竞争备用地址；直连和代理连接均使用该逻辑 | 可改善首次建连和断线重连，尤其是 IPv6 可解析但不可达的场景 |
| `tungstenite` | 更新 WASM 下 `permessage-deflate` 的压缩后端、最低 Rust 版本及代理代码整理 | 对本服务 Linux 环境的帧解析、Ping/Pong 和连接存活语义没有实质变化 |
| Codex `0.143.0` | 加入 GPT-5.6 模型；将 `max` 作为正式推理强度；增量 WebSocket 请求比较时忽略 response metadata | 影响模型能力声明和客户端是否选择增量请求，不代表上游改变了 `previous_response_id` 的失效规则 |
| Codex `0.144.0` | Responses WebSocket 继续走低延迟通道，同时支持系统代理和自定义 CA | 主要改变 Codex 客户端的建连路径 |
| Codex `0.144.1` | 安装器和 Code Mode 可靠性修复 | 没有新的 Responses 或 WebSocket 协议变化 |

参考：[Codex 0.143.0](https://github.com/openai/codex/releases/tag/rust-v0.143.0)、[Codex 0.144.0](https://github.com/openai/codex/releases/tag/rust-v0.144.0)、[Codex 0.144.1](https://github.com/openai/codex/releases/tag/rust-v0.144.1)。

同时核对了官方客户端的续接实现：

1. `previous_response_id` 由当前 WebSocket session 的上一条响应生成，只用于增量请求；
2. 检测到连接关闭并新建 WebSocket 前，官方客户端会清除 `last_request` 和 `last_response`；
3. 新连接发送完整请求，不会把旧连接缓存中的 response ID 继续带过去；
4. 官方实现没有针对 `previous_response_not_found` 的专门 400 重试分支，安全性来自新连接前主动清理增量状态。

参考：[增量请求构造](https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/core/src/client.rs#L1223-L1254)、[新连接状态清理](https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/core/src/client.rs#L1323-L1354)。

本项目已经对齐的部分包括 `responses_websockets=2026-02-06` beta header、自定义 CA 和 `permessage-deflate`。尚未完全对齐的是：底层 transport 创建新 WebSocket 后仍会原样发送已经准备好的 payload，其中可能包含旧 `previous_response_id`；`v1.0.11` 依靠上层的一次 history recovery 事后补救。

### 2. 问题原因

这两个旧依赖不是本次 `previous_response_not_found`、`stream closed before response.completed` 或 15 分钟后连接失效的直接原因：

- 新版 `tokio-tungstenite` 只改善 DNS 解析后的 TCP 建连竞争，不能延长已建立 WebSocket 的上游 response 缓存寿命；
- 新版 `tungstenite` 没有增加主动 Ping、Pong 超时或断流后的业务请求恢复；
- 官方 Codex `0.144.1` 也没有周期性主动 Ping；它在读取期间处理 Ping/Pong，并依靠 idle timeout 和断线后的新连接状态重置；
- `previous_response_not_found` 的关键差异仍是连接状态管理：官方新连接不继承旧增量状态，本项目目前会先尝试发送旧 ID，再依赖上层识别 400 后恢复；
- 智能调度只有在切换账号或打破会话亲和性时才会放大问题，本次日志已确认没有切换账号，因此不是根因。

因此，依赖升级能够降低部分“建连或重连失败”，但不能单独解决当前红色断流错误。

### 3. 修复方案

1. **优先对齐连接状态语义**：将 `previous_response_id` 与上游账号、具体 WebSocket session 共同绑定。连接已失效时，不能直接在新连接上发送旧 ID。
2. **安全重建请求**：如果代理持有完整历史，新连接应清除旧 ID 并重放完整 input；如果只有客户端增量输入，必须返回明确的续接失效错误，不能把残缺输入伪装成无损恢复。
3. **更新两个依赖 pin**：将两个 rev 更新到 Codex `0.144.1` 使用的版本，并在同一次变更中更新。新版 `tokio-tungstenite` 自身已经依赖新的 `tungstenite` pin，分开更新可能引入重复依赖和类型不一致。
4. **保留独立的保活修复**：主动 Ping、Pong deadline、连接 idle 生命周期和业务层重试应继续作为本项目自己的连接池策略，不能依赖本次库升级自动解决。
5. **升级后验证**：覆盖 IPv4/IPv6 建连、custom CA、`permessage-deflate`、空闲 15 分钟后复用、旧连接断开后完整历史重放，以及 `previous_response_not_found` 在首输出前后的两种时序。

依赖升级优先级低于新连接状态修复：可以升级，但不能把它作为现有 400 和流式断连问题的唯一修复方案。
