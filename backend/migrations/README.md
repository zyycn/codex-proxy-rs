# 数据库迁移

sqlx 在服务启动、监听请求之前按编号执行迁移，并把文件 checksum 记入 `_sqlx_migrations`。
已应用迁移的字节与记录不一致时，服务会拒绝启动。

`0001_initial.sql` 定义数据库初始化基线，后续 schema 变更按编号新增迁移。
完整清单以本目录 SQL 文件和 `.frozen-sha256` 为准。

## 冻结规则

- 同一大版本内，已合入 main 的迁移文件**永久冻结**，一个字节都不能改——包括注释和
  空白。任何改动都会让该大版本的已部署实例在下次重启时因 checksum 失配而无法启动，
  且没有自动恢复路径。
- 同一大版本内的 schema 变更一律新增编号迁移，永远不回改
  旧文件。切换大版本时允许把已完成升级的 schema 折叠成新的 `0001` 基线，因为跨大版本
  不支持在线升级，目标版本必须使用全新数据目录部署。
- `.frozen-sha256` 是冻结清单（`sha256sum` 格式）。CI 会校验：清单内文件
  字节未变，且目录里每个 `*.sql` 都已入册。新增迁移时在同一提交里执行
  `sha256sum 000N_xxx.sql >> .frozen-sha256` 入册。
- 若同一大版本内确需修正已冻结迁移里的错误，用新迁移做补偿性变更（`alter` / 回填），
  不要动原文件。

从迁移目录检查已登记文件：

```bash
cd backend/migrations
sha256sum --check --strict .frozen-sha256
```

CI 还会检查是否遗漏新 SQL 文件，并在 PR 中检查清单只增不改。
遇到 checksum 不一致，先核对运行版本和文件来源；不要修改数据库中的 checksum 来绕过校验。

## 本地测试库

`gateway-store` 的 PG/Redis 集成测试需要以下环境变量，未设置时在本地
静默跳过（CI 缺失则直接失败）：

```bash
export CPR_TEST_DATABASE_URL='postgres://<user>:<password>@127.0.0.1:5432/<db>'
export CPR_TEST_REDIS_URL='redis://:<password>@127.0.0.1:6379'
```

测试自建随机 schema / key 前缀做隔离，但仍应使用专用开发或测试实例，不要连接生产库。
凭据从自己的部署配置或 CI Secret 中读取，不要粘贴未脱敏的 `docker inspect` 输出。
环境变量未设置导致的跳过不算数据库测试通过。

运行包含 `StoreBundle` 初始化的完整集成测试时，两条测试 URL 都须包含密码，且密码满足 Store
启动配置的 48 位十六进制要求；仅能连接数据库并不代表该初始化合同通过。专用服务使用对应测试密码，
并在测试进程中清除 `CPR_DATABASE_URL`、`CPR_REDIS_URL`、`CPR_DATABASE_PASSWORD` 和
`CPR_REDIS_PASSWORD`，避免启动配置被部署环境覆盖。
