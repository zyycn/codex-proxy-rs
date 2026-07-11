# API 契约一致性审查(backend HTTP API)

> 审查于 `feat/postgres-redis-migration` 工作树(fleet 改名之后)。覆盖 `api/client/`(OpenAI 兼容面)、`api/admin/`(管理台面)、`api/router.rs` 路由组织、`infra/json.rs` 分页原语、以及前端 `frontend/src/api/` 消费侧的类型对齐。方法:codegraph + rg 定位,逐个 endpoint 对照 serde struct 与 axum 路由,前后端字段名两侧核对。
>
> 状态：2026-07-11 已完成 P0 第 1 条、P1 第 2/3 条和 P2 第 4/7/8 条的代码实施与全量质量门禁。下文“现状”均指修复前审查基线。

先说结论:字段命名极其干净(全局 camelCase,95 处 `rename_all = "camelCase"`,无一处 snake_case 泄漏),两套错误信封各自服务两类客户端且都不泄露内部错误细节,OpenAI 兼容面贴合官方契约。**真问题集中在分页**:`infra/json.rs` 里 cursor 与 page/offset 两套并存,导致三种矛盾——(1)keys endpoint 只有 cursor 一套,前端不翻页 → **超过 50 个 key 被静默截断**(唯一的真数据丢失 bug);(2)同一 endpoint 的 `data.page` 形状随请求参数在两种 schema 间漂移,且用 untagged enum 无判别字段;(3)cursor 分支在 accounts/usage/ops 上已是前端不走的死代码。其余是 REST 方法语义(`POST /delete` vs `DELETE`)、版本化(admin 无 `/v1`)、compact 与 responses 的上游错误 status 映射分叉等风格/边角不一致。

## P0 — 会导致客户端出错的真问题

### 1. `/api/admin/keys` 只有 cursor 分页,前端不跟游标 → 超过 50 个 key 静默丢失
- 现状:keys endpoint 是**唯一只支持 cursor 一套**的列表接口。`api_keys`(`api/admin/keys_routes.rs:114-139`)只接收 `ApiKeysQuery { cursor, limit }`(`keys_routes.rs:26-31`),不认 `page`/`pageSize`;`limit = clamp_limit(query.limit.unwrap_or(50))` 默认 50、上限 200。
- 前端 `getApiKeys()`(`frontend/src/api/modules/api-keys.ts:5-10`)**不传任何参数**,`loadApiKeys` 直接 `apiKeys.value = data.items`(`frontend/src/views/api-keys/composables/useApiKeyMutations.ts:38-39`),`next_cursor` 被丢弃;`useApiKeyFilters` 再对这已被截断的数组做**纯客户端分页**(`frontend/src/views/api-keys/composables/useApiKeyFilters.ts:21-22`)。
- 后果:一旦 key 总数 > 50,前端永远只看得到**最新创建的 50 个**,较旧密钥永久不可见,分页器还显示得像是全量。这是本次审查唯一的**真数据可见性 bug**,不是洁癖。
- 修复:keys 已统一实现 `page`/`pageSize`/`search` + `count(*)`,前端直接消费服务端页和 `data.page.total`,搜索覆盖名称、标签、ID 的完整数据集,不再对单页结果做本地搜索或二次分页。

## P1 — 契约稳定性

### 2. 同一 endpoint 的 `data.page` 形状随请求参数在两套 schema 间漂移
- 现状:accounts / usage / ops 三个列表同时支持两种模式,按"请求里有没有 `page`/`pageSize`"运行时二选一:
  - `usage_records`(`api/admin/usage_routes.rs:197`)`use_numbered_page = page.is_some() || page_size.is_some()`;
  - `ops_errors`(`api/admin/ops_routes.rs:66`)同款判定;
  - `accounts`(`api/admin/accounts_routes/mod.rs:525`)同款判定。
- 响应用 untagged enum `PageMeta`(`api/admin/response.rs:210-215`)承载两种截然不同的 `data.page`:
  - Cursor 分支 → `{ limit, nextCursor }`(`response.rs:193-197`);
  - Numbered 分支 → `{ page, pageSize, total, totalPages }`(`response.rs:200-207`)。
- 问题:同一个 URL,传 `?cursor=` 与传 `?page=` 拿到的 `data.page` 是两种 schema;`#[serde(untagged)]` 又不带判别字段(没有 `"mode":"cursor"` 之类),第三方 API 消费者只能靠"猜哪些字段在"来分辨。前端因为**永远只传 numbered 参数**(`useUsageRecordsTable` 传 `page/pageSize`、`useAccountMutations` 传 `page/pageSize`、`useOpsErrorsTable` 读 `result.page.total/page/pageSize/totalPages`)侥幸无感,但契约面是脆的。
- 建议:统一收敛到 numbered 一套(前端事实上只用它),响应 `data.page` 定为单一稳定 schema;若要保留 cursor 供未来无限滚动,至少给 `PageMeta` 加显式判别字段,别用 untagged。
- 修复:accounts / keys / usage / ops 已统一为 numbered 分页,`PageMeta` 已收敛为单一结构 `{ page, pageSize, total, totalPages }`,默认请求也不再产生另一种 schema。

### 3. cursor 分页在 accounts/usage/ops 上已是前端不走的死代码
- 现状:`infra/json.rs` 同时导出 cursor 原语(`Page`/`encode_cursor`/`decode_cursor`,`json.rs:11-19`、`36-46`)与 page/offset 原语(`NumberedPage`/`page_offset`,`json.rs:22-33`、`59-61`)。每个双模 handler 都完整保留了 cursor 分支(如 `usage_routes.rs:230-253`、`ops_routes.rs:88-100`、`accounts_routes/mod.rs:557-579`),但前端全部只走 numbered 分支。
- 唯一还真用 cursor 的调用方是 `dashboard_summary` 内部 `list(None, LIMIT, ...)` 取最近 N 条(`api/admin/dashboard_routes.rs:214`)——它只要"第一页",本质是 `limit` 而非翻页。
- 额外死代码:`AccountUsageQueryService::list` 与 `PgAccountUsageStore::list_usage` 没有 HTTP 或其他生产调用方,同样只保留了无消费者的 cursor SQL。
- 问题:两套原语 + 每 handler 双分支,是这组契约不一致(第 1、2 条)的根因,也是持续的维护税与测试盲区。
- 建议:与第 2 条一起做——对外列表统一 numbered;cursor 若仅服务 dashboard 的"取前 N 条",可降级成一个不暴露翻页语义的 `list_recent(limit)`,把 `Page`/`encode_cursor` 从公共分页原语里摘出去。
- 修复:Dashboard 已改用显式 `list_recent(limit)`,`Page`/`encode_cursor`/`decode_cursor`、各列表 cursor 分支和账号用量 cursor 死路径均已删除。

## P2 — 风格不统一 / 边角契约,视精力而定

### 4. `/v1/responses` 与 `/v1/responses/compact` 对同一上游失败映射出不同 HTTP status
- 现状:非流式 `/v1/responses` 走 `responses_dispatch_error_response`(`api/client/responses.rs:316`),内部用 `ResponseDispatchStatusMode::UpstreamFailureStatus`,对 `Upstream(_)` **强制 502**(`api/client/errors.rs:266-276`);compact 走 `responses_compact_dispatch_error_response`(`responses.rs:356`),用 `ResponseDispatchStatusMode::Client`,**透传上游真实 status**(如 500/503,`errors.rs:118-132`)。
- 问题:上游同一个 5xx,responses 返回 502、compact 返回 500——两个姊妹 endpoint 对"同类错误"给客户端不同 status。二者都自洽(代理返 502 更规范;compact 透传更贴上游),但彼此分叉。
- 建议:统一策略(建议都按代理语义规整为 502/503),或在文档里显式标注 compact 为"透传上游 status"。
- 修复:Responses 与 Compact 共用同一客户端状态策略。可由客户端修复的上游 4xx 保留原状态;上游 503 保留为 503;其他通用上游 5xx 统一为 502。PostgreSQL 运维日志的 `client_status_code` 使用同一计算。

### 5. admin 写操作用 `POST /xxx/delete`、`POST /xxx/update`,与 settings 的 `DELETE` 语义自相矛盾
- 现状:同一 admin 面里,删除/更新混用两种风格:
  - 动词塞进路径 + POST:`POST /api/admin/accounts/delete`、`/accounts/update`、`/keys/delete`、`/keys/update`、`/usage/records/delete`(`api/admin/router.rs:72-73`、`101`、`105-106`);
  - 又存在规范的 `DELETE /api/admin/settings/admin-api-key`(`router.rs:44`)。
- 问题:纯 REST 语义不一致(POST 造资源 vs DELETE 删资源被 `POST /delete` 绕过)。因前端硬编码 method,不会真出错,属洁癖级。
- 建议:要么全面 `DELETE`/`PATCH`,要么承认"动作式 POST"约定并统一到底、连 settings 那个 DELETE 一起归拢;不必强改,但应二选一写进规范。

### 6. client 面 `/v1` 版本化,admin 面 `/api/admin` 无版本
- 现状:OpenAI 面全部 `/v1/*`(`api/client/router.rs:17-27`);admin 面全部 `/api/admin/*` 无版本段(`api/admin/router.rs`)。
- 问题:两面版本化策略不一致。admin 是内部管理台、前后端同仓齐发,无版本可接受;仅记录以免误会成疏漏。
- 建议:维持现状即可;若未来 admin 要对外开放,再引入 `/api/admin/v1`。（这个不做）

### 7. `code: "codex_api_error"` 在两种 `type` 下复用
- 现状:`responses_error_kind_for_status`(`api/client/errors.rs:249-264`)对 4xx 上游返回 `{ type: "invalid_request_error", code: "codex_api_error" }`,对 5xx 返回常量 `CODEX_API_ERROR = { type: "server_error", code: "codex_api_error" }`(`errors.rs:85-88`)。同一 `code` 字符串挂在两种 `type` 下。
- 问题:按 `(type, code)` 对做分支的客户端会看到同 code 不同 type。非阻断。
- 建议:给 4xx 场景换一个独立 code(如 `codex_client_error`),让 code 与 type 一一对应。
- 修复:通用上游 4xx 使用 `type: "invalid_request_error"` + `code: "codex_client_error"`;`codex_api_error` 只用于服务端错误类型。

### 8. 流式与非流式对"无可用账号"给出不同 error code
- 现状:非流式 `NoActiveAccount` → HTTP 503 + `code: "no_available_accounts"`(`api/client/errors.rs:144-155`);流式则以 SSE(HTTP 200)内嵌 `response.failed` 事件,kind 走 `UPSTREAM_UNAVAILABLE_ERROR` → `code: "upstream_unavailable"`(`errors.rs:53-56`、`338`)。
- 问题:同一"无账号"条件,流式/非流式 code 命名不同(`upstream_unavailable` vs `no_available_accounts`)。HTTP status 差异是流式固有(200 后无法改 status),但 code 字符串可以对齐。
- 建议:两条路径统一 code 命名。
- 修复:流式 `response.failed` 与非流式响应均使用 `code: "no_available_accounts"`。

### 9. 前端仅 `system` 模块有 TS 类型,其余全 `any` —— 无编译期漂移防护
- 现状:`system.ts` 定义了 `SystemVersion`/`SystemUpdateInfo` 等接口(`frontend/src/api/modules/system.ts:7-33`),经核对与后端 `VersionData`(`update/service.rs:43-54`)、`UpdateInfoData`(`update/release.rs:25-39`)**字段逐一吻合**;但 accounts/usage/dashboard/keys/settings 模块的入参与返回全是 `any`(如 `api/modules/accounts.ts:5`、`usage.ts:3`)。
- 问题:高频实体(账号/key/用量)没有一处 TS 契约,后端改字段名前端零编译告警,只会运行时 `undefined`。当前无实际错配(camelCase 两侧一致,抽查 `OpsErrorDetailModal` 读 `failureClass/clientApiKeyId/upstreamRequestId` 与 `OpsErrorLog` flatten 序列化对得上),但缺防护网。
- 建议:为账号/key/用量/设置补最小 TS interface(可从后端 struct 半自动生成),把 `request<T>` 的泛型用起来。

## 做得好的(记录一下,免得反复怀疑)

- **字段命名全局统一 camelCase**:95 处 `rename_all = "camelCase"`,仅 3 处 `lowercase` 且都用在枚举变体序列化(`telemetry/usage/types.rs:10`、`telemetry/dashboard.rs:28`、`update/service.rs:67`,产出 `"info"/"success"` 这类小写值),用法正确,不算不一致。
- **内部错误细节不泄露**(抽查确认):admin 各错误类型(`UsageQueryError`/`OpsQueryError`/`AccountManageError`/`KeyManageError`)的 `Display` 全是静态文案(如 `"failed to list usage records"`),`AdminError::internal(error.to_string())` 落地的是这些静态串而非裸 DB 错误;settings 更是刻意把 `Database/Json/StoredField` 统一收敛成 `"Failed to persist settings"`(`api/admin/settings_routes.rs:155-164`)。少数带 detail 的(`FetchQuota`/`OAuthCodeExchange`,`accounts_routes/quota_routes.rs:38`)是上游 Codex 报文且仅面向已鉴权 admin,可接受。
- **两套错误信封各服务其众且都自洽**:admin 用 `{ code:u32, message, data:null }` + 业务码段(400xx/401xx/409xx/500xx,`api/admin/response.rs:13-29`),OpenAI 面用 `{ error:{ message, type, code } }`(`api/client/errors.rs:91-107`)贴官方结构。这是按受众分层,不是分裂。
- **OpenAI 兼容度到位**:`/v1/models` 返回 `{ object:"list", data:[{ id, object:"model", created, owned_by:"openai" }] }`(`api/client/models.rs:35-42`、`92-99`),`/v1/models/{id}` 返回单 model 对象,SSE 用 `text/event-stream` + `data: [DONE]\n\n` 收尾(`api/client/sse.rs`、`upstream/openai/protocol/sse.rs:23`),均合官方契约;`/v1/responses` 是透明代理原样透传。
- **admin 鉴权入口统一**:`AdminAuth` 提取器一处实现(会话 Cookie 或 `x-api-key`,`api/admin/session.rs:45-58`),所有 admin handler 走同一门。
- **SPA fallback 对 API 路径正确回 404 JSON**:`/api`、`/v1` 前缀命中时返回 `{ code:40401, message, data:null }` 而非把 index.html 当接口喂回(`api/assets.rs:29-39`),与 admin 错误信封同构。
- **request_id 契约闭环**:入站校验/生成、注入 extension、回写响应头 `x-request-id`(`api/middleware/request_id.rs`),前端 `request.ts` 从 `response.headers['x-request-id']` 取回挂到 `ApiError`(`frontend/src/api/request.ts:71`)。

## 核实过的误报(排查后判定无问题)

- **`system` 前端接口 vs 后端 struct 疑似漂移**:逐字段核对 `SystemVersion`/`SystemUpdateInfo` 与 `VersionData`/`UpdateInfoData`,camelCase 名称全部一致,无漂移。
- **query 参数疑似 snake_case**:各 `*Query` struct 字段名虽写作 `client_api_key_id`/`page_size`/`start_time`,但 struct 上都有 `rename_all = "camelCase"`,实际接收的是 `clientApiKeyId`/`pageSize`/`startTime`,与前端发送一致。
- **`AdminAccountPageEnvelope` 是不是又一套分页 schema**:它(`accounts_routes/mod.rs:355-398`)确实手写重复了 `AdminPageEnvelope` 的 cursor/numbered 逻辑,但**产出的 `data.page` 形状与共享 `PageMeta` 完全一致**,只是多挂了一个 `data.summary`。属代码冗余(可复用 `AdminPageEnvelope`),非契约不一致,故不计入 P 级问题。
- **client 面 401 消息"Missing client API key"用词**:key 存在但非法时也回这句(`api/client/auth.rs` 只对外暴露 bool,失败原因仅进日志),消息略含糊但 status 正确、不泄露 key,属文案洁癖非契约错。

## 建议动手顺序

1. **P0 第 1 条**:已完成。keys 使用服务端 numbered 分页、全量搜索和总数统计。
2. **P1 第 2、3 条**:已完成。对外列表统一 numbered,Dashboard 使用 `list_recent(limit)`,cursor 原语与死路径已删除。
3. **P2 第 4、7、8 条**:已完成。Responses/Compact/流式错误状态与错误码已统一。
4. **P2 第 9 条**(前端补 TS 类型)可作为独立收尾,给账号/key/用量/设置建最小 interface。
5. **P2 第 5、6 条**(REST 方法语义 / 版本化)属规范决策,择期定调即可,不必强改。

## 实施验收

- Rust 格式检查通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings` 通过,0 警告。
- 后端全量集成测试 623 项通过,0 失败。
- 前端 Prettier 检查、`vue-tsc` 和 Vite 生产构建通过,0 警告。
- Docker runtime 镜像 `codex-proxy-rs:api-contract-review` 构建成功;容器健康检查为 healthy,`GET /healthz` 返回 204,启动日志 WARN/ERROR 为 0。
