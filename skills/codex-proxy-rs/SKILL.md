---
name: codex-proxy-rs
description: Codex Proxy RS 项目专用指南。Use when working in the codex-proxy-rs repository, or when the user asks to understand, develop, test, refactor, release, Dockerize, configure, debug this Rust/Axum backend, Vue 3/Tailwind v4 frontend, Tailwind v4 theme tokens, login UI, Docker deployment, GitHub Release, GHCR image, SSE update logs, version publishing, or online one-click update flow.
---

# Codex Proxy RS

这个 skill 记录 Codex Proxy RS 的仓库结构、开发命令和约束。

## 开始前

1. 确认当前仓库根目录是 `codex-proxy-rs`。
2. 如果仓库存在 `.codegraph/`，定位代码和理解调用链时先用 CodeGraph，再用 `rg` 或手动读文件。
3. 非简单改动先阅读 `references/repo-guide.md`。
4. 基于当前文件确认事实，不要只凭历史记忆回答或修改。
5. 保持改动紧贴用户当前请求；不要擅自重组目录、部署结构或发布链路。

## 任务分流

- 后端：遵循 `backend/src` 里的 Rust/Axum 模式；集成测试放 `backend/tests`，禁止把测试代码放进 `backend/src`。
- 前端：使用 Vue 3 `<script setup>`、TypeScript、Tailwind v4 utilities 和已有 CSS 主题 token。
- Docker / 发布：保持 runtime 镜像最小化，并维护现有 GitHub Release、GHCR、在线更新契约。
- 文档：短、准、和真实行为一致；除非用户要求，不新增冗余文档文件。

## 验证

选择能证明本次改动的最小命令集：

```bash
cargo fmt --manifest-path backend/Cargo.toml --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --test main --locked
pnpm --dir frontend build
docker compose -f deploy/docker-compose.yml config
```

涉及在线更新时，至少运行相关 `admin::system` 测试。涉及发布或 Docker 镜像时，按 `references/repo-guide.md` 核对真实产物名、版本元数据和镜像 tag。

## Git

沿用仓库提交格式：简短 conventional commit subject，并带署名：

```text
Co-authored-by: Codex <noreply@openai.com>
```
