# 架构重构方案 B（精炼版）：能力域架构，精确到文件

> 状态：提案 / 待评审
> 取代：本文档第一版的粗略结构。对比对象：`architecture-layered-plan.md`（方案 A）。

## 铁律

**每个目录都能用一句话说清，没有任何目录身兼两职。** 尤其：「对接 chatgpt.com 的脏活」只能出现在 `codex/gateway/` 一处。

## 顶层三域 + 横切

```text
admin/      后台管理（控制面）         ──┐ 消费 codex 能力
codex/      代理能力（产品本体）        ←─┘ ──┐ 依赖底座
platform/   共享内核（存储/加密/身份）   ←─────┘
config/ utils/   横切，被所有层使用
app/        组装三者
```

依赖方向单向无环：`admin → codex → platform`。codex 不知道 admin 存在（纯代理可独立运行），admin 只是套在外面的管理界面。

## 完整目录树（精确到文件）

```text
src/
├── main.rs
├── lib.rs
│
├── app/                              # 🔧 组装层
│   ├── mod.rs
│   ├── state.rs                      #   AppState { codex, admin, platform }
│   ├── router.rs                     #   挂 codex::serving::http + admin::http + platform 中间件
│   ├── bootstrap.rs                  #   构建依赖图
│   └── tasks/                        #   ⭐ 后台任务协调器（统一启停所有域的任务）
│       ├── mod.rs
│       ├── types.rs                  #     ← scheduler/types.rs (SchedulerHandle / SchedulerError)
│       └── coordinator.rs            #     ← 新增：tokio::select! 统一启停
│
├── codex/                            # 🟢 codex 能力域（产品本体）
│   ├── mod.rs
│   │
│   ├── serving/                      #   ① 对客户端：服务 /v1 请求
│   │   ├── mod.rs
│   │   ├── http/                     #     薄 axum handlers ← http/v1/
│   │   │   ├── mod.rs
│   │   │   ├── router.rs             #       ← http/v1/router.rs
│   │   │   ├── auth.rs               #       ← http/v1/auth.rs（校验 cpr_ key，调 platform::identity）
│   │   │   ├── errors.rs             #       ← http/v1/errors.rs（OpenAI 兼容错误体）
│   │   │   ├── chat.rs               #       ← http/v1/chat.rs
│   │   │   ├── responses.rs          #       ← http/v1/responses.rs
│   │   │   ├── models.rs             #       ← http/v1/models.rs
│   │   │   └── diagnostics.rs        #       ← http/diagnostics.rs（公开诊断路由）
│   │   ├── chat.rs                   #     Chat 用例服务 ← service/chat.rs
│   │   ├── responses.rs              #     Responses 用例服务 ← service/responses.rs
│   │   ├── diagnostics.rs            #     诊断快照逻辑 ← service/diagnostics.rs
│   │   └── dispatch/                 #     上游编排 ← codex/upstream/
│   │       ├── mod.rs                #       ← codex/upstream/mod.rs（CodexUpstreamService）
│   │       ├── dispatch.rs           #       ← codex/upstream/dispatch.rs
│   │       ├── fallback.rs           #       ← codex/upstream/fallback.rs（429/402/403 换号）
│   │       ├── refresh.rs            #       ← codex/upstream/refresh.rs（401 刷新重试）
│   │       ├── stream.rs             #       ← codex/upstream/stream.rs（SSE 收集）
│   │       ├── usage.rs              #       ← codex/upstream/usage.rs（用量记录）
│   │       └── affinity.rs           #       ← service/session_affinity.rs（sticky 路由）
│   │
│   ├── accounts/                     #   ② 账号资产域 ← codex/accounts/
│   │   ├── mod.rs
│   │   ├── model.rs                  #     ← codex/accounts/model.rs
│   │   ├── pool.rs                   #     ← codex/accounts/pool.rs（调度/槽位）
│   │   ├── lifecycle.rs              #     ← codex/accounts/lifecycle.rs
│   │   ├── repository.rs             #     ← codex/accounts/repository.rs
│   │   ├── cf_path_block.rs          #     ← codex/accounts/cf_path_block.rs
│   │   ├── usage_snapshots.rs        #     ← codex/accounts/usage_snapshots.rs
│   │   ├── service/                  #     ← codex/accounts/service/（账号业务操作）
│   │   │   ├── mod.rs
│   │   │   ├── cookies.rs
│   │   │   ├── health.rs
│   │   │   ├── import.rs             #       手动/CLI/OAuth 导入
│   │   │   ├── mutation.rs
│   │   │   ├── quota.rs
│   │   │   ├── refresh.rs
│   │   │   └── runtime_pool.rs
│   │   ├── cookies/                  #     account-scoped cookies ← codex/cookies/
│   │   │   ├── mod.rs
│   │   │   ├── jar.rs
│   │   │   └── repository.rs
│   │   └── models/                   #     模型目录（按 plan）← codex/models/
│   │       ├── mod.rs
│   │       ├── catalog.rs
│   │       ├── repository.rs
│   │       └── service.rs
│   │
│   ├── gateway/                      #   ③ ⭐ 连 chatgpt.com（外部对接细节唯一处）
│   │   ├── mod.rs
│   │   ├── transport/                #     ← codex/transport/
│   │   │   ├── mod.rs
│   │   │   ├── client.rs             #       Pinned TLS + Desktop 指纹头
│   │   │   ├── headers.rs
│   │   │   ├── sse.rs
│   │   │   ├── websocket.rs
│   │   │   ├── types.rs
│   │   │   └── usage.rs
│   │   ├── protocol/                 #     ← codex/protocol/（OpenAI ↔ Codex）
│   │   │   ├── mod.rs
│   │   │   ├── codex_to_openai.rs
│   │   │   ├── openai_to_codex.rs
│   │   │   ├── schema.rs
│   │   │   └── error.rs
│   │   ├── oauth/                    #     ← codex/oauth/
│   │   │   ├── mod.rs
│   │   │   ├── client.rs
│   │   │   ├── refresh.rs
│   │   │   ├── token.rs
│   │   │   └── cli_import.rs
│   │   └── fingerprint/              #     ← codex/fingerprint/
│   │       ├── mod.rs
│   │       ├── model.rs
│   │       ├── repository.rs
│   │       └── updater.rs
│   │
│   ├── logs/                         #   ④ 事件日志域（serving 产生，admin 读）
│   │   ├── mod.rs                    #     ← logs/mod.rs
│   │   ├── event.rs                  #     ← logs/event.rs
│   │   ├── repository.rs             #     ← logs/repository.rs
│   │   ├── rotation.rs               #     ← logs/rotation.rs
│   │   └── service.rs                #     ← service/log.rs
│   │
│   ├── usage/                        #   ⑤ 用量统计域
│   │   ├── mod.rs
│   │   └── service.rs                #     ← service/usage.rs
│   │
│   └── tasks/                        #   ⑥ codex 域后台调度 ← scheduler/
│       ├── mod.rs
│       ├── refresh.rs                #     ← scheduler/refresh.rs（令牌刷新）
│       ├── quota.rs                  #     ← scheduler/quota.rs（配额解锁）
│       └── model.rs                  #     ← scheduler/model.rs（模型刷新）
│
├── admin/                            # 🔵 后台管理域（控制面）
│   ├── mod.rs
│   ├── http/                         #   /admin/* ← http/admin/
│   │   ├── mod.rs                    #     ← http/admin/mod.rs
│   │   ├── router.rs                 #     ← http/admin/router.rs
│   │   ├── response.rs               #     ← http/admin/response.rs（AdminError envelope）
│   │   ├── auth.rs                   #     ← http/admin/auth.rs（login/status/logout）
│   │   ├── api_keys.rs               #     ← http/admin/api_keys.rs
│   │   ├── logs.rs                   #     ← http/admin/logs.rs
│   │   ├── models.rs                 #     ← http/admin/models.rs（refresh-models）
│   │   ├── settings.rs               #     ← http/admin/settings.rs
│   │   ├── usage.rs                  #     ← http/admin/usage.rs
│   │   ├── diagnostics.rs            #     ← http/admin/diagnostics.rs
│   │   └── accounts/                 #     ⭐ 拆分 http/admin/accounts.rs（1697 行）
│   │       ├── mod.rs                #       路由注册 + 共享 DTO
│   │       ├── list.rs               #       accounts / export_accounts
│   │       ├── create.rs             #       create_account / import_accounts / import_cli_auth
│   │       ├── mutate.rs             #       refresh / reset_usage / label / status / batch_status
│   │       ├── delete.rs             #       delete_account / batch_delete
│   │       ├── health.rs             #       health_check_accounts
│   │       ├── quota.rs              #       quota_warnings / account_quota
│   │       ├── cookies.rs            #       get / set / delete cookies
│   │       └── oauth.rs              #       ← http/admin/auth.rs 中 OAuth 流程
│   │                                 #         device-login / poll / login-start / code-relay / callback
│   ├── auth/                         #   管理员认证 + key 管理策略
│   │   ├── mod.rs
│   │   ├── service.rs                #     ← service/admin_auth.rs（密码/会话校验）
│   │   └── api_key.rs                #     ← service/api_key.rs（key CRUD 管理策略）
│   ├── settings.rs                   #     ← service/settings.rs（运行时配置写 local.yaml）
│   └── tasks/
│       ├── mod.rs
│       └── session_cleanup.rs        #     ← scheduler/session_cleanup.rs
│
├── platform/                         # 🔌 共享内核（纯数据/加密/身份，无业务策略）
│   ├── mod.rs
│   ├── storage/                      #   ← storage/
│   │   ├── mod.rs
│   │   └── db.rs
│   ├── identity/                     #   ⭐ 跨域身份原语（admin 管 / codex 验，向下收口）
│   │   ├── mod.rs                    #     ← auth/mod.rs
│   │   ├── api_key.rs                #     ← auth/api_key.rs（类型/哈希/verify）
│   │   ├── api_key_repository.rs     #     ← auth/api_key_repository.rs
│   │   ├── admin_session.rs          #     ← auth/admin_session.rs
│   │   └── error.rs                  #     ← auth/error.rs
│   ├── crypto.rs                     #   ← utils/crypto.rs
│   └── http/                         #   共享 HTTP 横切 ← http/
│       ├── mod.rs                    #     ← http/mod.rs
│       ├── middleware.rs             #     ← http/middleware.rs
│       ├── health.rs                 #     ← http/health.rs
│       └── auth.rs                   #     ← http/auth.rs（从 header 提取 cpr_ / session）
│
├── config/                           # ⚙️ 配置 ← config/（不变）
│   ├── mod.rs
│   ├── loader.rs
│   └── types.rs
│
└── utils/                            # 🛠️ 通用工具
    ├── mod.rs
    ├── json.rs
    └── pagination.rs
```

## 三处需要拆分/拆解的非纯移动

大部分是目录搬迁，但有 3 个文件需要真正动刀：

1. **`http/admin/accounts.rs`（1697 行）→ `admin/http/accounts/` 9 文件**。按上面 16 个 handler 归类（list/create/mutate/delete/health/quota/cookies/oauth + mod 路由）。

2. **`service/admin_auth.rs` 拆责**。密码登录/会话校验留 `admin/auth/service.rs`；OAuth token 交换 + 账号导入的编排移到 `admin/http/accounts/oauth.rs`，由它调用 `codex::gateway::oauth`（换 token）+ `codex::accounts::service::import`（落库）。

3. **api-key 一分为二**。校验原语（哈希/verify/repo）下沉 `platform/identity/`；管理策略（创建/禁用/导入导出）留 `admin/auth/api_key.rs`。这样 `/v1` 校验（codex）和 `/admin` 管理（admin）都向下依赖 platform，杜绝 codex→admin 反向依赖。

## 跨域接缝归属

| 接缝 | 管理方 | 使用方 | 落点 |
| --- | --- | --- | --- |
| 客户端 API Key (`cpr_`) | admin | codex/serving | `platform/identity/api_key*.rs` |
| Admin Session | admin | admin + platform 中间件 | `platform/identity/admin_session.rs` |
| 账户存储 | admin（增删改） | codex（池调度） | 数据在 `platform/storage`；runtime pool 在 `codex/accounts`；admin 调 `codex::accounts::service` |
| 事件日志 | admin（读/清） | codex/serving（写） | `codex/logs/`，admin→codex 正向读 |

## 迁移执行顺序（分层提交，每步 `cargo build` 通过才提交）

1. `platform/`（storage + crypto + identity + http 横切）→ 改引用 → build ✅ → commit
2. `codex/gateway/`（transport/protocol/oauth/fingerprint）→ build ✅ → commit
3. `codex/accounts/`（含 cookies/models 内迁）→ build ✅ → commit
4. `codex/serving/`（v1 http + chat/responses + dispatch + affinity + diagnostics）→ build ✅ → commit
5. `codex/{logs,usage,tasks}/` → build ✅ → commit
6. `admin/`（http 拆分 accounts + auth 拆责 + tasks）→ build ✅ → commit
7. `app/tasks/` 协调器 → build ✅ → commit
8. 更新 `tests/` 引用 → `cargo test` ✅ → commit
9. 删除空目录（旧 codex/、http/、service/、scheduler/、auth/、logs/、storage/）→ 最终验证 → commit

---

## 与方案 A 的最终对比

| 维度 | 方案 A：分层（横切） | 方案 B 精炼：能力域（纵切） |
| --- | --- | --- |
| 顶层解开「后台 vs codex」 | ❌ 纠缠下沉进 `core/` | ✅ 顶层即断层线 |
| `codex` 是否还身兼两职 | infra/codex 仅对接（OK），但 core 混业务 | ✅ gateway 仅对接，serving/accounts 仅业务 |
| `core/` god-package 风险 | 高 | 无 core/ |
| 「HTTP 在哪」单一答案 | ✅ 都在 api/ | ❌ 各域 http/ 内（但每域内聚） |
| 改一个特性触达面 | 散在 api+core+tasks+infra | 多数在单域内 |
| 定时任务归属 | 独立 tasks/ 层 | 跟域走，app/ 仅协调 |
| 跨域接缝 | 被 core/ 掩盖 | 显式收口 platform/identity |
| reviewer 学习成本 | 低（教科书） | 中（模块化单体/DDD） |

**结论**：方案 B 精炼版才真正根治你反复撞的痛点——`codex` 这个词从此只在 `gateway/` 表示「对接」，业务用 `serving/accounts/logs/usage` 领域名表达，后台是 `admin/` 控制面。方案 A 只是把 `codex/` 改名成技术层，纠缠搬进了 `core/`。
