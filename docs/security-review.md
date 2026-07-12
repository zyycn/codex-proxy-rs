# 安全审查(backend)

> 审查于 `feat/postgres-redis-migration` 工作树(fleet 改名之后)。覆盖静态凭据存储(PG 明文 vs 加密)、鉴权路径(admin 密码 / admin API key / client API key / 会话)、客户端 IP 信任边界、限流与爆破防护、admin 会话生命周期、自更新供应链、secrets 加载与泄露面、CORS / 安全响应头。所有结论均已用工具核到 `file:line`。

先说结论:**鉴权原语这一层做得很扎实**——admin 密码 argon2、admin API key 与会话 token 都是 SHA-256 摘要后再 `subtle::ConstantTimeEq` 比对、会话 token 256-bit 熵且 Redis 侧只存摘要、OAuth 走 PKCE+state、SQL 全参数化、容器非 root。真正的短板集中在**"静态凭据明文落库"**和**"信任边界"**两类。最需要处理的三条:(1) 上游 OAuth `access_token`/`refresh_token` 在 PG 明文存储;(2) client API key 同样明文存储且鉴权靠明文点查(与 admin key 的"存摘要"做法不一致);(3) `X-Forwarded-For` 等转发头无条件信任,导致登录失败锁定可被逐请求换 IP 绕过。这三条一条是"服务的核心资产裸奔",一条是"唯一的爆破防线形同虚设",都值得优先动手。

需要强调的是:明文落库两条属于**latent(潜在)风险——利用前提是攻击者已经能读到 PG**(备份泄露、只读副本、pg_hba 配错、内鬼、或未来引入的 SQLi)。当前代码 SQL 全参数化、没找到注入面,外部攻击者并没有现成路径读库。但对一个"核心资产就是这些 token"的服务,at-rest 加密是行业基线,所以仍按高优先级列出,而不是制造"正在被利用"的假紧迫感。

## P0 — 静态凭据明文落库

### 1. 上游 OAuth `access_token` / `refresh_token` 在 PostgreSQL 明文存储

- 现状:建表 `infra/migrations/0001_initial.sql:46-47` 是 `access_token text not null` / `refresh_token text`,没有任何加密列或 pgcrypto。写入侧 `fleet/store/write.rs:18-19` 直接 `.bind(account.access_token.expose_secret())` + 绑定 expose 后的 refresh_token,即**明文入库**。内存里的 `secrecy::SecretString` 只负责防止 Debug/日志误打印(抽查 `fleet/store/rows.rs`、`export.rs` 均正确走 `ExposeSecret`),**到了 PG 就退化成明文**。
- 问题:这是整个服务最高价值的资产——一批可直接调用上游的 OAuth token。任何 DB 层面的暴露(备份/快照泄露、只读副本、误配 `pg_hba.conf`、运维/内鬼、未来的 SQLi)= 全量 token 立即可用,攻击者零额外成本(不需要爆破,不需要解密)。`refresh_token` 尤其致命:即便 access_token 短时过期,refresh_token 可持续换新。
- 建议(按投入产出排序):
  1. **应用层 AEAD 加密**(推荐):用 `chacha20poly1305`/`aes-gcm`,主密钥从环境变量/KMS 注入(**不要**写进 `config.yaml`),写库前加密、读库后解密,`SecretString` 边界几乎不用改。附带一次密钥轮换预案。
  2. 或 **pgcrypto `pgp_sym_encrypt`**:改动集中在 SQL,但密钥仍要在连接串/会话里传,防护面弱于应用层,且密钥容易随查询进日志。
  3. 若短期不做加密,则必须**文档化并加固 DB 访问面**:副本/备份加密、最小化 `pg_hba` 网段、独立库权限,并在部署文档里明确"PG 一旦泄露即等同全量 token 泄露"。
- 定级说明:利用前提是"已能读 PG",属 latent;但资产价值 + 修复成本低,按 P0 处理。

### 2. client API key 明文存储 + 明文点查(与 admin key 的摘要做法不一致)

- 现状:建表 `0001_initial.sql:12` 是 `key text not null unique` ——**明文**;鉴权 `keys/store.rs:55` 是 `select id from client_api_keys where key = $1 and enabled`,拿请求里的明文 key 去 PG 做等值点查。对照之下,admin API key 是 `hash_credential`(SHA-256,`infra/identity.rs:86`)存摘要、`settings/service.rs:117` 用 `ct_eq` 比对;admin 密码是 argon2。**同一套系统里,client key 是唯一"明文落库"的凭据**。
- 问题:
  1. **at-rest**:DB 泄露 → 所有 client key 直接可用(无需破解)。与 #1 同类,但这里连"高熵不可爆破"的兜底都用不上,因为拿到的就是明文。
  2. **一致性**:admin 侧已经证明团队知道"存摘要 + 恒时比较"的正确姿势,client 侧却没套用,属明显遗漏而非设计取舍。
  3. **管理端回显**:`api/admin/keys_routes.rs` 的 `ClientApiKeyData` 会把完整 `key` 序列化返回(list/get/create 都带),虽是 admin 鉴权后才可见,但意味着"任何能进管理台的人可随时导出全部明文 key"。
- 关于"恒时比较"(任务问题 2 的直接回答):verify 路径是 `starts_with("sk_")` 预筛 + PG btree 等值点查(`keys/service.rs:49-56`),既**不是**应用层朴素 `==`(不存在经典的逐字节 Rust 比较时序 oracle),也**不是**显式恒时比较,而是把比较委托给了 Postgres 索引。加上 key 是 256-bit 随机(`identity.rs:63-68`),**时序侧信道在这里实际不可利用**(网络抖动远大于 btree 比较差异,且猜前缀无意义)。所以真正的问题是"明文 at-rest",而非"没恒时比较"——不必为时序问题制造紧迫感。
- 建议:与 admin key 对齐——生成时 SHA-256 摘要入库(高熵密钥用 SHA-256 足够,不需要 argon2),鉴权改为按摘要点查;保留 `prefix` 列供管理台展示与定位;创建接口"仅此一次返回明文"(前端已有"完整 Key 仅显示一次"文案,后端配合即可)。

## P1 — 信任边界与供应链

### 3. 转发头无条件信任 → 登录失败锁定可绕过、审计可伪造

- 现状:`api/middleware/request_id.rs:130-138` 的 `auto_forwarded_client_ip` 依次取 `CF-Connecting-IP` → `X-Real-IP` → `X-Forwarded-For` 首个公网 IP,**没有可信代理(trusted proxy)白名单**,不校验直连 peer 是否是已知反代。也就是说这些头**可被任意客户端伪造**。解析出的 IP 一路用于:admin 登录锁定的 key(`auth_routes.rs:51` → `admin_login_source` → `SessionService::login` 的 `source`)以及审计(usage/ops 记录的 client_ip)。
- 问题:
  1. **爆破防线被绕过(最实际)**:登录锁定按 `source`(=解析出的 IP)计数,5 次/15 分钟锁 15 分钟(`auth/service.rs:17-19,157-166`)。攻击者只要**每次请求换一个 `CF-Connecting-IP` 头**,每次都是"新来源",锁定永远不触发——唯一的在线爆破防护形同虚设。这条**匿名可直接利用**,不需要任何前置条件。
  2. **审计可伪造**:usage/ops 里的 client_ip 可被随意伪造,事后取证与限流溯源不可信。
- 建议:
  1. 引入**可信代理配置**:仅当直连 peer(`ConnectInfo<SocketAddr>`)属于已配置的反代网段时,才采信 `CF-Connecting-IP`/`X-Real-IP`/`XFF`;否则一律用 socket peer IP。这是修复根因。
  2. 锁定逻辑**增加与 IP 无关的维度**:如按用户名(当前恒为默认 admin)或全局失败计数兜底,使"换 IP"无法把计数打散。
- 说明:锁定状态是进程内 `HashMap`(`service.rs:28,51`),重启即清零、多实例不共享;当前单二进制部署可接受,若未来多实例需挪到 Redis。

### 4. 自更新无签名校验(供应链)

- 现状:自更新会下载 GitHub Release 资产并替换正在运行的二进制。已有防护:HTTPS-only + host 白名单(`update/download.rs:125-152`,仅 github.com / objects.githubusercontent.com)、500MB 大小上限(`service.rs:34,477`)、SHA-256 checksum 校验(`download.rs:85-123`)、临时目录隔离 + `canonicalize`、rollback 支持。但 **checksum 来自同一个 Release 里的 `checksums.txt`**(`service.rs:480-485`),校验的是"下载内容 == 该 Release 自己声明的哈希",**没有任何针对发布者的密码学签名**(cosign/minisign/GPG)。
- 问题:信任锚点退化为"GitHub 仓库本身 + TLS"。若 `CPR_UPDATE_REPOSITORY` 指向的仓库被攻破、维护者 token 泄露、或 Release 被替换,则 checksum 会跟着被一起改,校验必然通过——攻击者由此获得"替换运行中进程二进制"的能力(RCE 级)。更新虽需 admin 鉴权 + `build_type == release`(`service.rs:390-401`),但这只挡住外部触发,挡不住"上游包本身被投毒"。
- 建议:引入**detached 签名验证**——用 minisign/cosign 对产物签名,公钥**编译进二进制**(不可通过配置替换),下载后先验签再落地;并在部署文档明确"仓库完整性 == 二进制完整性"。属设计取舍类,按 P1/中等优先级推进即可。
- 记一笔做得好的:传输层卫生(HTTPS、host 白名单、size cap、checksum、rollback、temp 隔离)已经比"裸 curl | sh"强很多。

## P2 — 加固项,视精力而定

### 5. 缺少安全响应头(HSTS / X-Content-Type-Options / X-Frame-Options / CSP)

- 现状:全仓 grep 不到任何 `Strict-Transport-Security` / `X-Content-Type-Options` / `X-Frame-Options` / `Content-Security-Policy` / `SetResponseHeader`(router 组装见 `api/router.rs:82-90`,只有 trace 与 request_id 两层)。管理台 SPA 与 admin API 同源提供。
- 问题:admin UI 无 `X-Frame-Options`/CSP → 有点击劫持面;无 HSTS → 有协议降级面(会话 cookie 已是 `Secure`,风险被部分抵消)。
- 建议:加一个轻量 `SetResponseHeaderLayer`:`X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Strict-Transport-Security`(仅在确认整站 HTTPS 后)、以及一条最小 CSP。成本很低。

### 6. client 侧 `/v1/responses` 关闭了请求体大小限制

- 现状:`api/client/router.rs:26` 对整个 client 路由 `.layer(DefaultBodyLimit::disable())`,即公网 `/v1/responses` 等端点**请求体无上限**。admin 路由则沿用 axum 默认限制。
- 问题:若请求体在转发前被缓冲(按 memory 记录的"typed rebuild"路径,body 会被解析重建),则单请求可撑爆内存,构成 DoS 向量。需确认缓冲行为(**待确认**:核对 `dispatch` 侧是否 streaming 透传还是先 `bytes()`/`to_vec()` 全量读入;验证方法——`rg -n "to_bytes|body::to_bytes|bytes\(\)|Vec<u8>" src/api/client src/dispatch`)。
- 建议:即便需要支持大 payload,也应设一个"够大但有限"的上限(如 32/64MB),而非 `disable()`。

### 7. AdminError 把内部错误串直接回给客户端

- 现状:`AdminError::internal(...)`(`api/admin/response.rs:127`)以及若干 `error.to_string()`(如 `keys_routes.rs:81` 的 `client_key_error` internal 分支、`settings_persist_failed`)会把底层错误消息塞进响应 body 的 `message` 字段。
- 问题:轻度信息泄露——可能回显存储层/依赖的实现细节。均为 admin 鉴权后接口,未发现直接回显 secret,故定 P2。
- 建议:对外统一返回通用文案(如 "internal error"),底层 detail 只进日志(与 `telemetry/ops` 分工一致)。

### 8. 默认管理员密码在首启后仍留在 `config.yaml`

- 现状:`admin.default_password` 首启用于播种管理员(`bootstrap/services.rs:492-512`),启动时强校验 ≥12 位且非弱口令(`config.rs:310-318`,弱口令表 `config.rs:5-14`)。校验做得好,但该密码之后**一直明文留在 config.yaml**。
- 建议:文档提示"首启成功后可清空/轮换 `default_password`",避免长期明文留存;或改为只接受哈希/一次性播种标记。

## 做得好的(记录一下,免得反复纠结)

- **admin 密码 argon2**:`infra/identity.rs:38-51`,`hash_admin_password`/`verify_admin_password` 走 argon2 + 随机 salt,姿势正确。
- **admin API key 存摘要 + 恒时比较**:`identity.rs:86` SHA-256 摘要落库,`settings/service.rs:108-118` 用 `subtle::ConstantTimeEq::ct_eq` 比对,且空值短路——教科书做法。
- **会话 token 强度与 Redis 侧防护**:`generate_admin_session_token` 是 256-bit CSPRNG(`identity.rs:79-83`,`rand::rng()` = ThreadRng);Redis 里 key 用 `hash_credential(session_id)` 摘要化(`auth/store.rs:135`),**Redis dump 也拿不到可用的 session id**;TTL 走 `EX`(`store.rs:105-113`),logout/delete 可吊销。会话生命周期整体没有短板。
- **会话 cookie 属性齐全**:`Secure; HttpOnly; SameSite=Lax`(`api/admin/auth_routes.rs:24`),配合无 CORS 天然防跨站。
- **登录锁定机制本身存在**:5 次/15 分钟(`auth/service.rs:17-19`)——防线在,只是被 #3 的 IP 伪造绕过;修好信任边界后即有效。
- **管理端接口全量鉴权**:抽查 system/accounts/keys/settings/usage/ops/dashboard 全部处理器都带 `_auth: AdminAuth` 提取器(`api/admin/session.rs:15-25` 统一入口),没发现漏挂鉴权的管理端点。
- **OAuth 账号导入走 PKCE + state**:`fleet/manage/oauth.rs:42-48` 随机 32/64 字节 state/verifier + S256 challenge,`exchange` 校验 `session.state == state`(`oauth.rs:83-86`),session 有 TTL。
- **SQL 全参数化**:grep `format!("…select/insert/update/where…")` 零命中,所有查询用 `sqlx::query(...).bind(...)`,无字符串拼接注入面。
- **容器非 root**:`deploy/Dockerfile:54,73,83` 以 `cpr`(uid 10001, nologin)运行,产物 `--chown` 到该用户。
- **config 不进日志**:`AppConfig` 虽 `derive(Debug)` 但全仓未发现打印/`{:?}` 记录,secrets 不会因日志泄露。
- **`.runtime/config.yaml` 已 gitignore**:`git check-ignore` 确认被忽略,示例配置 `deploy/config.example.yaml` 不含真实凭据。

## 核实过的误报

- **"CORS 配置过宽"**:实际**完全没有 CORS 层**(grep 零命中)。对 cookie 鉴权的 admin API 而言,"不允许任何跨源" + `SameSite=Lax` 是**安全默认**,不是缺陷。不用加 CORS,除非将来要做跨域前端。
- **"client API key 验证有逐字节比较时序 oracle"**:如 P0-#2 所述,比较委托给 Postgres 索引点查,且 key 为 256-bit 高熵,时序侧信道实际不可利用。真问题是明文 at-rest,不是时序。
- **"account 导出接口泄露 token"**:`fleet/manage/export.rs` 确实以明文 JSON 导出 token,但这是 admin 鉴权后的**预期导出功能**,属"admin 被攻破即等同 token 外泄"的固有信任假设,非越权泄露。

## 建议动手顺序

1. **P1 第 3 条(IP 信任边界)先做**——匿名可直接利用、危害是"唯一爆破防线失效",且改动集中在 `request_id.rs` 一处 + 锁定加一个非 IP 维度,投入产出最高。
2. **P0 第 2 条(client key 摘要化)**——有 admin key 的现成范式可抄,改 `keys/store.rs` 生成/查询 + 建一个数据迁移(旧明文 key 转摘要),同时收敛管理端明文回显。
3. **P0 第 1 条(上游 token 加密)**——改动面比 #2 大(加解密边界 + 密钥管理 + 迁移),但资产价值最高;建议应用层 AEAD,密钥走环境变量/KMS。
4. **P1 第 4 条(自更新签名)**——供应链加固,需引入签名产物与内嵌公钥,配合发布流程改造,单独排期。
5. **P2 第 5、6、7、8 条**——安全响应头、body limit、错误脱敏、默认密码文档化,按精力顺手清理(第 6 条先确认缓冲行为)。
