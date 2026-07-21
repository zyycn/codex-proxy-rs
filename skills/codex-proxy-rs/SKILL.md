---
name: codex-proxy-rs
description: Codex Proxy RS 仓库开发指南。Use when changing or auditing its Rust backend, Vue frontend, PostgreSQL/Redis state, account scheduling, Responses transport and recovery, telemetry, Docker deployment, CI, release, configuration, or project documentation.
---

# Codex Proxy RS

## 事实来源

1. 以当前源码、配置和测试为准。
2. 非简单改动读取 `references/repo-guide.md`。
3. 涉及目录、生命周期、存储或后台任务时读取 `docs/architecture.md`。
4. skill 或文档与实现冲突时，在同一改动中修正。

存在 `.codegraph/` 时可先定位调用链；结论必须回到源码和测试验证。

## 边界

- `gateway-api` 只做协议适配；`gateway-core` 编排；Provider 独占 credential、catalog 与 transport；`gateway-store` 实现持久化端口。
- Provider 每次调用只选择一个 credential，不隐藏换号、业务 retry 或跨 Provider fallback。
- PostgreSQL 是持久化权威；Redis 只保存可恢复协调状态和 OAuth pending flow。
- 测试放在各 package 的 `tests/`。禁止在生产 `src/` 写 test-only 代码。
- Vue 使用 `<script setup lang="ts">`、现有基础组件和主题 token。
- README 面向使用者，保持简短；长期架构只写入 `docs/architecture.md`。
- 不添加兼容 shim、重复状态机、第二套配置或补丁式旁路。

## 工作流

1. 检查工作树，区分用户改动与当前任务。
2. 追踪入口、owner、状态变化和输出，再决定修改点。
3. 在最小所有权边界内实现；行为变化用外置集成测试固定。
4. 同步受影响的架构、配置或 skill。
5. 按风险运行验证，最后检查 staged 与 unstaged 范围。

## 验证

从仓库根目录执行：

```bash
cargo +1.97.0 fmt --manifest-path backend/Cargo.toml -- --check
cargo +1.97.0 clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.97.0 test --manifest-path backend/Cargo.toml --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

后端完整测试需要 PostgreSQL、Redis，以及与 `deploy/config.yaml` 一致的
`CPR_TEST_DATABASE_URL`、`CPR_TEST_REDIS_URL`。

## Git 与发布

- 使用简短 Conventional Commit subject。
- 提交前检查 `git status --short`、cached/unstaged diff 和 `git diff --check`。
- 提交带 `Co-authored-by: Codex <noreply@openai.com>`。
- 发布以 `release/version.yaml` 和带注释的 `vX.Y.Z` tag 为准。
- 发布完成后核对远端 `main`、tag、Actions、Release asset 与 GHCR。
