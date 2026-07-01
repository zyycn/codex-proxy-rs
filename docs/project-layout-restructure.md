# Project Layout Restructure

## 背景

项目目录已按 Sub2API 的边界收敛：

- `backend/` 承载后端应用、后端构建元信息和后端测试。
- `frontend/` 承载前端应用。
- `deploy/` 承载 Docker、Compose、部署脚本和部署示例配置。

根目录只保留仓库级入口和文档，不再承载后端源码、前端源码或具体部署实现。

## 完成后结构

```text
codex-proxy-rs/
├── backend/
│   ├── Cargo.toml
│   ├── Cargo.lock
│   ├── build/
│   │   ├── build.rs
│   │   └── VERSION
│   ├── rust-toolchain.toml
│   ├── src/
│   └── tests/
├── frontend/
│   ├── package.json
│   ├── pnpm-lock.yaml
│   ├── pnpm-workspace.yaml
│   ├── vite.config.ts
│   └── src/
├── deploy/
│   ├── Dockerfile
│   ├── docker-compose.yml
│   ├── config.example.yaml
│   └── README.md
├── docs/
├── README.md
└── .github/
```

说明：

- `backend/build/VERSION` 是唯一的源码默认版本文件。它对应 Sub2API 的 `backend/cmd/server/VERSION`，但本项目的 Rust 入口仍保留在 `backend/src/main.rs`，所以版本元信息归入 `backend/build/`。
- `backend/build/build.rs` 读取 `CPR_VERSION`，否则读取 `backend/build/VERSION`，最后才回落到 `CARGO_PKG_VERSION`。
- `frontend/` 替代原 `web/`。前端只负责展示后端返回的版本、构建类型、部署类型和更新进度。
- `deploy/` 替代根目录里的部署文件归属。根目录只保留仓库级入口和文档，不承载具体部署实现。
- 测试代码继续放在测试目录，禁止放入 `backend/src`。

## 路径映射

| 原路径 | 新路径 | 备注 |
| --- | --- | --- |
| `src/` | `backend/src/` | Rust 后端源码。 |
| `tests/` | `backend/tests/` | Rust 集成测试，仍在测试目录，不进入源码目录。 |
| `Cargo.toml` | `backend/Cargo.toml` | 后端 package manifest。 |
| `Cargo.lock` | `backend/Cargo.lock` | 后端锁文件，随后端移动。 |
| `build.rs` | `backend/build/build.rs` | 后端构建脚本。 |
| `VERSION` | `backend/build/VERSION` | 后端应用版本源。 |
| `rust-toolchain.toml` | `backend/rust-toolchain.toml` | 后端工具链配置。 |
| `web/` | `frontend/` | 前端应用。 |
| `Dockerfile` | `deploy/Dockerfile` | 生产镜像构建入口。 |
| `docker-compose.yml` | `deploy/docker-compose.yml` | Docker 部署入口。 |
| `config.yaml` | `deploy/config.example.yaml` | 根目录不保留本地运行配置。 |

## Cargo 组织方式

优先采用 Sub2API 风格：后端是自包含子项目。

```text
backend/
├── Cargo.toml
├── Cargo.lock
├── build/
│   ├── build.rs
│   └── VERSION
├── src/
└── tests/
```

后续命令从：

```bash
cargo test --test main admin::system --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
```

调整为：

```bash
cargo test --manifest-path backend/Cargo.toml --test main admin::system --locked
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

如果后续需要根目录统一调度，可以再加一个很薄的 root workspace `Cargo.toml`，但不要让根目录重新成为后端版本源。

根目录 `target/` 不是项目构建目标目录。迁移后后端构建产物位于 `backend/target/`；如果本地编辑器或 rust-analyzer 在根目录生成 `target/flycheck*`，它只是本地检查临时产物，可以直接删除，且已被 `.gitignore` 忽略。

## 前端迁移点

`web/` 已迁移到 `frontend/`，同步点：

- `.github/workflows/release.yml` 中所有 `pnpm --dir web ...` 改为 `pnpm --dir frontend ...`。
- Dockerfile 中所有 `web/package.json`、`web/pnpm-lock.yaml`、`web/dist` 改为 `frontend/...`。
- README、部署文档和开发命令里的 `web` 改为 `frontend`。
- 前端 API 类型和更新弹窗不做业务重写，只做路径迁移。

## 部署迁移点

`deploy/` 负责部署相关文件：

- `deploy/Dockerfile`
- `deploy/docker-compose.yml`
- `deploy/config.example.yaml`
- `deploy/README.md`

Docker 构建上下文仍建议使用仓库根目录：

```bash
docker build -f deploy/Dockerfile -t codex-proxy-rs .
```

这样 Dockerfile 可以同时访问 `backend/` 和 `frontend/`，避免把构建上下文拆散。

Compose 文件迁移到 `deploy/` 后，默认挂载路径随 Compose 文件落在 `deploy/` 目录，保持部署文件和部署状态集中；需要接入外部配置或数据目录时，通过环境变量覆盖：

```yaml
services:
  codex-proxy-rs:
    volumes:
      - ${CPR_CONFIG_FILE:-./config.yaml}:/app/config.yaml:ro
      - ${CPR_DATA_DIR:-./data}:/app/data
      - ${CPR_LOG_DIR:-./logs}:/app/logs
```

使用 `deploy/config.example.yaml` 作为样例，运行时配置由用户复制为 `deploy/config.yaml` 或通过 `CPR_CONFIG_FILE` 挂载提供。

## 发布链路调整

发布版本必须只有一个来源：

1. Git tag，例如 `v0.1.1`。
2. CI 去掉前缀 `v`，得到 `0.1.1`。
3. CI 写入 `backend/build/VERSION`。
4. CI 构建后端时传入：
   - `CPR_VERSION=0.1.1`
   - `CPR_GIT_SHA=<sha>`
   - `CPR_BUILD_TIME=<utc>`
   - `CPR_BUILD_TYPE=release`
5. `backend/build/build.rs` 将这些值注入二进制。
6. 后端 `/api/admin/system/version` 和 `/api/admin/system/check-updates` 使用二进制内的版本值。
7. 前端只展示后端返回的数据，不做版本映射。

需要同步修改：

- `.github/workflows/release.yml`
- `deploy/Dockerfile`
- `backend/build/build.rs`
- `backend/src/admin/system/routes.rs`
- `backend/tests/admin/system/mod.rs`

## Dockerfile 调整

目标 Dockerfile 阶段：

```dockerfile
FROM node:24-bookworm-slim AS frontend-builder
WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY frontend ./
RUN pnpm build

FROM rust:1.95-bookworm AS backend-builder
WORKDIR /app/backend
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/build ./build
COPY backend/src ./src
COPY --from=frontend-builder /app/frontend/dist ./web/dist
RUN cargo build --release --locked --bin codex-proxy-rs

FROM debian:bookworm-slim AS runtime
WORKDIR /app
ENV CPR_DEPLOYMENT_MODE=docker
COPY --from=backend-builder /app/backend/target/release/codex-proxy-rs /usr/local/bin/codex-proxy-rs
COPY --from=frontend-builder /app/frontend/dist ./web/dist
CMD ["codex-proxy-rs"]
```

注意：

- Docker runtime 镜像不设置 `CPR_VERSION`，避免运行时环境变量成为第二版本源。
- Docker runtime 仍只包含主服务，不恢复 updater sidecar。
- `deploy/Dockerfile` 的构建上下文应保持仓库根目录。

## 迁移记录

本次迁移执行项：

1. 新建 `backend/`，用 `git mv` 移动 `src/`、`tests/`、`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`，并将 `build.rs`、`VERSION` 归入 `backend/build/`。
2. 新建 `frontend/`，用 `git mv web frontend`。
3. 新建 `deploy/`，用 `git mv Dockerfile deploy/Dockerfile`、`git mv docker-compose.yml deploy/docker-compose.yml`，并移动部署样例配置。
4. 修改 `.github/workflows/release.yml` 的工作目录、构建路径、打包路径和 Dockerfile 路径。
5. 修改 `deploy/Dockerfile` 的 `COPY` 路径。
6. 修改 `.dockerignore` 和 `.gitignore`，使用 `frontend/node_modules`、`frontend/dist`、`backend/target`。
7. 修改文档和 README 中的开发、测试、构建命令。

## 验收命令

迁移完成后至少运行：

```bash
cargo test --manifest-path backend/Cargo.toml --test main admin::system --locked
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
pnpm --dir frontend build
docker build -f deploy/Dockerfile -t codex-proxy-rs:latest .
docker compose -f deploy/docker-compose.yml config
git diff --check
```

如果保留 root workspace，则还需要确认：

```bash
cargo metadata --format-version 1
```

输出中不能重新出现旧的 updater binary，也不能把根 `VERSION` 当成版本源。

## 完成标准

- 顶层不再有 `src/` 和 `web/`。
- 后端源码、测试和版本源都归属 `backend/`。
- 前端应用归属 `frontend/`。
- Dockerfile、Compose 和部署样例归属 `deploy/`。
- `backend/build/VERSION` 是唯一源码默认版本文件。
- 发布 tag、release asset、Docker image label、后端 `/version`、前端更新弹窗显示同一个版本。
- Docker 部署仍是单主服务模式，不恢复 updater sidecar。
