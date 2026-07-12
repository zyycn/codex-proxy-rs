# 审查文档索引(codex-proxy-rs)

> 汇总于 `feat/postgres-redis-migration` 工作树。本目录下 10 份 `*-review.md` 是对 backend/frontend/deploy/CI 的分领域审查,统一采用 P0/P1/P2 分级 + "做得好的" + "核实过的误报" + "建议动手顺序" 的结构。本文件是导航 + 跨领域交叉印证,不重复各文档正文。

## 10 份审查一览

| 文档 | 领域 | 最高优先级发现(一句话) |
| --- | --- | --- |
| [security-review.md](security-review.md) | 安全 | 上游 OAuth token 与 client API key 明文落库;转发头无条件信任致登录锁定可绕过 |
| [concurrency-review.md](concurrency-review.md) | 并发/锁/错误处理 | **P0** 客户端首 token 前取消会永久泄漏 WS 池 Busy 槽位,账号连接池静默失效 |
| [storage-review.md](storage-review.md) | 数据库/Redis/迁移 | **P0** `CURRENT_SCHEMA_VERSION` 手工同步,第一次加列漏改即"上线成功、重启失败" |
| [api-contract-review.md](api-contract-review.md) | HTTP API 契约 | **P0** 管理台 keys 页只有 cursor 分页而前端不跟游标,>50 个 key 被静默截断 |
| [logging-review.md](logging-review.md) | 日志 | **P0** 无 stdout 层致 `docker logs` 全空;全项目零 `error!` |
| [frontend-review.md](frontend-review.md) | 前端工程 | 无 ESLint(唯一门禁是 prettier);401 会话过期无跳转,页面永不回登录页 |
| [test-coverage-review.md](test-coverage-review.md) | 测试覆盖 | 存储故障注入 / 并发争用 / telemetry 三块几乎零直测 |
| [dependency-review.md](dependency-review.md) | 依赖 | `config` 默认 features 全开编死重;`sqlx` sqlite 走 bundled 编进生产二进制 |
| [deploy-ci-review.md](deploy-ci-review.md) | 部署/CI | 每次 push 两次全冷 Docker 构建;Redis 无密码;日志无上限 |
| [naming-review.md](naming-review.md) | 命名 | 仅剩 5 处 `foo.rs + foo/` 模块风格未统一到 `mod.rs` |
| [dead-code-and-smells-review.md](dead-code-and-smells-review.md) | 死代码/异味 | 死代码仅 4 个纯委托 wrapper;2 处锁持有过久(与并发审查呼应) |

## 跨领域交叉印证(多份文档指向同一根因)

这几处不是孤立发现,而是从不同角度撞到了同一件事,修复时应合并考虑:

1. **明文凭据 = 安全 × 存储**:上游 token / client key 明文落库,[security](security-review.md) 从"资产裸奔 + 与 admin key 摘要做法不一致"定性,[storage](storage-review.md) 从建表 DDL(`0001_initial.sql:12,46-47`)佐证。若做 client key 摘要化,会**需要一条真实迁移**——正好撞上 storage P0 的迁移框架加固;两件事应排在同一个迁移窗口。

2. **客户端取消 = 并发 × 测试盲区**:[concurrency](concurrency-review.md) 的 P0(WS 槽位泄漏)+ P1(账号 slot 虚占)是"acquire 后无 Drop 兜底"的同一根因;而 [test-coverage](test-coverage-review.md) 恰好指出"并发争用 / 取消路径零覆盖"——这条 bug 能长期潜伏正是因为没有取消测试。修复 PR 应同时补一个"首 token 前 drop 请求 future"的回归测试。

3. **双分页 = API × 前端**:[api-contract](api-contract-review.md) 的 P0(keys 页 cursor 截断)与 [frontend](frontend-review.md) 的"API 层类型断层"是同一条边界的两面——后端给 cursor、前端不跟且类型被 any 击穿。收敛到 numbered 分页需要前后端一起改。

4. **sqlite 依赖 = 存储 × 依赖**:[storage](storage-review.md) 从"import_sqlite 退役条件已满足"、[dependency](dependency-review.md) 从"sqlx sqlite bundled 编译成本"两头指向同一动作;且两份都独立发现 **`sqlx uuid` feature 全仓零使用**(可即刻删)。

5. **无上限增长 = 日志 × 部署 × 存储**:[logging](logging-review.md) 的文件日志无大小上限、[deploy-ci](deploy-ci-review.md) 的 Docker `json-file` 无 `max-size`、[storage](storage-review.md) 的 affinity SET 只增不减——三处都是"长期运行吃满资源"的同类隐患,运维加固时一并处理。

## 跨全部审查的 P0 清单(建议最先处理)

按"可匿名利用 / 不可自愈 / 数据可见性"排序:

1. **转发头信任边界**([security](security-review.md) P1 但**匿名可直接利用**)—— 唯一在线爆破防线失效,改动集中在 `request_id.rs` 一处。**投入产出最高,建议第一个做。**
2. **WS 槽位取消泄漏**([concurrency](concurrency-review.md) P0)—— 唯一"不可自愈的实际 bug",连接池静默失效只能重启恢复。
3. **迁移版本常量漂移**([storage](storage-review.md) P0)—— 一行改动消灭"第一次加列就翻车"的确定性事故,且是后续所有加密/加列迁移的前置。
4. **keys 页分页截断**([api-contract](api-contract-review.md) P0)—— 唯一真数据可见性 bug,管理台超 50 key 不可见。
5. **stdout 日志层缺失**([logging](logging-review.md) P0)—— 容器日志采集全线接不上,排障基础设施缺位。
6. **上游 token 明文**([security](security-review.md) P0)—— 资产价值最高,但属 latent(需先能读 PG),改动面最大,单独排期。

## 整体评价

跨 10 个领域看下来,这是一个**工程素养明显在线**的代码库:鉴权原语(argon2 / ConstantTimeEq / PKCE / 摘要化会话)、锁纪律(std 锁无一跨 await、poison 处理统一)、事务契约(事实+桶同事务)、迁移防御(单事务 / 防降级)、测试基建(每测试独立真库)都达到或超过行业基线。发现的问题**集中在边界与演进**:取消安全、信任边界、第一次 schema 演进、容器日志出口——都是"正常路径写得好、异常/演进路径没兜住"的类型,而非架构性硬伤。多份审查各自独立地把预设的部分"疑似问题"核实为误报(strict 已开、recovery 路径其实测得最密、时序 oracle 不可利用、迁移框架其实存在),说明这套代码经得起推敲。
