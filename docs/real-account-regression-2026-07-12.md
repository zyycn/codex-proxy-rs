# 真实账号回归报告（2026-07-12）

## 1. 范围与环境

- 分支：`feat/postgres-redis-migration`
- 基线提交：`00fb1465b05bfbddb03a2463207066d89b17c9fe`
- 真实账号：2 个，均为 `active / k12`，账号文件权限已收紧为 `0600`
- 共同可用模型：7 个；回归模型为 `gpt-5.6-sol`
- 镜像：`codex-proxy-rs:real-test-20260712`
- 镜像 ID：`sha256:308ef0e1c6dac2f83a2de30bebe85ead32b779913011c6a7bbcfc71d1aea412e`
- 镜像大小：41,127,442 bytes
- 隔离服务：`http://127.0.0.1:18082`
- Compose 项目：`cpr-real-20260712`

本文只记录聚合结果。账号 ID、邮箱、access/refresh token、Cookie、客户端密钥、
response ID、会话 ID 和上游 request ID 均未写入报告。

## 2. 质量门禁

| 门禁 | 结果 |
| --- | --- |
| Rust 格式 | `cargo fmt --all -- --check` 通过 |
| Rust 静态检查 | Clippy `--all-targets --all-features -D warnings` 通过 |
| 后端测试 | 658 passed，0 failed，0 ignored |
| 前端格式 | Prettier 通过 |
| 前端类型与生产构建 | `vue-tsc -b && vite build` 通过，2,837 modules transformed |
| GitHub Actions | Actionlint 通过 |
| Rust 依赖 | RustSec 扫描 339 个 crate，0 漏洞 |
| 前端依赖 | pnpm production audit，0 漏洞 |
| 最终镜像 | Trivy HIGH/CRITICAL、OS/library、ignore-unfixed：0 |
| Compose | `docker compose config --quiet` 通过 |

## 3. 真实请求矩阵

| 场景 | 结果 |
| --- | --- |
| 新库启动、迁移、PG/Redis 健康检查 | 通过；迁移为 `1 / initial`，checksum 64 字符 |
| 管理端登录、状态、`Cache-Control: no-store` | 通过 |
| 模型快照 | 两个账号均返回相同的 7 个模型 |
| HTTP SSE | 200，包含 created/completed/[DONE]，输出完整 |
| WebSocket 转 SSE | 200，包含 created/completed/[DONE]，输出完整 |
| 同连接 `previous_response_id` 续接 | 通过 |
| 应用重启后的托管会话续接 | 通过；首次续接不可用后完整 replay，`attemptCount=2` |
| 日常多轮对话 | low/medium/high/max 均覆盖；计划、修改计划、摘要等场景通过 |
| 会话 owner 被禁用后换号 | 通过；按运行时调度策略选择其他账号，`account_switched=true` |
| 外部未知 previous response | 只请求一个账号；原样返回 400，未遍历账号池 |
| 智能、额度重置优先、轮询、粘性策略 | 全部实际切换并完成；轮询连续请求命中两个不同账号 |
| 4 路并发 | 全部 200/completed，两个账号各承载 2 路 |
| 客户端取消 | 客户端超时后下一请求仍成功，健康检查仍为 204 |
| 客户端密钥禁用/启用 | 禁用后 401，恢复后 200；`last_used_at` 已持久化 |
| 无密钥、错误密钥、未知模型、坏 JSON、非对象 JSON、超大请求体 | 分别按契约返回 401/404/400/413 |
| Codex `input` 字符串 | 上游原样返回 400 `Input must be a list`；改为标准 item 列表后 200 |
| 全账号禁用 | 返回 `no_available_accounts` 终止事件，未向上游穿透；恢复后正常 |
| 非法账号导入 | 400，账号总数保持 2，未留下半初始化记录 |
| SPA 首页与静态资源 | 200 |
| PG/Redis 卷重建 | 数据、会话、密钥和 affinity 均保持；重启后真实请求 200 |

## 4. PostgreSQL 数据状态

最终持久化数据如下：

| 表 | 行数 | 核对结果 |
| --- | ---: | --- |
| `schema_migrations` | 1 | `initial`，checksum 长度 64 |
| `accounts` | 2 | 均为 active/k12；无 quota/Cloudflare 冷却 |
| `account_usage` | 2 | 与账号一一对应，无孤儿 |
| `account_cookies` | 0 | 风控清理后无遗留 Cookie |
| `admin_users` | 1 | 唯一管理员记录 |
| `client_api_keys` | 1 | enabled，且已有 `last_used_at` |
| `fingerprints` | 1 | 当前指纹 |
| `fingerprint_update_history` | 1 | 更新历史可查询 |
| `runtime_settings` | 1 | 策略已恢复为 smart |
| `usage_records` | 21 | 21 个不同 request ID，全部为真实成功请求 |
| `ops_error_logs` | 11 | 11 个不同 request ID，均为预期边界/上游失败事实 |
| `request_time_buckets` | 10 | 成功/失败与两张事实表精确一致 |

### 4.1 成功事实与账号统计

- 成功明细：21；HTTP SSE 1，WebSocket 20；涉及 2 个账号、1 个模型。
- 推理强度：high 2、low 11、medium 3、max 1、未显式指定 4。
- token：input 2,340、output 1,917、cached 0、reasoning 596、合计 4,257。
- 账号累计请求尝试：32；空响应 0。它包含失败尝试，因此大于 21 条成功事实。
- 平均端到端延迟 4,473 ms；平均首字延迟 2,586 ms。

### 4.2 错误事实与统计桶

- 400/400/400：HTTP SSE 3、WebSocket 4，均为上游请求级错误。
- HTTP 已提交后捕获的上游 400：WebSocket 2，客户端状态按实际响应保持 200。
- `no_available_accounts`：2，客户端状态 503，未产生上游状态。
- 桶合计：`success_count=21`、`error_count=11`，分别等于成功与错误事实表行数。
- 桶 token 合计与 `usage_records` 完全一致；负计数、`min_latency > max_latency` 均为 0。

### 4.3 一致性、容量与保留策略

- 账号用量、Cookie、成功/错误账号引用、客户端密钥引用的孤儿数均为 0。
- 未验证约束、无效索引、未 ready 索引均为 0。
- 数据库约 9.12 MB；12 张业务表连同索引约 0.92 MB。
- 保留周期：usage 30 天、ops 30 天、bucket 90 天。
- 已分别插入 31/31/91 天的隔离标记并重启任务，三类过期标记均从 1 清理为 0。

## 5. Redis 数据状态

- 总键数 42；STRING 23、ZSET 18、HASH 1。
- 41 个键带 TTL；唯一常驻键为 `models:plan_snapshots` HASH。
- Redis 逻辑数据约 36 KB；单键最大约 6.3 KB。
- affinity：response 21、conversation 16、account 2。
- 管理会话 2，均有 TTL；模型计划 field 1；刷新 lease 与登录节流残留均为 0。
- replay 最大深度 4、最大累计字节 3,068、最大节点值 1,671 bytes。
- response 到 conversation/account 的缺失成员均为 0；父链缺失和索引孤儿均为 0。
- replay 无 TTL、无效 JSON、敏感凭据和 `encrypted_content` 命中均为 0。
- AOF 已启用；AOF 写入、重写和 RDB 保存状态均为 `ok`。

## 6. 测试中发现并修复

1. PostgreSQL Alpine 初始化存在 locale/trust 警告：改为固定 digest 的 Bookworm，显式使用 SCRAM。
2. 可恢复的上游失败在最终成功前被提前记录为 WARN：删除中间告警，终态事件唯一负责级别。
3. 流提交前的上游 400 被写成 `client_status_code=200`：改为记录实际返回状态；提交后的流仍保持 200。
4. 缺失、错误或禁用客户端密钥属于预期请求结果却写 WARN：改为 INFO；存储不可用仍为 WARN。

## 7. 最终运行态

- 应用、PostgreSQL、Redis 三个容器均为 healthy。
- 应用以 `10001:10001` 运行，使用最终镜像 ID。
- 三个容器日志轮转均为 `json-file / 10m / 5`。
- 最终重建后的应用日志只有 INFO；PG 的 WARNING/ERROR/FATAL/PANIC 为 0；Redis warning 为 0。
- `/healthz` 返回 204；标准 Codex item-list 请求返回 200/completed/[DONE]。

## 8. 有意不执行的破坏性场景

- 未故意损坏或消费真实 refresh token，未主动制造封号、Cloudflare 风控或 429。
- 未对真实账号执行高频压测或上千账号遍历，避免污染账号环境和额度。
- 未破坏 PG 数据文件、Redis AOF 或模拟宿主机磁盘写满。

上述错误分类、账号轮换、全候选遍历、PG/Redis 写失败降级、并发槽位和 WebSocket 池取消安全，
均由 658 项集成测试中的确定性故障注入覆盖。
