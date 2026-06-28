# 删除代码功能等价审计

审计日期：2026-06-28

对照基准：

- 当前 Rust 工程：`/home/zyy/Codes/codex-proxy-rs`
- TS 参考工程：`/home/zyy/桌面/Codes/codex-proxy`

本文档只审计近期删除或收敛的功能是否造成产品能力缺失。结论不以文件名为准，而以当前路由、服务实现和 TS 版已存在能力为准。

## 总体结论

当前删除并不全是问题。部分删除符合当前产品方向：后台设置减少人工配置、日志策略固定、接口去掉路径 id、账号导入增强为 CPR/Sub2API 双格式并支持重复账号更新，这些属于合理收敛。

但有几类能力被删后没有等价替代，会影响管理闭环：

1. 账号导出缺失，导入和备份迁移不闭环。
2. 批量账号状态更新缺失，账号池批量维护效率下降。
3. 批量健康检查缺失，目前只剩单账号模型测试。
4. 手动 Cookie 维护缺失，Cloudflare 人工旁路能力不完整。
5. 独立用量统计接口缺失，当前只被 Dashboard 和账号列表部分吸收。
6. 手动模型刷新缺失，后台只剩启动后和周期任务刷新。
7. 客户端 API Key 导出缺失，密钥备份/迁移能力被削弱。

## 当前 Rust 管理端路由事实

当前 `src/admin/router.rs` 保留的管理端接口：

- 登录会话：`/api/admin/login`、`/api/admin/auth/status`、`/api/admin/logout`
- 设置：`/api/admin/settings`
- 概览：`/api/admin/dashboard/summary`、`/api/admin/dashboard/trend`
- 账号：`/api/admin/accounts`、`/api/admin/accounts/import`、`/api/admin/accounts/oauth/authorize`、`/api/admin/accounts/oauth/exchange`、`/api/admin/accounts/test`、`/api/admin/accounts/models`、`/api/admin/accounts/delete`、`/api/admin/accounts/update`、`/api/admin/accounts/refresh`、`/api/admin/accounts/quota`
- 日志：`/api/admin/logs`、`/api/admin/logs/delete`、`/api/admin/logs/detail`
- 客户端 API Key：`/api/admin/keys`、`/api/admin/keys/delete`、`/api/admin/keys/update`

已从路由层移除的旧 Rust 管理端接口：

- `/api/admin/models/refresh`
- `/api/admin/usage`
- `/api/admin/usage/summary`
- `/api/admin/accounts/export`
- `/api/admin/accounts/quota-warnings`
- `/api/admin/accounts/health-check`
- `/api/admin/accounts/reset-usage`
- `/api/admin/accounts/cookies`
- `/api/admin/logs/state`
- `/api/admin/keys/export`

## 逐项审计

| 能力 | Rust 当前状态 | TS 参考状态 | 结论 | 建议 |
| --- | --- | --- | --- | --- |
| 账号导入 | 已保留并增强。`src/admin/accounts/service/importing.rs` 支持 CPR/Sub2API，重复账号会按 ChatGPT identity 更新，缺少字段时会补充 account id、user id、email、quota。 | `src/routes/accounts.ts` 支持 `/auth/accounts/import`。 | 不缺失。当前 Rust 实现比旧导入更贴近当前产品。 | 保留当前实现。 |
| 账号导出 | 当前路由已删除 `/api/admin/accounts/export`，旧 `export_accounts` 和 `export_with_tokens` 不再暴露。 | `src/routes/accounts.ts` 有 `/auth/accounts/export`，支持按 ids 和 format 导出。 | 功能缺失。导入存在但导出消失，备份、迁移、批量排障不闭环。 | P1 恢复，格式只保留 CPR，避免旧 native 命名和兼容分支。 |
| 批量删除账号 | 当前保留 `/api/admin/accounts/delete`。 | TS 有 `/auth/accounts/batch-delete`。 | 不缺失。 | 保留。 |
| 批量状态更新 | 当前 `/api/admin/accounts/update` 只解析单账号 `id` 更新，旧 `batch_update_status` 已删除。 | TS 有 `/auth/accounts/batch-status`。 | 功能缺失。批量禁用、恢复账号池时需要逐个操作。 | P1 恢复为 `/api/admin/accounts/update` 的批量 data 形式，保持无路径 id。 |
| 单账号刷新/额度查询 | 当前保留 `/api/admin/accounts/refresh` 和 `/api/admin/accounts/quota`，id 放在 body 或 query。 | TS 有 `/auth/accounts/:id/refresh`、`/auth/accounts/:id/quota`。 | 不缺失，且当前 Rust 路由形态更符合项目规范。 | 保留当前形态。 |
| 重置账号用量 | 当前删除 `/api/admin/accounts/reset-usage`，未发现等价管理端入口。 | TS 有 `/auth/accounts/:id/reset-usage`。 | 功能缺失但不是核心链路。主要影响调试、演示和清理异常统计。 | P3 低优先级恢复，或明确产品不提供手动重置。 |
| 批量健康检查 | 当前删除 `/api/admin/accounts/health-check`，只保留 `/api/admin/accounts/test?id=...` 单账号 SSE 模型测试。 | TS 有 `/auth/accounts/health-check`，支持 ids、stagger、concurrency。 | 功能缺失。单账号测试不能替代账号池健康巡检。 | P1 恢复批量健康检查，复用现有单账号探测和 loading 流程。 |
| 测试模型列表 | 当前保留 `/api/admin/accounts/models?id=...`，从上游返回模型。 | TS 模型列表能力来自模型 catalog 和上游刷新。 | 不缺失。 | 保留当前接口名 `models`。 |
| OAuth 添加账号 | 当前新增 `/api/admin/accounts/oauth/authorize`、`/api/admin/accounts/oauth/exchange`。 | TS 有 `/auth/accounts/login` 和 OAuth 链路。 | 不缺失。Rust 当前已按后台弹窗流程实现。 | 保留。 |
| 手动 Cookie 维护 | 当前删除 `/api/admin/accounts/cookies`。Cookie 存储仍存在，Cloudflare 逻辑会删除账号 cookie，但没有管理端查看/写入入口。 | TS 有 `/auth/accounts/:id/cookies` GET/POST/DELETE，并在 quota 失败时提示写入 cf cookie。 | 功能缺失。遇到 Cloudflare 挑战时缺少人工修复入口。 | P2 恢复，接口保持无路径 id，例如 `GET/POST /api/admin/accounts/cookies?id=...`，删除可并入 POST action 或独立 delete。 |
| 配额告警列表 | 当前删除 `/api/admin/accounts/quota-warnings`。账号列表 summary 仍有 high_usage/attention，但没有独立 warning 列表。 | TS 有 `/auth/quota/warnings`。 | 部分缺失。列表页摘要不能替代可点击、可定位、可清理的告警集合。 | P2 视 UI 是否需要恢复；如果不做告警中心，应在产品上明确删除。 |
| 独立账号用量分页 | 当前删除 `/api/admin/usage`，账号列表会内嵌账号用量，Dashboard 会读取 usage summary 和 buckets。 | TS 有 `/admin/usage-stats/summary` 与 `/admin/usage-stats/history`。 | 部分缺失。概览和账号列表替代了展示，但没有独立用量页/API。 | P2 如果后续有用量分析页，应恢复为 `/api/admin/usage` 和 `/api/admin/usage/summary`；否则可接受删除。 |
| 用量历史 | 当前 Dashboard 趋势读 `usage_time_buckets`，但没有通用 history API。 | TS 有 `/admin/usage-stats/history`，支持 granularity 和 hours。 | 部分缺失。当前只服务 Dashboard，不适合复用到独立分析页面。 | P2 如要开放分析视图，新增 `/api/admin/usage/history`，直接读聚合事实表。 |
| 手动模型刷新 | 当前删除 `/api/admin/models/refresh`，但 `ModelRefreshTask` 会启动后和每小时刷新，并有 `refresh_once()`。 | TS 有 `/admin/refresh-models` 手动触发。 | 功能缺失但已有后台替代。问题在于管理端无法立即刷新模型目录。 | P2 恢复轻量接口，直接调用现有 `refresh_once()` 或后台任务句柄，避免再引入 `admin_models` 旧模块。 |
| OpenAI 模型列表 | 当前 `/v1/models`、`/v1/models/catalog`、`/v1/models/{model}`、`/v1/models/{model}/info` 仍存在。 | TS 有同类模型路由。 | 不缺失。 | 保留。 |
| 日志列表/详情/清空 | 当前保留 `/api/admin/logs`、`/api/admin/logs/detail`、`/api/admin/logs/delete`。 | TS 有 `/admin/logs`、`/admin/logs/:id`、`/admin/logs/clear`。 | 不缺失。Rust 当前筛选字段更丰富。 | 保留当前形态。 |
| 日志开关状态 | 当前删除 `/api/admin/logs/state`。 | TS 有 `/admin/logs/state` GET/POST。 | 不算缺失。此前产品已决定日志采集和容量走默认策略，不在系统设置暴露。 | 不恢复。 |
| 设置项大而全 | 当前 `/api/admin/settings` 只保留 modelAliases、modelAccountRoutes、refreshMarginSeconds、refreshConcurrency、maxConcurrentPerAccount、requestIntervalMs、rotationStrategy。 | TS `general-settings`/`quota-settings` 暴露端口、TLS、默认模型、日志容量、更新策略等大量字段。 | 不算缺失。当前产品已收敛为只保留需要人工决策的运行配置。 | 不恢复 TS 大而全设置页。 |
| 客户端 API Key 列表/创建/更新/删除 | 当前保留 `/api/admin/keys`、`/api/admin/keys/update`、`/api/admin/keys/delete`，并保存完整 `sk_` key 供复制。 | TS 的 `/auth/api-keys` 是第三方 provider key pool，领域不同。 | 当前核心能力不缺失。 | 保留当前“客户端访问密钥”定位，不混入 provider key pool。 |
| 客户端 API Key 导出 | 当前删除 `/api/admin/keys/export`。旧 Rust 实现曾导出 `rustLocalClientApiKeys` 并标记 `rotation_required`。 | TS provider key pool 有 `/auth/api-keys/export`。 | 功能缺失但需谨慎。当前项目已允许保存完整 key，导出可以服务备份；但也会扩大密钥泄露面。 | P3 如恢复，只做管理员显式导出，并使用 CPR 格式；不恢复旧 `rustLocalClientApiKeys` 命名。 |

## 不建议恢复的删除项

以下删除符合当前项目方向，不建议回滚：

1. `/api/admin/logs/state`：日志策略已固定，系统设置不再暴露日志采集和容量。
2. TS 版 `general-settings` 中的默认模型、默认推理强度、默认 service tier：这些应由客户端请求决定，网关不兜底。
3. TS 版模型 aliases 的旧配置形态：当前 Rust 已拆成 `modelAliases` 和 `modelAccountRoutes`，边界更清楚。
4. 路径中携带 id 的管理端接口形态：当前 Rust 统一使用 query/body 传 id，更符合近期接口规范。
5. 第三方 provider API Key pool：当前 Rust 的 API Key 是客户端访问密钥，不应混入 TS 版 provider key pool 概念。

## 建议恢复优先级

P1：应该优先补齐，直接影响管理闭环。

- 账号导出：`GET /api/admin/accounts/export`
- 批量账号状态更新：复用 `/api/admin/accounts/update`
- 批量健康检查：`POST /api/admin/accounts/health-check`

P2：影响运维体验和可诊断性，建议排第二批。

- 手动 Cookie 维护：`GET/POST /api/admin/accounts/cookies`
- 配额告警列表：`GET /api/admin/accounts/quota-warnings`
- 独立用量统计和历史：`GET /api/admin/usage`、`GET /api/admin/usage/summary`、可选 `GET /api/admin/usage/history`
- 手动模型刷新：`POST /api/admin/models/refresh`

P3：低频能力，可先产品确认。

- 重置账号用量：`POST /api/admin/accounts/reset-usage`
- 客户端 API Key 导出：`GET /api/admin/keys/export`

## 恢复原则

如果恢复上述能力，不应该把旧代码原样搬回来。需要按当前项目规范重写：

1. 不在路径中放 id，id 统一放 query 或 body。
2. 不恢复兼容命名，例如 `native`、`rustLocalClientApiKeys`。
3. 不恢复大而全的 settings/config 写回逻辑。
4. 不为了兼容旧数据增加迁移分支；已有开发库需要重建。
5. 账号导出格式只使用 CPR，和当前导入格式闭环。
6. 批量操作必须返回 `updated/deleted/notFound` 这类可被 UI 准确呈现的数据。
7. 涉及真实网络请求的操作必须有后端测试覆盖，包括成功、部分失败、空 ids、未登录。

## 需要补的测试

当前被删除的测试里包含 `tests/admin/models/*`、`tests/admin/keys/import_export.rs`、旧账号 import/export 测试等。若恢复能力，至少补回以下测试面：

- 账号导出：全部导出、按 ids 导出、无效格式、未登录。
- 批量状态：部分 id 不存在、空 ids、非法 status、状态写入后账号列表可见。
- 批量健康检查：成功、失败、跳过、并发参数校验。
- Cookie 维护：写入、读取、清空、账号不存在。
- 用量接口：分页、汇总、空数据库。
- 模型刷新：无账号、部分计划失败、成功刷新并写入模型快照。
- API Key 导出：未登录、空列表、导出字段包含完整 key。
