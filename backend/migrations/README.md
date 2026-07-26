# 数据库迁移

sqlx 在服务启动时（`serve` 监听之前）按编号顺序执行本目录的迁移，并把每个
文件的 checksum 记入 `_sqlx_migrations`。此后每次启动都会重新校验：**已应用
迁移的文件字节与记录不一致时，服务直接拒绝启动**。

## 冻结规则

- 已合入 main 的迁移文件**永久冻结**，一个字节都不能改——包括注释和空白。
  任何改动都会让所有已部署实例（含自更新链路上的存量用户）在下次重启时
  因 checksum 失配而无法启动，且没有自动恢复路径。
- schema 变更一律新增编号迁移（`0003_...`、`0004_...`），永远不回改旧文件。
- `.frozen-sha256` 是冻结清单（`sha256sum` 格式）。CI 会校验：清单内文件
  字节未变，且目录里每个 `*.sql` 都已入册。新增迁移时在同一提交里执行
  `sha256sum 000N_xxx.sql >> .frozen-sha256` 入册。
- 若确需修正已冻结迁移里的错误，用新迁移做补偿性变更（`alter` / 回填），
  不要动原文件。

## 本地测试库

`gateway-store` 的 PG/Redis 集成测试需要以下环境变量，未设置时在本地
静默跳过（CI 缺失则直接失败）：

```bash
export CPR_TEST_DATABASE_URL='postgres://<user>:<password>@127.0.0.1:5432/<db>'
export CPR_TEST_REDIS_URL='redis://:<password>@127.0.0.1:6379'
```

测试自建随机 schema / key 前缀做隔离，可安全指向开发库；凭据按本地部署
实际值填写（`docker inspect` 对应容器可查）。
