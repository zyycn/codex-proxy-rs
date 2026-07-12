---
name: codex-proxy-rs
description: Codex Proxy RS 仓库开发指南。Use when working on its Rust/Axum backend, Vue frontend, PostgreSQL/Redis storage, account-pool scheduling, Responses proxy and recovery, telemetry, Docker deployment, CI/release, configuration, debugging, or documentation.
---

# Codex Proxy RS

使用此 skill 处理 Codex Proxy RS 的开发、审计、验证和发布工作。

## 开始前

1. 确认当前仓库根目录是 `codex-proxy-rs`。
2. 存在 `.codegraph/` 时，优先用 CodeGraph 定位调用链，再回到源码和测试验证。
3. 非简单改动先阅读 `references/repo-guide.md`。
4. 涉及目录边界、请求链路、存储归属或后台任务时，同时阅读 `docs/architecture.md`。
5. 始终以当前源码、配置和测试为准；发现 skill 与代码不一致时，在同一改动中修正 skill。
6. 保持改动紧贴用户请求，不擅自改变目录、部署或发布契约。

## 任务分流

- 后端：遵循 `backend/src` 当前领域边界；测试统一放在 `backend/tests`，禁止写入 `backend/src`。
- 存储：PostgreSQL 是权威持久化，Redis 保存运行态数据，本地数据目录保存身份密钥和更新状态；SQLite 只用于历史库导入。
- 前端：使用 Vue 3 `<script setup>`、TypeScript、Tailwind v4、已有组件和主题 token。
- Docker / 发布：保留非 root runtime、Compose 密钥注入、命名卷、GitHub Release、GHCR 和在线更新契约。
- 文档：README 面向部署者和使用者；`docs/architecture.md` 面向开发者。不要新增过程型审计或迁移文档。

## 验证

按改动范围选择足以证明行为的命令。跨领域改动执行完整门禁：

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets --all-features --locked
cargo test --test main --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
```

后端集成测试需要可用的 PostgreSQL 和 Redis；通过 `CPR_TEST_DATABASE_URL`、`CPR_TEST_REDIS_URL` 指定测试连接。使用部署 Compose 时，连接密码必须与 `deploy/.env` 一致。

涉及在线更新时，至少运行 `api::admin::system_routes` 相关测试。涉及发布或镜像时，按 `references/repo-guide.md` 核对版本元数据、Release asset 和镜像 tag。

## Git

沿用仓库历史：使用简短的 Conventional Commit subject，并带署名：

```text
Co-authored-by: Codex <noreply@openai.com>
```
