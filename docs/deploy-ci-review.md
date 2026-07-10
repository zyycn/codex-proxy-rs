# 部署与 CI 审查(deploy/ + .github/workflows/)

> 审查于 `5f2839db`,覆盖 `deploy/`(Dockerfile / docker-compose.yml / dockerignore / README)与 `.github/workflows/`(ci.yml / release.yml / security-scan.yml)。

整体底子相当好:多阶段构建、BuildKit cache mount、非 root 用户、`cap_drop: ALL`、`no-new-privileges`、PG18 新卷布局挂法正确(`/var/lib/postgresql` 而非 `/data` 子目录)、发布侧 SBOM + provenance + cosign keyless 签名齐全。以下按优先级列出值得动的点。

## P0 — 每次 push 两次全冷 Docker 构建(最大浪费)

- 现状:`ci.yml` 的 `deploy` job 与 `security-scan.yml` 的 `docker-security` job,**每次 push 到任意分支**都各自 `docker build --target runtime` 一次。BuildKit 的 `RUN --mount=type=cache` 在 CI runner 上不跨 run 持久,所以这是两次完全从零的 release 编译(backend + frontend),每次十余分钟 × 2。
- 建议:
  - `docker-security`(trivy)只在 `schedule` + main push 跑;
  - `ci.yml` 的 `deploy` job 加 `paths` 过滤(`deploy/**`、`backend/Cargo.lock`、workflow 自身)或只在 PR / main 跑;
  - 更进一步:security-scan 复用 ci 构建好的镜像(artifact 传递或按 digest 从 registry 拉),不重复构建。
- 顺带(触发面收敛):`push: branches: ['**']` + `pull_request:` 双触发——开 PR 的分支会全量跑两遍(push ref 与 PR merge ref 的 concurrency group 不同,互不取消)。走 PR 流程就把 push 限成 main;从不开 PR 就删掉 `pull_request:`。二选一。

## P1 — 供应链输入侧与输出侧不对称

- 现状:发布产物做到了 cosign + attestation 水准,但 workflow 里所有第三方 action(`dtolnay/rust-toolchain`、`Swatinem/rust-cache`、`pnpm/action-setup`、`docker/*`、`softprops/action-gh-release`、`aquasecurity/trivy-action`、`sigstore/cosign-installer`)都是 **tag 引用**。tag 可被 force-push,tj-actions 供应链事件即由此而来。
- 建议:非 `actions/*` 官方之外的 action 一律 pin 到 commit SHA(注释保留版本号)。
- 顺带:`.github/` 下**没有 dependabot.yml**,action 版本更新目前全靠手动。加 `package-ecosystem: github-actions`(可顺带 cargo + npm)自动提更新 PR,与 SHA pin 配合正好。

## P1 — Redis 裸奔,与 Postgres 不一致

> 状态：2026-07-11 已完成。Compose 使用 `CPR_REDIS_PASSWORD` 配置 `requirepass`，健康检查与应用连接串已同步。

- 现状:`docker-compose.yml` 中 Postgres 有密码,Redis 没有 `requirepass`——同 compose 网络内任何容器、以及宿主机本地进程(绑了 `127.0.0.1:6379`)均可无认证读写。
- 建议:`command` 加 `--requirepass ${CPR_REDIS_PASSWORD:-...}`,应用侧连接串同步。
- 顺带:pg / redis 的宿主端口映射(`127.0.0.1:5432` / `127.0.0.1:6379`)若只是偶尔调试用,建议注释掉——应用走 compose 内网,不需要它们。

## P1 — 日志与资源限制不一致

> 状态：2026-07-11 已完成日志上限；PostgreSQL / Redis 资源上限仍待单独评估。三服务统一使用 `json-file` `10m × 5`。

- 现状:三个服务的 stdout 走默认 `json-file` driver,没配 `max-size` / `max-file`,长期运行吃满磁盘(容器内 `/app/logs` 还写一份);postgres / redis 没有任何资源限制,而 app 有(`cpus` / `mem_limit` / `pids_limit`)——PG 内存失控会拖垮宿主。
- 建议:三个服务统一加 `logging` 限制(或宿主 daemon.json 全局配);给 pg / redis 补资源上限;Postgres 大查询会碰默认 64MB shm,可按需加 `shm_size`。

## P2 — release 无测试门禁

- 现状:`release.yml` tag 一推即构建发布,不校验该 commit 的 CI 是否绿过。从未过 CI 的 commit 打 tag 也能发版。
- 建议:`release-info` 后加一个 job 用 `gh api` 查该 SHA 的 check-runs 结论,不绿即 fail;或在 release 流程内跑一次 `cargo test`。

## P2 — 小件(顺手可做)

- `ci.yml` 与 `release.yml` 所有 job 都没有 `timeout-minutes`(security-scan 有)——挂住的 job 默认吃满 6 小时。
- `cargo install cargo-audit` 首次 / 缓存失效时现编译数分钟,换 `taiki-e/install-action`(预编译秒装);或升级成 `cargo-deny`,advisory + license + 重复依赖一起管。
- trivy 只做门禁不留记录,加 SARIF 输出 + `security-events: write`,可在 GitHub Security tab 看历史趋势。
- `release.yml` L222 / L237 `mkdir -p ../dist/binaries/...` 重复两次;`build-and-push` 的 `id` 无人引用。纯洁癖。
- `Dockerfile` 的 `CPR_VERSION` 默认空串,手动构建 `release-asset` target 不传参会产出 `codex-proxy-rs__linux_amd64.tar.gz`(双下划线)。加个默认值或 fail-fast 均可。

## 核实过的误报(记录一下,免得反复怀疑)

- **CI 只跑 `cargo test --test main` 不算漏测**:`backend/src` 零 `#[cfg(test)]`,所有测试都走 `tests/main.rs` 单入口,`--test main` 即全量。
- **frontend CI 没单独 typecheck 步骤不算漏**:`build` 脚本本身是 `vue-tsc -b && vite build`,typecheck 已含在 build 里。
- **postgres:18 挂 `/var/lib/postgresql` 是对的**:PG18 官方镜像改了数据布局(`PGDATA=/var/lib/postgresql/<major>/docker`),挂整个父目录才能支持原地 pg_upgrade;挂 `/data` 子目录反而是旧习惯踩坑。
- **rust 版本一致性没问题**:Dockerfile `rust:1.95-bookworm`、CI `dtolnay/rust-toolchain@1.95`、release 构建容器 `rust:1.95-bookworm` 三处对齐。

## 建议动手顺序

1. **P0**:CI 构建去重 + 触发收敛(纯 workflow 改动,立竿见影省分钟数)。
2. **P1 三条**:SHA pin + dependabot、Redis 密码 + 端口收敛、日志/资源限制(前者动 workflow,后两者动 compose,互不冲突可一批)。
3. **P2**:release 门禁与小件,顺手时清掉。
