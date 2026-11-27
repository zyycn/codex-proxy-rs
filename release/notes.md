# v3.56.0

## 新增功能

- Antigravity OAuth 账号可开启自动使用 Google AI 点数，套餐额度耗尽后按上游余额和最低使用门槛继续生成
- 中间件可用 `omitBodyAt` 省略指定阶段的请求正文，或用 `websocketErrorCodes` 仅订阅指定的 Responses WebSocket 下行错误

## 优化改进

- Chat Completions 直接适配 Responses、Messages 和 GenAI 上游，支持 JSON 和 SSE
- Antigravity 原生 GenAI 接口支持 `x-goog-api-key` 传入网关 Client Key，桌面端和 CLI 按 Google 的菜单顺序显示当前 Key 可用的型号
- xAI 原生 Responses 请求保留工具、推理参数和上游续接语义

## 问题修复

- Codex 压缩请求从 HTTP 进入且无需 WebSocket 时，默认沿用 HTTP/SSE
- Antigravity 刷新额度后，账号页使用 Google 返回的套餐描述
- 管理端与 Client Key 用量明细的思考档位列统一左对齐

## 升级须知

- 原生 Responses、Messages 和 GenAI 请求只匹配支持对应协议的上游；此前通过 Responses 调用已导入的 Messages API Key 账号时，请切换到 `/v1/messages` 或 `/v1/chat/completions`
- Codex 模型目录只显示当前 Key 可通过 Responses 执行的模型，Antigravity 模型请通过原生 GenAI 客户端或 Chat Completions 调用
