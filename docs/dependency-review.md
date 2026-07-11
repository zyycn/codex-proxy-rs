# 依赖健康审计(backend)

> 审计于 `feat/postgres-redis-migration` 工作树，2026-07-11 以 `b0d8be0a` 复核。对象为 `backend/Cargo.toml` 的 41 个直接依赖 + 2 个 dev-dependencies；Linux normal+build 依赖树 299 个 crate，Cargo.lock 全平台 382 个。方法：`cargo tree -d`（重复版本）、cargo-machete 0.9.2（未使用依赖）、cargo-audit 0.22.2 + RustSec 1159 条 advisory（漏洞）、crates.io 当前版本、`rg` 逐个抽查 feature 与 crate 的实际代码使用点。

先说结论：底子相当健康。machete 零未使用，cargo audit 零漏洞零警告，redis/reqwest/rustls 的 feature 白名单纪律很好，也没有重复能力 crate。需要处理的是三类问题：**config 与 SQLx 默认 feature 带来的无用编译能力**、**测试 feature 混入生产依赖**，以及**缺少就地说明的 TLS 指纹版本 pin**。SQLite bundled C 库确有成本，但 `import-sqlite` 仍是权威架构规定的正式命令，本轮不能用默认关闭的兼容 feature 隐藏；源库退役后应直接删除整条导入路径。

## 实施结果（2026-07-11）

- 已完成 config、SQLx、Tokio、Axum feature 收窄；SQLx derive 与 Tokio test-util 只在 dev-dependencies 启用；dirs 升至 6.0.0，并刷新全部兼容范围内的 lock 版本。`cargo update --locked --dry-run` 最终为 0 个可更新包。
- Linux normal+build 依赖从 299 降至 272 个 package id，Cargo.lock 全平台 package 从 382 降至 338；生产树已无 `sqlx-macros`，config 的 INI/RON/JSON5/TOML 解析链及相关 proc-macro 已移除。
- `cargo machete --with-metadata backend` 为 0 个未使用依赖；`cargo audit --file backend/Cargo.lock` 为 0 漏洞、0 warning；生产 check、all-target/all-feature clippy、release build 均在 `RUSTFLAGS=-D warnings` 下通过。
- PostgreSQL/Redis 实库测试共 634 项，结果 634 passed / 0 failed。测试仍全部位于 `backend/tests/`，没有向 `backend/src/` 写入测试代码。
- runtime 镜像 `codex-proxy-rs:dependency-review` 构建成功，镜像 ID `sha256:f1f297745aa9`，本机显示 158 MB；Trivy 0.72.0 对 HIGH/CRITICAL、ignore-unfixed 扫描为 0 漏洞。使用独立 PostgreSQL 18 + Redis 8 启动后 `/healthz` 返回 204，容器 healthy、0 次重启，启动日志 WARN/ERROR/PANIC/FATAL 均为 0。
- tower-http 0.7 与 reqwest 0.12.28 的传递依赖约束冲突，单独升级会同时保留 0.6/0.7 两套，因此按审计结论保持 0.6.11；SQLite 导入路径也按权威架构继续作为正式命令保留，没有引入临时 feature 或兼容分支。

## P0 — 常驻的无用编译成本

### 1. config 默认 features 全开:只用 YAML,却编译着 5 种用不到的格式解析器
- 现状:`config = "0.15.23"` 未声明 features → 默认打开 `toml, json, yaml, ini, ron, json5, convert-case, async`。而代码里唯一的加载点是 `bootstrap/config.rs:383` 的 `File::from(config.yaml)`,格式只有 YAML(`load_from_dir` 硬编码 `config.yaml`,无其他扩展名支持)。
- 实测:`cargo tree -i` 逐个确认 rust-ini、ordered-multimap、dlv-list、const-random(+proc-macro)、ron、json5、pest/pest_derive/pest_generator/pest_meta、toml、convert_case 全部**只被 config 拉入**——约 13 个 crate 纯死重,其中 pest_derive、const-random-macro 是 proc-macro(占编译关键路径)。这条链还顺带引入了全树第三个 hashbrown 版本(0.14.5,经 rust-ini→ordered-multimap)和一支 getrandom 0.2(经 const-random)。
- 建议:`config = { version = "0.15", default-features = false, features = ["yaml"] }`。一行改动,-13 crate,风险≈0。

### 2. SQLx 默认 features 与 `sqlite` 总开关同时过宽
- 现状：`sqlx` 没有设置 `default-features = false`，因此除显式功能外还启用了默认的 `any`、`macros`、`migrate`、`json`。项目使用自建 PG 迁移框架，生产代码没有 SQLx 查询宏；唯一的 `sqlx::FromRow` derive 位于外部测试。显式的 `sqlite` 又是总开关，同时启用 bundled、deserialize、load-extension、unlock-notify；导入代码只需要 bundled 基础驱动。
- 问题：生产构建编译了没有调用方的 SQLx Any、宏、内置迁移和 SQLite 扩展能力。原审计只看到 SQLite C 库，漏掉了这部分更容易消除的成本。
- 建议：主依赖设置 `default-features = false`，显式保留 PG、JSON、chrono、uuid、运行时与 TLS，并将 `sqlite` 收窄为 `sqlite-bundled`；仅在 dev-dependencies 为外部测试开启 `derive`。

### 3. bundled SQLite C 库仍在生产二进制，但当前不能 feature-gate
- 现状：`sqlite-bundled` 会编译并静态链接 SQLite C amalgamation，顺带引入 sqlx-sqlite、libsqlite3-sys 和 flume。唯一业务调用方是 `bootstrap/import_sqlite/`，但 `main.rs`、`docs/architecture.md` 与 `docs/database.md` 均把 `import-sqlite` 定义为正式维护命令。
- 结论：成本真实存在，原建议的默认关闭 Cargo feature 却会让标准发布二进制缺少权威命令，不予采用。当前只收窄 SQLx 能力；SQLite v3 源库退役后，直接删除 `import-sqlite` 分支、`bootstrap/import_sqlite/`、相关外部测试和 SQLite feature，不保留兼容开关。

## P1 — 策略与卫生

### 4. reqwest / rustls 精确 pin 缺少就地说明，但动机已经确认
- 现状：`=0.12.28` / `=0.23.36` 从 scaffold 首提交 `fc47967f` 就存在。该提交的 `docs/dependency-policy.md` 明确要求：在 TLS 指纹复核证明新版本与真实 Codex Desktop 一致前保持两者固定。该文档后来随旧项目布局清理，但决策没有被推翻，Cargo.toml 也一直保留精确 pin。
- 最新实物复核：解包 2026-07-11 官方 latest DMG，应用版本为 `26.707.41301`（DMG SHA-256 `467a25381af2943f2c9b8794adae28b36400bd6da2753daf4889a5fd550c4d65`）。内置 Codex 二进制同时含 `reqwest 0.12.28` 与 `0.13.4`；对照同日 OpenAI Codex 源码 `5c19155c`，Codex API、core 与 http-client 主请求链仍使用 workspace `reqwest 0.12`，只有独立的 `rmcp-client` 使用 `0.13`。内置 `rustls` 仍为 `0.23.36`。因此不能因二进制出现 `reqwest 0.13.4` 就升级本项目主传输栈，当前两个精确 pin 与最新 Desktop 主链一致。
- provider 边界：最新 Codex 通过 `codex-utils-rustls-provider` 进程级安装 AWS-LC，以覆盖企业代理可能使用的 ECDSA P-521/SHA-512；本项目当前显式使用 ring。provider 切换会改变签名算法、构建链和 TLS 行为，不属于版本新旧问题，本轮不夹带修改。若决定追齐，必须作为独立 TLS 指纹变更，覆盖真实 OpenAI HTTP、SSE、WebSocket、自定义 CA 与发布镜像后再落地。
- 风险：精确 pin 会阻止 `cargo update` 自动接收后续补丁；当前 reqwest 0.12.28 已是 0.12 最新版，rustls 0.23 已有 0.23.41。本次 cargo audit 无漏洞，但未来 advisory 必须触发显式 TLS 指纹复核与版本修改。
- 结论：保留两个 pin，在 Cargo.toml 直接记录已核实的 Desktop 版本；升级前重新解包当时 latest Desktop，并用官方源码区分主链与旁路依赖。`security-scan.yml` 已在 push、pull request 与定时任务执行 cargo audit，无需重复新增门禁。

### 5. 九个 crate 多版本共存，根因三个，可控的只有 config 一条
- 现状(`cargo tree -d`):rand_core ×3(0.6.4 / 0.9.5 / 0.10.1)、getrandom ×3(0.2 / 0.3 / 0.4)、hashbrown ×3(0.14 / 0.16 / 0.17);rand、sha2、digest、crypto-common、block-buffer、cpufeatures 各 ×2。
- 根因拆解(逐个 `cargo tree -i` 核实):
  - **config→rust-ini 链**:hashbrown 0.14 与 getrandom 0.2 的 const-random 支——做 P0-1 顺带消掉;
  - **tungstenite fork 停在 rand 0.9 线**:rand 0.9.4 / rand_core 0.9.5 / getrandom 0.3 全部来自 openai-oss-forks 的 tungstenite,与直接依赖 rand 0.10 + sqlx-postgres 的 rand 0.10 线分叉——只有更新 fork 才能统一;
  - **RustCrypto 世代分裂**:argon2 0.5(blake2/password-hash)、sha1(tungstenite/headers)、sqlx-core 在 digest 0.10 线,sqlx-postgres 已迁 digest 0.11 线(sha2 0.11 / hmac 0.13 / md-5 0.11)——等 argon2 0.6 转正(现为 0.6.0-rc.8)可再收敛一档。hashbrown 0.16(sqlx 的 hashlink)与 0.17(indexmap 2.14)属生态过渡期,不可控。
- 建议:做 P0-1;fork 侧机会性跟进 rand 0.10;argon2 0.6 stable 后升级统一。其余接受现状,无需折腾。

### 6. tokio `test-util` feature 挂在生产 [dependencies]
- 现状:`tokio = { features = [..., "test-util"] }` 在主依赖表;全 `src/` 无 `pause/advance/start_paused` 使用,唯一使用方是 `tests/upstream/openai/transport/websocket.rs`。
- 问题:生产二进制的 tokio 编译进了测试专用的时间控制代码路径,属语义污染(体积影响小)。
- 建议:[dev-dependencies] 加 `tokio = { version = "1", features = ["test-util"] }`,主表删除该 feature。dev-deps 的 feature 在编译测试时做并集,单元/集成测试均不受影响。

## P2 — 例行升级与可选收窄,视精力而定

### 7. dirs 5.0.1 落后一个 major（crates.io 最新 6.0.0）
- 仅 `infra/paths.rs` 两处调用(`data_local_dir` / `home_dir`),升级面极小,顺手一并做。

### 8. axum 默认 features 可白名单（收益小）
- 实测未用 Form / ws / MatchedPath / OriginalUri / Multipart，只用 Query 与 Json；可收窄为 `features = ["http1", "json", "query", "tokio", "tracing"]` + `default-features = false`。改动后需要跑全量测试确认没有隐式依赖 tower-log 等。

### 9. tower-http 0.7.0 必须与 reqwest/TLS 指纹升级一起处理
- 0.7.0 是当前最新版，但固定的 reqwest 0.12.28 依赖 `tower-http ^0.6.8`。应用单独升级会让 0.6 与 0.7 两套 tower-http 同时进入依赖树，违背本轮收敛目标。
- 0.7.0 还包含 `ServeDir` 尾斜杠文件路径从 200 改为 404 的行为变化；本项目用 `ServeDir` 提供 SPA 静态文件与 fallback。结论是本轮保留 0.6.11，等 reqwest/TLS 指纹升级时同步升级，并覆盖静态文件、SPA fallback、未知 API 与尾斜杠路径。

### 10. futures facade 可换 futures-util（迁移退役后处理）
- 全仓使用的 Stream / StreamExt / TryStreamExt / SinkExt / join_all / stream::unfold 都来自 futures-util；但当前 sqlx-sqlite 同样依赖 futures-executor，现在替换直接依赖不能从构建树移除它。等 SQLite 导入路径删除后再替换，避免只有语义没有实际收益的改动。

### 11. 例行 `cargo update`
- 2026-07-11 的 `cargo update --dry-run` 显示 29 个兼容更新，包括 bytes 1.12.1、rand 0.10.2、rustls-pki-types 1.15.0、regex 1.13.0、time 0.3.53 等。按既有依赖策略独立刷新 lock，并执行完整质量门禁；精确 pin 与 major 版本不混入该步骤。

## 做得好的(记录一下,免得反复怀疑)

- **cargo-machete 全绿**：41 个直接依赖零未使用；原有 dev-deps 只有 tempfile + wiremock 两个，极简。
- **重量级 crate 的 feature 纪律好**:redis `default-features = false` 只开 tokio-comp/connection-manager/script(script 实测 `fleet/refresh/lease.rs:49` Lua 租约在用);reqwest `default-features = false`、TLS 明确走 rustls-tls-native-roots(全树无 openssl);rustls `default-features = false` 只开 ring/std/tls12;tokio 逐项列 feature 而不是无脑 `full`。
- **reqwest 四种解压 feature 与代码一致**:gzip/brotli/zstd/deflate 在 `transport/client.rs:81-84` 全部显式开启,指纹 Accept-Encoding 头(`bootstrap/config.rs:230`)也四种全声明——不是拍脑袋开的。
- **serde_json `preserve_order` + indexmap 是刻意设计**:透明代理与指纹头需要保序(`transport/headers.rs`、`fingerprint/*` 全用 IndexMap),与 /v1/responses 透明性目标一致,不是多余 feature。
- **无重复能力 crate**:HTTP client、JSON、时间库(chrono)、错误派生(thiserror)各一个;tungstenite + tokio-tungstenite 是基座+异步包装的正常组合,不算重复。
- **版本总体接近 latest**：axum 0.8.9、tokio 1.52.3、sqlx 0.9.0、redis 1.3.0、serde 1.0.228、tracing 0.1.44、chrono 0.4.45 等对照 crates.io 均为当前稳定版；明确例外是 TLS 指纹 pin、dirs/tower-http major 与待刷新的兼容 lock。cargo audit 零漏洞零警告。
- **release profile 配套到位**:lto=thin、codegen-units=1、strip=symbols、panic=abort,对 299 crate 的树该省的都省了;`rust-version = "1.95"` 明示 MSRV。

## 核实过的误报

- **rand_core 0.6.4 直接依赖"看似没人 use"**——实为 feature-enabler:`infra/identity.rs:4` 用的 `password_hash::rand_core::OsRng` 要求 rand_core 的 `getrandom` feature,而 argon2 0.5.3 的 features 表(已核实)没有透传开关,只能由顶层依赖做 feature 并集打开。**不能删**;建议在 Cargo.toml 加一行注释防止后人误删(machete 恰好因 `password_hash::rand_core::` 的字符串巧合不报它,属侥幸而非可靠)。
- **tungstenite 0.27 + tokio-tungstenite 0.28 版本"错位"不是 bug**:两者都被 `[patch.crates-io]` 指到 openai-oss-forks 的固定 rev,fork 内部自洽;deflate/proxy feature 也确有使用(`transport/websocket.rs:280` permessage-deflate)。
- **直接依赖 sha2 0.10 不升 0.11 是对的**:0.10 线还被 argon2(blake2)、sha1、sqlx-core 占着,单独升直依赖消不掉旧版本,反而破坏 password-hash 生态兼容。
- **flate2 与 reqwest 的 gzip 不重复**:前者是自更新 tar.gz 的同步解包(`update/archive.rs`),后者是 HTTP 响应异步解压,场景不同,各自成立。
- **`cargo tree -d` 里 tokio/thiserror/uuid/chrono/serde_json 同版本出现两次不是重复版本**:那是 sqlx proc-macro 的 host 侧构建单元,同版本、不同构建上下文,正常现象,不可也不必消除。
- **flume "凭空"出现在树里**：确认只被 sqlx-sqlite 拉入；当前正式导入命令仍保留，因此它会在 SQLite 路径退役时一起消失，无需单独处理。

## 建议动手顺序

1. **feature 卫生**：config 只开 YAML；SQLx 关闭默认 features 并把 `sqlite` 收窄为 `sqlite-bundled`；tokio `test-util` 与 SQLx `derive` 下沉 dev-dependencies；axum 使用显式白名单。
2. **决策就地化**：为 reqwest/rustls pin 与 rand_core feature-enabler 写短注释，CI cargo audit 保持现状。
3. **例行升级**：dirs 升 6，执行兼容范围内的 cargo update，并跑完整门禁。
4. **TLS 栈升级时同步处理 tower-http**：本轮保持 0.6.11；未来 reqwest 指纹复核通过后同步升级 0.7，并执行 assets 路由与全量测试。
5. **迁移退役清理**：确认 SQLite v3 源库不再需要后，一次删除 import-sqlite、SQLite 依赖和相关测试；随后把 futures facade 换成 futures-util。此前不引入 feature 兼容路径。
