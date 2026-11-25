# v3.55.0

## 新增功能

- 接入 Antigravity OAuth 账号，支持授权、导入导出、模型目录、额度查询与本地费用估算
- 支持 Antigravity 原生 GenAI 生成、流式输出和 Token 计数，并可通过 Responses、Chat Completions 与 Messages 调用
- 新增「按账号与会话匿名化工作区」隐私预设，打乱工程名并附加数字，换账号或会话时派生新化名，同时移除 Git 远端与提交 metadata

## 优化改进

- 账号亲和设置统一应用于 OpenAI、xAI 和 Antigravity，支持宽松、优先与严格模式
- 备份设置不再回传已保存的密钥，保存与连接测试按配置版本校验
- 请求列表缺少上游耗时时，显示带 `≈` 的本地平均输出速率

## 问题修复

- 修复 OpenAI 耗尽窗口丢失、重置时间无法对应或持续后移时的额度恢复判断
- 修复上游 WebSocket 控制连接过期后，客户端未收到重连提示的问题
- 修复调整模型价格或轮换管理密钥后，运行设置草稿被误判为冲突的问题

## 升级须知

- 已有亲和设置自动迁移，Admin API 字段 `openaiAccountAffinity`、`openaiSessionAffinityTtlHours` 改为 `accountAffinity`、`sessionAffinityTtlHours`
- 备份管理接口写入时须携带 `storageRevision`，读取密钥配置状态使用 `secretConfigured`，保存时省略 `secretAccessKey` 保留原值
- 账号导出统一使用 `label`、`createdAt`、`updatedAt`、`accessTokenExpiresAt` 和 `scopes` 字段，xAI 凭据位于 `credentials` 对象，直接解析导出文件的工具需同步适配
- Antigravity 所有生成入口使用服务端 AGY 系统提示词配置替换客户端系统指令，转换入口不支持 `previous_response_id` 续接
- 未发布的迁移 `0046～0048` 合并为 `0046`，已执行这些迁移的源码部署实例需先核对迁移历史，正式版升级不受影响
