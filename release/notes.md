# v3.56.1

## 协议变更与升级

> [!IMPORTANT]
> 从 v3.56.0 起，网关移除了原生协议之间的内置转换。Responses、Messages 和 GenAI 请求各走对应的上游。Chat Completions 继续适配这三种上游，支持 JSON 和流式响应。
>
>做 2API 时，跨协议转换很容易变成长期维护负担。工具调用、流式事件和会话续接各有自己的规则，有些天生就不一致，即使是相当热门的开源项目，当下也正在频繁的遇到转换问题，甚至被拖着迈不动脚。这段时间我就在想，这是不是伪需求，发展线路正不正确，客户端更新新功能，转换逻辑就得跟着修，而且虽然各家推出了各家的协议文档，但是放到客户端上总有差别，甚至要花费海量的精力去处理因此带来的问题，更别提安全性，用钦定平台都会被风控，换一个就更别说了。
>
>后续我会把精力放在原生透传和链路安全上，尽量去抹平 apikey 接入带来的风险，实现 OAuth 级别的一致，继续把 Chat Completions 到各上游的适配做稳，为什么是 Chat 呢，因为它已经是成熟非变更的协议，而且国内很多客户端也都是走 Chat。
>
>当然如果有三方 harness 声明支持平台协议，我也很愿意去花费精力去适配少量的协议差错，但是不同的是，这次转变了，不再是网关，而是客户端。
>
>有特定跨协议需求时，可以通过上游适配插件实现，插件系统完全支持。[上游适配器文档](https://github.com/zyycn/codex-proxy-rs/blob/v3.56.1/backend/crates/gateway-plugin/sdk/docs/upstream-adapters.md)

- 此前用 Responses 调用 Messages API Key 账号的客户端，请切换到 `/v1/messages` 或 `/v1/chat/completions`
- Codex 的模型列表只显示当前 Key 能通过 Responses 调用的模型。此前在 Codex 中选用 Antigravity 模型的用户，可切换到原生 GenAI 客户端或 Chat Completions

## 新增功能

- 中间件可用 `omitBodyAt` 省略指定阶段的请求正文，或用 `websocketErrorCodes` 仅订阅指定的 Responses WebSocket 下行错误

## 优化改进

- Antigravity 原生 GenAI 接口支持 `x-goog-api-key` 传入网关 Client Key，桌面端和 CLI 按 Google 的菜单顺序显示当前 Key 可用的型号
- xAI 原生 Responses 请求保留工具、推理参数和上游续接语义

## 问题修复

- Antigravity OAuth 账号的额度面板补上「额度点数」入口，可查看 Google AI 点数余额并设置是否自动使用
- Codex 压缩请求从 HTTP 进入且无需 WebSocket 时，默认沿用 HTTP/SSE
- Antigravity 刷新额度后，账号页使用 Google 返回的套餐描述
- Antigravity 的新请求记录恢复显示请求发生时的订阅名称
- 管理端与 Client Key 用量明细的思考档位列统一左对齐
