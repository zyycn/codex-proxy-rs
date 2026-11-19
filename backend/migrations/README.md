# 数据库迁移

服务监听请求前，sqlx 按编号执行本目录的 SQL，并将文件校验和写入 `_sqlx_migrations`。已应用文件的校验和不匹配时，服务拒绝启动

[状态所有权](../../docs/architecture.md#8-状态所有权) · [离线恢复](../../deploy/README.md#人工恢复数据库)

## 命名约定

- 表名使用小写 `snake_case`，实体集合使用复数，配置与单例状态使用自然语义，如 `runtime_settings`、`request_analytics_build_state`
- 业务归属使用领域前缀，同一实体统一使用 `client_api_key`、`account_group`、`provider` 等术语，不添加 `tbl_` 等对象类型前缀
- 统计时间粒度使用 `15m`、`1h`、`1d`；只保存计数的表保留 `_counts` 后缀
- 表名调整时同步查询、函数体及所属索引和约束名称，已冻结迁移仍保持原样

## 冻结规则

- 同一大版本内，合入 `main` 的迁移文件**按字节永久冻结**，包括注释和空白。修改 schema 或修正错误须追加编号迁移，使用 `alter` 或数据回填
- 跨大版本不支持在线升级，目标版本必须使用全新数据目录部署；此时可将已完成升级的 schema 折叠为新的初始化基线
- 支持连续升级的预发行线从首次发布起同样冻结迁移。alpha、beta、rc 到对应正式版必须保留
  已发布迁移的编号与字节；同一轮 exp 的后续发行也只能追加迁移。发布前以该发行线已发布版本
  的数据库验证升级路径，不能只与上一正式版比较。不同 exp 实验线之间、exp 与正式版之间
  不保证迁移历史兼容
- `.frozen-sha256` 使用 `sha256sum` 格式。新增迁移时在同一提交执行 `sha256sum 000N_xxx.sql >> .frozen-sha256` 入册；CI 校验文件字节、SQL 登记完整性及 PR 中清单只增不改

从迁移目录检查已登记文件：

```bash
cd backend/migrations
sha256sum --check --strict .frozen-sha256
```

校验和不一致时无法自动恢复。核对运行版本与文件来源，不要改写数据库中的校验和

## 本地测试库

`gateway-store` 的 PostgreSQL/Redis 集成测试使用以下环境变量；所需测试服务未配置时，本地相关测试跳过，CI 直接失败：

```bash
export CPR_TEST_DATABASE_URL='postgres://<user>:<password>@127.0.0.1:5432/<db>'
export CPR_TEST_REDIS_URL='redis://:<password>@127.0.0.1:6379'
```

插件 Runtime 的真实子进程与持久化测试可使用专用的 `CPR_PLUGIN_TEST_DATABASE_URL`，此时须同时设置 `CPR_PLUGIN_TEST_REDIS_URL`。省略插件数据库 URL 时使用上面的测试服务

使用专用开发或测试实例，测试通过随机 schema 和 Redis key 前缀隔离数据。凭据取自部署配置或 CI Secret，不要分享未脱敏的 `docker inspect` 输出

PostgreSQL 的临时 schema 测试连接使用异步提交，保留事务可见性、回滚和完整迁移检查
需要验证数据库崩溃后的持久性时，使用独立数据库与生产连接配置，不复用这类 fixture

运行包含 `StoreBundle` 初始化的完整集成测试时，两条测试 URL 都须包含密码，原始密码须满足
[Store 密码要求](../../deploy/README.md#手动安装)，在测试 URL 中按 URL 规则编码。
在测试进程中清除 `CPR_DATABASE_URL`、`CPR_REDIS_URL`、`CPR_DATABASE_PASSWORD` 和 `CPR_REDIS_PASSWORD`，避免部署环境覆盖测试配置
