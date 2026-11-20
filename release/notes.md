# v3.54.0

## 新增功能

- 账号分组可设置模型允许或拒绝列表、子代理权限及 Ultrafast 档位，已有分组默认允许全部模型和子代理
- API 密钥支持批量编辑，可查看近 7 天或 30 天的模型用量、费用和请求数，也可将本期日、周预算窗口对齐到指定账号
- OpenAI 请求国家默认跟随代理或全局位置，也可在「系统设置 → 上游配置 → 请求位置覆盖」中选择发送空值、透传或指定国家
- OpenAI OAuth 同步审核默认优先使用有权限的 `codex-auto-review`，可在「系统设置 → 上游配置 → 模型映射」中关闭「专用审核模型」

## 优化改进

- 账号模型白名单可快速添加图片模型，账号额度页区分定时预热和额度重置后的预热
- 账号授权或凭据刷新失败时，管理端显示更具体的原因

## 问题修复

- OpenAI Responses 回退到 HTTP/SSE 后，继续使用已失效的响应 ID 会返回 `previous_response_not_found`，客户端可移除旧 ID 并携带完整历史重试
- OpenAI OAuth 配额刷新识别到访问令牌被撤销时会使其过期，刷新模型目录时保留凭据错误原因

## 升级须知

- 从正式版 `v3.53.1` 可直接升级，已验证分组、密钥和运行设置保留。运行过旧 `0045_account_group_policies` 或 `0046_openai_request_settings` 的源码部署无法直接复用该数据库，需从执行旧迁移前的备份升级，或另行迁移业务数据
- 自建管理 API 客户端保存运行设置时，须提交 `resetCreditOnLowWeeklyQuota`、`openaiOauthGuardianReviewModel` 和 `openaiRequestCountry`，可从当前设置读取并原样回传
- 管理 API 的账号代理绑定改用 `outboundProxyId`，全局和密钥的客户端身份配置分别改用 `providerRequestProfiles` 和 `providerRequestProfileOverrides`
- 读取仪表盘账号用量的客户端请改用 `metric`、`metricValueRaw` 和 `lastUsedRelative`，需要原始时间时读取 `lastUsedAt`
