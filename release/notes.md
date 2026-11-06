# v3.42.0

## 账号调度与额度

- Key 和账号可以分别设置排队超时。开启「尝试换号」后，OpenAI 或 xAI 账号队满或超时时，网关会再找一次能立即接单的账号；没有空位则返回原错误。Key 队列、固定账号、原生续写和必须跟随主会话的子线程不会因此换号
- OpenAI 调度减少了大号池选号和排队时的数据库与 Redis 读取。优先亲和模式下，主账号短时繁忙时临时分流，主账号失效或额度长期耗尽时，在后备账号接单后迁移绑定。没有显式会话 ID 的 Chat 和 Responses 请求，也可在同一 Key 下按缓存键或对话开头匹配账号；宽松模式仍按普通策略选号
- Codex OAuth 账号的「自动使用点数」默认关闭。需要在套餐额度耗尽后继续使用点数时，请在「账号管理 → 展开账号 → 额度重置」为对应账号开启。只有上游确认点数可用且没有支出限制时才会继续调度

## OpenAI 请求与安装身份

- 修复部分 Chat 客户端经 Codex OAuth 转发时，因携带默认采样参数或 `stream_options.include_usage` 而收到 400 的问题。旧式 token 上限会转为 `max_output_tokens`；显式截断、非空停止序列等策略仍交由上游校验。新增的字段处理仅用于 OAuth
- OpenAI Key 可配置安装身份种子。同一把 Key 使用同一账号时安装身份保持稳定；换 Key 或换账号时使用不同身份。未配置种子的 Key 继续使用账号原有身份

## 系统更新与管理端

- 检查更新时按通道读取已发布版本，不再逐页遍历 GitHub Release。查看详情或下载时才查询对应 tag；遇到 GitHub 限流时显示可重试时间。安装包下载后，校验、安装和重启不再需要访问 GitHub
- 编辑账号模型限制时，取消勾选的自定义模型会保留到本次编辑结束，可以在保存前重新勾选。账号分组色块支持悬停或点击查看组名及停用状态

## 升级事项

- 从 v3.41.2 升级时，两层排队超时都继承原值，换号默认关闭。两层独立计时，先后在两层排队的总等待时间可能变长。通过管理 API 或插件 SDK 更新运行设置时，用 `keyQueueTimeoutSeconds`、`accountQueueTimeoutSeconds` 和 `switchAccountOnQueueFailure` 替换 `concurrencyWaitTimeoutSeconds`
- 已配置的 OpenAI Key 安装 ID 会自动转为种子；管理 API 调用方请将 `installationId` 改为 `installationSeed`。这些 Key 的出站安装身份及关联会话、缓存标识会变化，已有 WebSocket 精确续写需重新发送完整上下文
- 使用自定义更新仓库的实例，请在该仓库的 `main/release/version.yaml` 维护 `published` 通道索引，并保留对应 tag 的公开 Release 和附件
- 从 v3.41.2 正式版升级只新增迁移 0039。若源码部署执行过中间版 main 的 `0039_queue_timeouts.sql`（包括随后执行过 0040 或 0041 的实例），其迁移记录与发行包不兼容。请勿直接覆盖升级或修改数据库中的校验和；应先在独立数据库验证恢复或数据迁移方案
