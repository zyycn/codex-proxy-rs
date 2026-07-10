# 日志与增长数据审查

> 审查基线：`5150eb7a`，分支 `feat/postgres-redis-migration`。
>
> 状态：2026-07-11 已完成代码实施、全量质量门禁与容器运行验收。
>
> 范围：应用 tracing、HTTP 访问日志、Docker 容器日志、PostgreSQL stderr/WAL，以及 PostgreSQL 日志表与统计表的增长边界。

## 1. 结论

审查基线的日志系统具备结构化、脱敏和按日文件轮转基础，但有四个需要优先修复的问题：

1. 应用只写文件，`docker logs` 为空；关闭文件日志会同时关闭全部 tracing。
2. 文件 writer 使用 `tracing_appender` 默认 lossy 模式，缓冲满时会静默丢日志，当前没有上报丢弃计数。
3. HTTP 请求日志字段重复，5xx 同时产生 completed 和 failed 两条终态事件，`/healthz` 每 30 秒产生两条无价值 INFO。
4. Docker 的应用、PostgreSQL、Redis 都使用无大小上限的 `json-file` 日志。

PostgreSQL 三张主要增长表已有按时间保留策略；基线中 `account_model_usage` 和 `fingerprint_update_history` 没有严格上限。本轮已关闭全部四项日志问题并补齐两张表的 100 行硬上限。

## 2. 已核实的改造前现状

### 2.1 应用日志

- `infra/logging.rs::init_tracing` 只安装 JSON 文件 layer，没有 stdout layer。
- Docker 实测 `docker logs codex-proxy-rs` 为 0 行，应用日志全部写入 `.runtime/logs`。
- `logging.enabled: false` 在 `bootstrap/services.rs::init_logging` 直接返回，不安装 subscriber，实际语义是关闭全部 tracing。
- 当前 `backend/src` 日志宏数量：
  - `error!`：0
  - `warn!`：70
  - `info!`：30
- `RUST_LOG` 是唯一过滤入口；配置文件没有日志级别字段。
- `tracing_appender::non_blocking` 默认 `lossy=true`。缓冲满时事件会被丢弃，代码没有保留或监控 `ErrorCounter`。
- 文件按中国自然日命名，但 `retention_days` 实际保留最近 N 个文件，不是严格按自然日截止。
- 单个自然日文件没有大小上限。

### 2.2 HTTP trace

- span 已包含 `request_id`、method、脱敏后的 URI。
- received 事件再次写入上述三个字段，JSON 的 event 与 span 重复。
- `ClientIp` 已由请求上下文解析，但没有进入 span。按产品决定，继续沿用现有自动解析转发头的语义，不增加可信代理配置。
- `/healthz` 位于 `http_trace_layer` 内；Docker 每 30 秒探测一次，每次产生 received + completed 两条 INFO，约 5760 条/天。
- `Span::none()` 不能跳过 TraceLayer callback，因此不能作为 healthz 降噪方案。
- `ServerErrorsAsFailures` 对 5xx 同时调用 `on_response` 与 `on_failure`，当前会记录两条终态事件。

### 2.3 敏感信息与结构化字段

- 已抽查的认证、Cookie 和请求 URI 路径没有直接记录 key、token 或 Cookie 值。
- query value 全部替换为 `<redacted>`。
- JSON layer 已包含 level、target、file、line、thread、current span 和 span list。
- “敏感信息零泄露”只能作为抽查结论，后续新增日志仍必须逐项审查字段和错误字符串。

### 2.4 PostgreSQL 日志与 WAL

当前容器实测：

| 项目 | 当前值 | 增长语义 |
| --- | --- | --- |
| `logging_collector` | `off` | PG 不在数据卷内生成日志文件 |
| `log_destination` | `stderr` | 全部进入 Docker `json-file` |
| Docker LogConfig | `json-file`,无 options | 当前没有大小上限，必须修复 |
| `max_wal_size` | `1GB` | checkpoint 的软目标，不是绝对硬上限 |
| `min_wal_size` | `80MB` | 当前 WAL 实测约 80MB |
| `wal_keep_size` | `0` | 不额外保留 WAL |
| `archive_mode` | `off` | 不产生无限归档目录 |
| replication slots | `0` | 当前没有 slot 阻止 WAL 回收 |

结论：PostgreSQL 日志本身不在 PG volume 中增长，当前风险是 Docker `json-file` 无上限。WAL 在目前无归档、无复制槽的部署下会被 checkpoint 回收，不需要另写删除任务；运维文档必须说明复制槽和归档开启后会改变该结论。

### 2.5 PostgreSQL 日志表与统计表

`RetentionTrimTask` 启动时立即执行，之后每小时执行一次，每轮从 `runtime_settings` 读取保留天数。

| 表/数据 | 当前边界 | 结论与处理 |
| --- | --- | --- |
| `usage_records` | 默认 30 天 | 已按 `created_at` 删除，不会按运行年限无限增长 |
| `ops_error_logs` | 默认 30 天 | 已按 `created_at` 删除，不会按运行年限无限增长 |
| `request_time_buckets` | 默认 90 天 | 已按 `bucket_start` 删除，不会按运行年限无限增长 |
| `account_cookies` | 按 `expires_at` 周期清理 | 有过期时间的 Cookie 有边界；会话 Cookie 受唯一键约束但没有时间保留期 |
| `account_usage` | 每账号一行，账号删除级联 | 行数上限等于账号数，不随请求数增长 |
| `account_model_usage` | 基线为每账号/模型一行 | 已新增每账号最多保留最近 100 个模型的硬上限 |
| `fingerprints` | 当前指纹固定 ID 覆盖更新 | 常量级 |
| `fingerprint_update_history` | 基线为每次实际更新插入一行 | 已新增全局最多保留最近 100 条的硬上限 |

时间保留限制的是数据时间范围，不是固定字节数：高流量下 30 天事实仍可达到很大体量。当前 PostgreSQL `autovacuum=on`，会回收 DELETE 后的页面供后续复用，但不会保证数据文件立即缩小；不应周期执行会锁表的 `VACUUM FULL`。

## 3. 已确认的目标设计

### 3.1 配置模型

不保留旧 `logging.enabled/directory/retention_days` 平铺结构，改为职责明确的配置：

```yaml
logging:
  level: info
  stdout: true
  file:
    enabled: true
    directory: /app/logs
    retention_days: 14
    max_file_size_mb: 20
    max_files: 20
```

- `RUST_LOG` 存在时优先于 `logging.level`。
- stdout 与文件使用相同 JSON 字段和中国时区时间戳。
- 允许显式关闭任一输出；默认至少启用 stdout。
- 文件名第一段为 `codex-proxy-rs.YYYY-MM-DD.log`，超限后使用递增 segment。
- 清理同时执行自然日截止与全局文件数上限，单个文件限制 20MB，总文件数限制 20。

### 3.2 non-blocking 丢弃监测

- stdout 与文件 writer 都使用 non-blocking，避免在 Tokio 请求线程执行阻塞 IO。
- 保持 lossy 模式，避免日志系统反压拖垮代理热路径。
- `LogGuard` 保留两个 writer 的 `ErrorCounter` 和 `WorkerGuard`。
- 独立监测线程定期读取计数；计数增加时直接向 stderr 写一条紧急 JSON 告警，避免重新进入 tracing 形成递归。
- 进程退出和自更新 exec 前停止监测线程，再 drop writer guards 完成 flush。

### 3.3 Docker 日志上限

三个服务统一配置：

```yaml
logging:
  driver: json-file
  options:
    max-size: 10m
    max-file: 5
```

每个容器 Docker 日志理论上限约 50MB。PostgreSQL 继续使用 stderr，不启用 `logging_collector`，避免再次双写到 PG 数据卷。

### 3.4 HTTP 日志

- `/healthz` 路由置于 `http_trace_layer` 外；失败时 handler 自身仍记录 WARN。
- received 事件降为 DEBUG，只写消息，字段从当前 span 继承。
- span 增加当前已解析的 `client_ip`。
- 2xx/3xx/4xx 只产生一条 INFO completed 终态事件。
- 5xx 跳过 completed，由 `on_failure` 只产生一条 WARN failed 终态事件。

### 3.5 日志级别规则

| 级别 | 使用场景 |
| --- | --- |
| ERROR | 本地持久化失败导致事实、计数或核心状态丢失；不可恢复的进程内部异常 |
| WARN | Redis 亲和丢失、Cookie 持久化失败、上游波动、可恢复后台任务失败 |
| INFO | 启停、状态恢复、配置变更、周期任务汇总 |
| DEBUG | 单请求开始、重试细节、无状态变化的周期检查 |

分级必须逐处判断，不做 WARN 到 ERROR 的机械替换。

### 3.6 PostgreSQL 增长边界补强

- 将 `account_model_usage` 和 `fingerprint_update_history` 纳入每小时 retention task。
- `account_model_usage` 每账号保留 `last_used_at desc nulls last, model` 排序后的前 100 行。
- `fingerprint_update_history` 全局保留 `created_at desc, id desc` 排序后的前 100 行。
- 清理经各自属主域 store 执行，`bootstrap` 只负责任务编排，不直接写 SQL。
- 现有三张时间保留表继续使用 30/30/90 天策略，不增加按请求内联 DELETE。

## 4. 实施顺序与状态

本轮范围为全部 P0、P1，以及 panic hook 和启动期日志级别配置。运行时动态调级与故障风暴采样明确不在本轮范围。

- [x] P0：重构日志配置和 stdout/file 双输出。
- [x] P0：增加 Docker 三服务日志上限。
- [x] P0：保留并监测 non-blocking 丢弃计数。
- [x] P1：healthz 降噪、ClientIp span、HTTP 单终态事件。
- [x] P1：本地数据丢失路径升级为 ERROR。
- [x] P1：补齐 `account_model_usage` / `fingerprint_update_history` 硬上限。
- [x] P2：panic hook，保留原 hook 与 backtrace 行为。
- [ ] P2：运行时动态调整日志级别；本轮先支持启动配置和 `RUST_LOG`。
- [ ] P2：上游故障风暴采样/限频；有实际告警系统后再设计。

## 5. 验收标准

1. `docker logs codex-proxy-rs` 能读取与文件一致的结构化事件。
2. stdout/file 任一关闭时另一输出正常；两者都关闭时配置加载失败。
3. 日志文件同时受 14 天、20MB/文件、20 个文件限制。
4. 模拟小 buffer 能观测到丢弃计数紧急告警。
5. healthz 成功请求不产生 trace；健康失败仍有 WARN。
6. 4xx 只有一条 INFO 终态，5xx 只有一条 WARN 终态；span 包含 client_ip。
7. 三个容器 `json-file` 均为 `10m × 5`。
8. 全量 retention 后，三张时间表满足保留期，两个历史/统计表满足 100 行硬上限。
9. PostgreSQL 维持 `logging_collector=off`、无归档、无复制槽时 WAL 正常回收。
10. Rust fmt、check、clippy `-D warnings`、全量测试、Compose config、镜像构建与干净启动全部通过。

## 6. 验收结果（2026-07-11）

- Rust：`cargo fmt --check`、all-targets/all-features Clippy `-D warnings` 全部通过；3 项库单测 + 619 项集成测试，0 失败。
- 前端：锁定依赖安装、Prettier 检查、`vue-tsc` 和 Vite 生产构建通过。
- 架构：权威文档 §9 临时门禁通过，没有恢复或留下检查脚本；所有 `backend/src` 文件不超过 800 行。
- 日志：干净启动后 stdout 与文件各观测到同一批 10 条有效 JSON，WARN=0、ERROR=0；手工和 Docker 的 healthz 探测都不产生 HTTP trace。
- 丢弃与轮转：小缓冲区单测确认紧急 JSON 告警；文件自然日、大小、总数上限以及 stdout-only/file-only 配置均通过测试。
- 数据库：两项真实 PostgreSQL 测试均将 102 行精确裁剪为最新 100 行；本地演示数据的 8 账号、3 密钥、160 成功事实、28 错误事实、672 统计桶在换镜像前后数量一致。
- PostgreSQL：`logging_collector=off`、`log_destination=stderr`、`archive_mode=off`、复制槽 0、WAL 实测 80MB；数据卷中没有 PG 日志文件。
- Docker：镜像 `codex-proxy-rs:logging-review` 清单摘要 `sha256:cfdd38395d19602dbdd31aa1bae5a03eed2e071cf770b729849bd2db8605378f`；三服务均 healthy、restart=0、`json-file 10m × 5`，`/healthz` 返回 204。
- 安全：Cargo audit、pnpm production audit 和镜像 HIGH/CRITICAL Trivy 均为 0 漏洞；Redis 已开启 `requirepass`，未认证 PING 返回 `NOAUTH`，启动 WARN=0。
