---
name: codex-proxy-rs
description: Codex Proxy RS 仓库开发与审计指南。Use for its Rust gateway, Vue admin, OpenAI/xAI providers, account routing, Responses/Images transport, PostgreSQL/Redis state, workers, deployment, release, CI, or project documentation.
---

# Codex Proxy RS

## 开始工作

1. 从仓库根目录检查 `git status --short`，保留用户已有改动。
2. `.codegraph/` 存在时先用 CodeGraph 定位 owner、调用链和动态分发，再读未覆盖的源码。
3. 先确定事实属于 API、Core、Admin、Store、Host、Provider 还是 frontend，不跨 owner 打补丁。
4. 行为与文档冲突时以当前源码、配置和测试为准，并在同一任务中修正文档。

仓库根目录没有 Cargo manifest；后端命令必须进入 `backend/` 或显式传
`--manifest-path backend/Cargo.toml`。

## 文档路由

| 需要了解 | 权威入口 |
| --- | --- |
| 用户能力与快速开始 | `README.md` |
| HTTP 路由、DTO、敏感导入/导出 | `docs/api.md` |
| 系统边界、数据流、状态 owner、不变量 | `docs/architecture.md` |
| Compose、密码、备份、更新与恢复 | `deploy/README.md` |
| 迁移冻结与测试库 | `backend/migrations/README.md` |
| 具体目录和常见改动入口 | `references/repo-guide.md` |

README 保持用户导向；不要把上游 URL、重试常量、数据库字段清单或实现时间线堆进 README/架构文档。

## 不可破坏的边界

- `gateway-api` 只做 HTTP/WS/SSE 适配；`gateway-core` 编排请求；Provider 独占 credential、catalog、
  quota 与 transport；`gateway-store` 实现持久化端口；具体实现只在 `apps/gateway` 组合。
- Provider 的一次 `execute` 只选择一个 credential。换号、业务 retry 和跨 Provider fallback 只能由 Core
  在发送/交付边界内决定。
- PostgreSQL 是持久化权威；Redis 只保存可重建、可过期的协调状态。
- 当前只有 `openai` 与 `xai`，不存在 Provider Instance 层；Client Key 绑定账号分组而不是 Provider。
- 已应用迁移按字节冻结；schema 变化新增编号迁移，并同步 `.frozen-sha256`。
- 真实 secret 不进入普通日志、Debug、fixture、audit details 或文档示例；明文 Admin 响应只能出现在
  账号导出、Key reveal、备份设置等明确敏感合同中。
- 测试放在各 package 的 `tests/`，统一挂载到单一 `main` 集成测试目标；生产 `src` 不写 test-only 代码。
- 不添加兼容 shim、第二套状态机、重复配置或跨层旁路。

## 当前关键合同

- 数据面公开 Responses JSON/SSE/WS、review、Images generation/edit 和模型目录；不提供 Chat Completions。
- OpenAI Responses/Images 保持业务 wire 透明；xAI 在 Provider 内完成 Grok/Responses 转换。
- Images 固定走 OpenAI Provider 自有端点，不要求模型，也不参与文本模型映射。
- OpenAI 账号支持 OAuth、AT/RT、OAuth JSON 和 Agent Identity；xAI 只接受 OAuth session/账号 JSON，
  不接受 API Key。
- 主动额度重置卡只由 OpenAI 上游持有。查询由用户显式触发，消费使用 UUIDv4 幂等键；不得写本地卡库存
  或直接改 quota reset 时间。
- credential 与 quota 是独立事实；刷新 token 不等于刷新额度，quota 401/403 也不等于 RT 永久失效。
- 当前业务 schema 是 `0001_initial.sql` 的十二张表，加 `0002_nullable_requested_model.sql` 的无模型请求
  兼容变更。
- 运行拓扑是单副本；worker lease 不是完整多副本 leader election，自更新也只替换当前进程。

## 前端约束

- 使用 Vue 3、`<script setup lang="ts">`、现有基础组件和主题 token。
- `frontend/src/api` 只保留 wire DTO 与请求函数；页面查询、缓存和交互状态留在对应 view/composable。
- 修改页面前先检查共享组件和相邻调用方；复用既有视觉语言，保持紧凑、低噪声和键盘可访问。
- 上游查询不得由列表渲染或隐藏轮询意外触发。不可逆动作需要明确确认、loading 和不确定结果恢复路径。

## 验证

按改动风险选择相关命令；完整门禁为：

```bash
cargo +1.97.0 fmt --all --manifest-path backend/Cargo.toml -- --check
cargo +1.97.0 clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.97.0 test --manifest-path backend/Cargo.toml --test main --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

`gateway-store` 的 PostgreSQL/Redis 集成测试使用 `CPR_TEST_DATABASE_URL` 和 `CPR_TEST_REDIS_URL`；
本地未设置时静默跳过，CI 缺失则失败。自动化测试不发送真实对话、不轮换生产 RT。

## Git 与发布

- 提交前检查 status、cached/unstaged diff 和 `git diff --check`。
- 使用简短 Conventional Commit subject；按用户要求添加 `Co-authored-by: Codex <noreply@openai.com>`。
- 发布权威是 `release/version.yaml` 与带注释的 `vX.Y.Z` tag。
- 正式发布使用 `release/publish <version>`；提交、Release 和实例升级是三个独立状态。
