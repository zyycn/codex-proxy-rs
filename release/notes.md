# v3.57.1

## 问题修复

- Antigravity CLI 使用分组模型调用 `search_web` 时，无需指定推理档位即可搜索，原有搜索选项随请求透传
- OpenAI Responses 通过 WebSocket 复用连接时，按当前请求的设置省略响应字段和流事件字段，避免沿用其他请求的设置
- OpenAI 模型目录保留预热连接握手返回的 ETag，复用同一连接时只确认一次目录版本
