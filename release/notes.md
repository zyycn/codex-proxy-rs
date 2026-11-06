# v3.42.0

## 新增功能

- 并发排队：Key 和账号可分别设置超时，OpenAI 或 xAI 账号队满或超时时可尝试换号一次（默认关闭）
- Codex OAuth：可在账号的「额度重置」中开启「自动使用点数」（默认关闭），套餐额度用尽且上游确认点数可用、无支出限制时生效
- OpenAI Key：可设置安装身份种子，让每把 Key 在每个账号上使用独立、稳定的安装身份

## 优化改进

- 系统更新：按通道查询，无需翻页；新版本公开且当前平台的安装包和校验文件齐全后才提示升级
- 安装更新：遇到 GitHub 限流时显示可重试时间；安装包下载后，校验和安装无需再访问 GitHub
- OpenAI 调度：大账号池选号和排队时减少数据库与 Redis 读取
- OpenAI 优先亲和：主账号繁忙时临时分流，失效或额度长期耗尽时改绑接单的账号
- OpenAI 会话：优先亲和模式下，未提供会话 ID 的 Chat 和 Responses 请求也会尽量复用同一账号
- 账号分组：悬停或点击色块即可查看组名和停用状态

## 问题修复

- 修复部分 Chat 客户端通过 Codex OAuth 转发时，默认采样参数或 `stream_options.include_usage` 导致请求返回 400 的问题
- 修复编辑模型限制时，取消勾选的自定义模型在保存前就从列表消失的问题

## 升级须知

- **源码部署：** 执行过中间版 `main` 的 `0039_queue_timeouts.sql` 时，请先在独立数据库验证迁移，勿直接覆盖升级或修改校验和（从正式版 v3.41.2 升级不受影响）
- **排队设置：** Key 与账号超时各自继承原值，先后排队的总等待时间可能变长
- **管理 API 和插件 SDK：** 将 `concurrencyWaitTimeoutSeconds` 换为 `keyQueueTimeoutSeconds` 和 `accountQueueTimeoutSeconds`，并传入换号开关 `switchAccountOnQueueFailure`
- **已设置安装 ID 的 Key：** 原值自动转为种子，API 字段由 `installationId` 改为 `installationSeed`；出站身份、会话和缓存标识随之变化，已有 WebSocket 精确续写需重发完整上下文
- **自定义更新仓库：** 用 `update-index` 预发行页指向版本 Release 的 `version.yaml` 附件，本仓库的发布工作流会自动维护
