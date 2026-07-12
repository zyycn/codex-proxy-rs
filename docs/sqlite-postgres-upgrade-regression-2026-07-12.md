# SQLite v3 到 PostgreSQL 升级回归（2026-07-12）

## 1. 测试环境

- 最终镜像：`codex-proxy-rs:real-test-20260712`
- 镜像 ID：`sha256:308ef0e1c6dac2f83a2de30bebe85ead32b779913011c6a7bbcfc71d1aea412e`
- Compose 项目：`cpr-sqlite-upgrade-20260712`
- 隔离服务：`http://127.0.0.1:18083`
- SQLite 源库：`.runtime/sqlite-upgrade-20260712/source.sqlite`，权限 `0600`
- PG/Redis 使用独立 Docker volume，与真实账号回归环境完全隔离

源库按 SQLite v3 权威 Schema 生成，`schema_migrations.max(version)=3`。测试数据包含：

- 管理员 1、客户端 key 1、运行时设置 1；
- 账号 2，其中 active 1、历史瞬态 refreshing 1；
- Cookie 1，且故意使用无法解析的过期时间；
- usage 混合事实 3：成功 1、错误 1、噪音 1；
- 独立 ops 错误 1、历史时间桶 1；
- `account_model_usage`、管理员会话、session affinity、刷新租约、模型快照各 1。

管理员密码哈希使用有效 Argon2 值，客户端 key 使用 SQLite v3 真实生成规则 `sk_`。
报告不记录密码、哈希、完整 key、账号 token、Cookie 值或任何真实账号标识。

## 2. 实际升级命令

先只启动空的 PG/Redis，再通过最终镜像执行正式子命令：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d postgres redis
docker compose --env-file deploy/.env -f deploy/docker-compose.yml run --rm --no-deps \
  -v "$(realpath /path/to/legacy.sqlite):/tmp/legacy.sqlite:ro" \
  codex-proxy-rs codex-proxy-rs import-sqlite /tmp/legacy.sqlite
```

`docker compose run ... import-sqlite` 会覆盖镜像 `CMD`，使 `tini` 尝试执行不存在的
`import-sqlite` 文件；Docker 场景必须显式写出容器内二进制 `codex-proxy-rs`。

## 3. 导入报告

正式命令退出码为 0，报告如下：

| 分类 | 项目 | 数量 |
| --- | --- | ---: |
| 导入 | `account_cookies` | 1 |
| 导入 | `accounts` | 2 |
| 导入 | `admin_users` | 1 |
| 导入 | `client_api_keys` | 1 |
| 导入 | `ops_error_logs` | 2 |
| 导入 | `request_time_buckets` | 1 |
| 导入 | `runtime_settings` | 1 |
| 导入 | `usage_records` | 1 |
| 规范化 | `accounts.refreshing_to_expired` | 1 |
| 丢弃 | `account_model_usage` | 1 |
| 丢弃 | `account_refresh_leases` | 1 |
| 丢弃 | `admin_sessions` | 1 |
| 丢弃 | `model_plan_snapshots` | 1 |
| 丢弃 | `session_affinities` | 1 |
| 丢弃 | `usage_record_noise` | 1 |
| 降级 | Cookie 过期时间解析失败 | 1 |

## 4. PostgreSQL 数据核对

- PG 迁移为 `1 / initial`，checksum 长度 64。
- 账号为 active 1、expired 1；`refreshing=0`。
- 原 refreshing 账号已经变为 expired，且 `next_refresh_at is null`。
- 完整客户端 key 与 prefix 原值保留，空 key 数为 0，迁移后 Bearer 鉴权成功。
- 管理 API key 只保存哈希，旧明文通过 `x-api-key` 验证成功。
- 运行时设置保持 smart、并发 3、请求间隔 50 ms；保留期补齐为 30/30/90 天。
- Cookie 行保留，坏过期时间按设计降级为 `expires_at is null`。
- 成功事实提升字段正确：requested/upstream model、service tier、首字延迟及 12/5/3/2 token。
- ops 最终为 2 条，状态分别为 429、502。
- 所有运行态表均未进入 PG，Redis 启动前为空。

## 5. 桶重建

执行正式 `rebuild-buckets`：

```text
deleted=1, rebuilt=2
```

重建后汇总为：成功 1、错误 2、input 12、output 5、cached 3；与拆分后的
`usage_records` 和 `ops_error_logs` 完全一致，不再沿用 v3 混合桶的旧口径。

## 6. 运行链路

完成迁移核对后，将唯一 active 的合成账号临时改为 disabled，避免假 token 请求真实上游；
此操作发生在迁移结果验收之后，不属于迁移逻辑。

| 检查 | 结果 |
| --- | --- |
| `/healthz` | 204 |
| 管理员密码重新登录 | 200，Redis 创建新会话 |
| 旧管理 API key | `x-api-key` 返回 200 |
| 旧客户端 key | `/v1/models` 返回 200，8 个模型 |
| 账号列表 | 2 行，分页 total=2 |
| 客户端 key 列表 | 1 行，完整 key 可取回且 enabled |
| usage 列表 | 1 条成功事实，状态 200 |
| ops 列表 | 2 条错误事实，状态 429/502 |
| Dashboard summary | 200，统计与 smart 策略可读取 |
| Settings | smart、3600/2/3/50 均与 v3 一致 |
| Redis 运行态 | 只有新管理员会话；affinity/lease/model snapshot 均为 0 |

## 7. 边界与持久化

- 对同一目标库再次执行导入，命令以 `TargetNotEmpty` 失败，所有表行数保持不变。
- 自动化测试同时覆盖未知源版本、坏时间戳全事务回滚和空管理 API key。
- 强制重建应用、PG、Redis 容器但保留 volume 后，账号 2、key 1、usage 1、ops 2、bucket 2、Cookie 1 全部保留。
- 重建后旧管理员会话仍有效，旧客户端 key 仍能访问 `/v1/models`。
- 最终三个容器均 healthy；应用日志只有 INFO，PG/Redis warning/error 均为 0。

## 8. 结论

SQLite v3 到 PG v1 的一次性升级链路可以正常使用，数据变换、显式丢弃、桶重建、
管理端读取、旧 key 鉴权和容器持久化均符合 `database.md`。Docker 用户必须使用文档中的
显式二进制命令，并严格遵守“先导入、后启动应用”的顺序。
