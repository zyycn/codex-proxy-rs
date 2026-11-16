# v3.50.0

## 新增功能

- URL 子路径部署：源码构建可通过 `CPR_WEB_BASE_PATH` 设置前缀，管理端、PWA 和生成的客户端地址沿用该前缀
- OpenAI 额度桶预热：可为账号的通用额度桶指定预热模型，窗口重置满 5 分钟后复查，仅对本轮尚未使用的窗口预热，默认关闭

## 问题修复

- OpenAI 建连恢复：WebSocket 新请求在发送前遇到瞬时建连失败时按 HTTP 的规则重试和换号，同一请求后续选号会跳过绑定到已确认不可达的代理入口的账号
- OpenAI 会话续接：可安全重放的原生续接遇到超出等待上限的容量拒绝时，在有可用替代账号和剩余换号次数的情况下先切换绑定，再通知客户端携带完整历史重试
- OpenAI 容量错误：响应移除旧账号的 `Retry-After` 和 `retry-after-ms`，避免客户端再次等待
- OpenAI 请求错误：将不带状态码的流内 `invalid_request_error` 归为请求错误，修复非流式调用收到 502 的问题
- 管理端应用：修复以独立窗口打开时内容被标题栏遮挡的问题

## 升级须知

- OpenAI 账号额度：数据库迁移自动保留已有调度上限，管理 API 调用方需将 `quotaLimits` 改为 `quotaSettings`，每个桶的值改为包含 `limitPercent` 和 `warmup` 的对象，并改用能力标记 `quotaSettingsSupported`
- URL 子路径部署：前缀在构建时确定，反向代理需剥除请求前缀并重写 Cookie `Path` 和站内 `Location`，升级时需重新构建
