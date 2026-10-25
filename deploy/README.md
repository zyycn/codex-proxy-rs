# 部署与运维

首次安装可按 [快速开始](../README.md#快速开始) 操作。
本文补充客户端配置、权限、备份和升级；部署命令从安装目录 `codex-proxy-rs/` 执行，
其中 `deploy/` 存放 Compose 文件和配置，`.runtime/` 存放持久化数据

| 任务 | 入口 |
| --- | --- |
| 安装与启动 | [手动安装](#手动安装) · [启动](#启动) · [公网访问](#公网访问) |
| 接入客户端 | [客户端配置](#客户端配置) · [时区与客户端环境信息](#时区与客户端环境信息) · [登录与生图排查](#登录与生图排查) |
| 日常运维 | [运行设置](#启动后的运行设置) · [小内存优化](#小内存优化) · [持久化与日志](#持久化与备份) · [请求排查](#请求错误排查) · [密码轮换](#密码语义) |
| 更新与恢复 | [镜像升级](#镜像升级与源码构建) · [在线更新](#管理端在线更新) · [备份与恢复](#备份与恢复) · [停止与重启](#停止与重启) |
| 运行源码 | [开发与源码联调](../docs/development.md) |

## 配置归属

配置按职责保存，不都在 YAML 中：

| 位置 | 内容 | 修改方式 |
| --- | --- | --- |
| `deploy/config.yaml` | 监听、数据库/Redis 连接、部署凭据、时区、日志与可选内存分配策略等启动配置 | 从模板创建，已有部署合并修改，不覆盖原文件 |
| `deploy/compose.yaml` | 镜像、容器网络、端口、挂载与资源限制 | 调整 Compose 并重建受影响容器 |
| PostgreSQL | 账号、Key、运行设置、上游身份、插件配置等业务数据 | 管理端或管理 API |
| 浏览器本地 | 主题和界面偏好 | 当前浏览器的主题设置 |

项目不使用 `.env` 配置文件。Compose 环境变量用于容器地址、镜像选择和构建发布；
可选的内存分配策略由 `config.yaml` 的 `services.app-runtime` 桥接到应用容器。
配置加载会忽略未知字段，并在启动控制台提示字段名；不输出对应值。缺少必填字段时会指出缺项并停止启动，
已知字段的类型和取值仍需合法；可选字段省略时使用默认值

`store.pool.observability_timeout_seconds` 默认 120 秒，控制观测查询与统计 Worker 的操作预算，
缓存等待和统计事务的 SQL 时限沿用该配置。修改后重启应用；连接排队仍受
`store.pool.acquire_timeout_seconds` 约束，模型请求时限由 `api.request_timeout_seconds` 单独控制

后端从当前目录向上查找 `deploy/config.yaml`，相对数据、日志和静态资源路径以该文件所在目录解析

Compose 通过环境变量将监听和数据库地址设为容器内地址，并指定镜像中的静态资源目录

### 部署时区

在 `deploy/config.yaml` 设置统一的 IANA 时区，省略时默认 `Asia/Shanghai`：

```yaml
host:
  timezone: 'Asia/Shanghai'
```

支持 `UTC`、`America/New_York`、`Asia/Kathmandu` 等名称；空值、未知名称和错误类型会阻止启动，
错误指出 `host.timezone`，不输出配置值。修改后重启网关生效，二进制与 Compose 使用同一配置

管理端与 Key 用量页的时间文本由后端生成；自然日筛选、日统计、Key 限额、账号预热和备份 Cron 使用该时区。
浏览器、进程 `TZ` 和 PostgreSQL 会话时区不覆盖业务配置；Compose 的基础设施时间基准为 UTC。
上游请求身份中的位置时区仍由对应运行设置或代理配置控制，与部署时区独立

切换时区保留已打开限额窗口的 UTC 边界与用量，到期后按新时区续接；不会因重启立即清零。
备份调度从切换后的当前时刻计算未来执行点，不补跑旧时区漏过的任务，已入队任务保持原状态。
夏令时中不存在的预热或 Cron 时刻跳过，重复时刻只选较早一次。日志日期与保留规则见[日志与保留窗口](#日志与保留窗口)

### 小内存优化

Linux/glibc 部署可选择更积极地归还请求结束后的空闲内存，适合内存紧张且请求后 RSS 长时间偏高的机器。
该选项默认关闭，官方 Linux 镜像支持；更频繁的映射与回收可能增加 CPU 开销，应结合实际负载观察

在 `deploy/config.yaml` 的 `services.app-runtime.environment` 中设置 `GLIBC_TUNABLES`，空字符串表示不启用额外调优：

```yaml
services:
  app-runtime:
    environment:
      GLIBC_TUNABLES: 'glibc.malloc.mmap_threshold=131072:glibc.malloc.trim_threshold=131072'
```

Compose 从 `services.app-runtime` 取得参数；不开启优化时保留该段并将值设为 `''`。
PostgreSQL 和 Redis 的凭据桥接配置与该段并列，无需在 `compose.yaml` 重复填写参数

校验并重建应用容器使配置生效；`docker compose restart` 不会更新容器环境变量：

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml up -d --no-build --no-deps --force-recreate codex-proxy-rs
```

此配置将 glibc 的 mmap 与 trim 阈值固定为 128 KiB，关闭对应的动态阈值调整。
如已有 `GLIBC_TUNABLES`，用冒号合并这两个参数，避免重复同名项；同时检查是否另设了 `MALLOC_MMAP_THRESHOLD_` 或 `MALLOC_TRIM_THRESHOLD_`，统一在一处维护。
参数含义见 [glibc 文档](https://sourceware.org/glibc/manual/latest/html_node/Memory-Allocation-Tunables.html)

关闭时删除这两个参数；没有其他 tunable 时将值恢复为 `''`，然后再次重建应用容器。
`services` 是 Compose 桥接区，直接运行二进制不会读取这一段。Linux/glibc 二进制部署需由启动器在进程启动前设置同一环境变量，例如：

```bash
GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072:glibc.malloc.trim_threshold=131072 ./codex-proxy-rs
```

该变量由 glibc 在进程启动时读取，不支持管理端热更新。
它不设置内存上限，不释放仍被请求或连接持有的缓冲，也不处理容器文件缓存；观察时分别比较进程 RSS、cgroup 的 `anon` 与 `file`

## 部署结构

```mermaid
flowchart LR
  Client[客户端 / 管理浏览器] -->|HTTPS| Proxy[反向代理]
  Proxy -->|HTTP / SSE / WebSocket| App[网关：单副本]
  App --> PG[(PostgreSQL)]
  App --> Redis[(Redis)]
  App --> Upstream[上游服务]
  App -. 数据库备份 .-> Backup[S3 / R2]
```

网关端口默认只绑定本机；数据库和 Redis 不对公网开放。插件以网关同一系统身份运行，不能作为不可信代码沙箱

## 手动安装

本节适用于 Linux amd64/arm64，需准备 Docker Engine、Docker Compose Plugin、curl 和 OpenSSL，
并确保当前用户能访问 Docker、可通过 `sudo` 或 root 设置目录权限。一键安装入口见
[快速开始](../README.md#一键安装)，脚本会自动完成本节准备和服务启动

下载同一正式 Release 的部署文件并设置目录权限：

```bash
mkdir -p codex-proxy-rs/deploy && cd codex-proxy-rs

# 只解析一次最新正式版本，确保两个文件来自同一 Release。
CPR_RELEASE_URL="$(curl -fsSL -o /dev/null -w '%{url_effective}' https://github.com/zyycn/codex-proxy-rs/releases/latest)"
CPR_RELEASE_TAG="${CPR_RELEASE_URL##*/}"
curl -fsSL "https://github.com/zyycn/codex-proxy-rs/releases/download/${CPR_RELEASE_TAG}/compose.yaml" \
  -o deploy/compose.yaml
curl -fsSL "https://github.com/zyycn/codex-proxy-rs/releases/download/${CPR_RELEASE_TAG}/config.example.yaml" \
  -o deploy/config.example.yaml

install -d -m 0750 .runtime/postgres .runtime/redis
sudo install -d -m 0770 -o "$(id -u)" -g 10001 .runtime/data .runtime/logs
sudo install -m 0640 -o "$(id -u)" -g 10001 deploy/config.example.yaml deploy/config.yaml
```

也可将 `CPR_RELEASE_TAG` 设置为指定的发布标签。Release 附带的 `compose.yaml` 默认使用该版本镜像，
配置模板和部署文件均包含在 `checksums.txt` 中；不要混用 `main` 分支模板与已发布镜像

为 PostgreSQL 与 Redis 分别生成一个密码：

```bash
openssl rand -hex 24
openssl rand -hex 24
```

把两个 48 位十六进制结果分别写入 `deploy/config.yaml` 的：

- `store.database.password`
- `store.redis.password`

另行设置 `admin.default_password`：至少 12 个字符，不能使用常见弱口令或包含 `$`

| 会话配置 | 含义 | 配置模板 / 默认值 |
| --- | --- | --- |
| `admin.session_ttl_minutes` | 管理员续期凭据有效期，成功续期后重新计时 | 模板为 10080 分钟（7 天） |
| `admin.session_absolute_ttl_minutes` | 从登录起计算的最长有效期 | 模板及省略时均为 43200 分钟（30 天） |
| `client.session_ttl_minutes` | 密钥身份固定有效期，不随活动续期 | 默认 1440 分钟（1 天） |

管理员访问凭据最多有效 24 小时，由内部策略控制；不活动期限较短时，访问期限缩短到其四分之一，为自动续期留出窗口。
推荐使用模板中的 7 天不活动期限和 30 天最长登录期限。修改配置需重启，不能延长已建立会话的最长有效期。
浏览器会话与续期规则见[认证 API](../docs/api.md#统一登录与会话)，独立于 `/v1/*` 鉴权和限额

PostgreSQL 与 Redis 密码统一接受 16–128 字节，不能全为空白或包含控制字符；仍须满足
各自服务端的密码策略。托管服务可使用服务商允许的高强度密码，默认安装继续生成 48 位随机十六进制密码

Compose 通过 `config.yaml` 的凭据桥接区引用同一密码；三个值都不需要额外导出为环境变量，
数据库和 Redis 密码不能嵌入连接 URL。通过环境变量提供密码时分别使用
`CPR_DATABASE_PASSWORD` 与 `CPR_REDIS_PASSWORD`，原始密码由连接 URL 构造器编码，不要预先做 URL 编码。Compose 凭据桥接建议沿用上面的
随机十六进制密码，避免 `$` 的变量插值语义

Linux 上应用容器以 `10001:10001` 运行。上述命令将应用数据和日志目录设为 `0770`，
配置设为 `0640`，均由当前用户持有、容器组 `10001` 访问。
`config.yaml` 通过 Compose `configs` 只读挂载，普通 Compose 保留宿主机文件的 UID/GID 和 mode

### Redis ACL 用户

连接已配置 ACL 用户的 Redis 时，把用户名写在 `store.redis.url` 中，密码仍单独填写：

```yaml
store:
  redis:
    url: 'redis://u1@redis-host:6379/0'
    password: &redis_password '<u1 的密码>'
```

将这两项合并到已有配置，保留其他配置和密码锚点；Redis 服务端需事先创建并授权该用户。
用户名中的 `@`、`:` 等特殊字符需要 URL 百分号编码。未填写用户名时使用 Redis 的 `default` 用户

环境变量 `CPR_REDIS_URL` 优先于 `store.redis.url`。默认 `deploy/compose.yaml` 已将它设为
`redis://redis:6379/`，使用 ACL 用户时还需把应用服务的该环境变量改为
`redis://u1@redis-host:6379/0`，其中主机名必须能从应用容器访问；只改 `config.yaml` 不会生效。
默认 Compose Redis 服务使用密码认证，不会自动创建 ACL 用户

## 启动

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build --wait
docker compose -f deploy/compose.yaml ps
```

健康检查：

```bash
curl -i http://127.0.0.1:8080/healthz
```

`204 No Content` 表示应用、PostgreSQL、Redis 和后台任务的健康检查通过，不代表每个上游账号都可用

`503 Service Unavailable` 的失败组件与原因保存在运维错误记录中，组件为 `health`，关联标识以
`probe:` 或 `worker:` 开头；探针超时也会指明组件。连续相同故障去重，恢复后再次失败重新记录，
应用日志记录故障与恢复摘要。健康响应保持空正文，详情沿用有界观测队列，队列或存储故障时可能缺失

HTTP 在数据库迁移和初始化完成后才监听。迁移期间启动日志每 15 秒报告已等待时间，
包含执行迁移和等待迁移锁的时间，不代表完成比例；不要因暂时无法访问而反复重启。
镜像和 Compose 默认提供 5 分钟健康检查启动宽限期，探测成功后立即转为健康，无需等满宽限期。
历史数据较多时应提前延长 `healthcheck.start_period` 和部署工具的等待超时；只修改 Compose
文件不会改变已创建容器的检查配置，需要在维护窗口重新创建容器

不要把未脱敏的 `docker compose config` 或 `docker inspect` 输出上传到工单；它们会包含
PostgreSQL/Redis 启动密码。日常校验使用 `config --quiet`

### 启动后的运行设置

在管理端修改以下设置，保存后对新请求生效，重启不会覆盖已保存的选择：

| 设置 | 入口与边界 |
| --- | --- |
| 上游客户端身份 | “系统设置 → 上游配置 → 客户端身份”；Key 可按 Provider 覆盖，支持自动或固定版本 |
| 全局请求位置 | “系统设置 → 上游配置 → 请求位置覆盖”；默认关闭，不从 YAML 导入 |
| 代理位置 | “代理管理”；优先于全局位置，全局关闭时仍可独立生效 |

位置覆盖只影响 OpenAI 请求中受支持的位置与时区信息，不改变上下文日期、真实出口 IP、epoch 时间戳或系统时区。
`openai.residency` 和 `openai.account_routing_override` 是独立于客户端身份的启动配置，
分别指定 `x-openai-internal-codex-residency` 和 `x-openai-account-routing-override` 请求头。
两项均为可选字符串，填写值原样发送，不校验地域或路由枚举；省略或设为 `null` 时删除对应的下游请求头且不补发。
示例配置均填写 `us`，修改后重启应用生效。身份与位置字段见 [运行设置 API](../docs/api.md#8-运行设置)
后台账号操作使用 Provider 自己的官方画像；发布资料刷新不改写用户选择或 `config.yaml`

### 批量维护代理

在“代理管理 → 新增代理”切换到“批量”，每行粘贴一个完整代理 URL、`host:port` 或 `host:port:用户名:密码`，最多 100 条。
完整 URL 使用各自协议，简写使用表单选择的协议；IPv6 主机使用方括号，简写中的密码可包含冒号。
名称按前缀和输入顺序生成，请求位置继承全局设置。勾选“添加后测试”时，只测试本次成功添加的记录。
添加和测试结果合并展示，便于区分保存失败和连接测试失败

列表支持跨页选中，勾选代理后显示“批量测试”按钮，点击直接测试选中项。
测试同时检测连通性和出口地区；测试前固定实际名单，同时执行两条，结果更新到列表。
批量添加中的停止操作只阻止后续提交，已开始的请求仍会完成。
离开代理页面停止后续提交，批量进度不在刷新后保留，已保存代理和测试结果仍可在列表查询

格式错误、重复或配置冲突不覆盖已有代理，也不阻止其他条目处理。
“修改未添加项”只带回明确未添加的条目；连接测试失败的代理已经保存，可在列表中重新测试。
请求中断或结果无法确认时暂停后续提交，添加结果待确认的条目可能已保存，应先刷新列表核对，不能直接重复添加

### 插件命令行

使用与网关相同的工作目录和 `deploy/config.yaml`，读取制品已接受且当前已启用的插件实例：

```bash
codex-proxy-rs plugin --help
codex-proxy-rs plugin '实例ID' '命令' --help
codex-proxy-rs plugin '实例ID' '命令' --参数 '值'
```

将示例中的实例 ID、命令、参数名和值替换为插件实际提供的内容

不带子命令或使用 `serve` 时启动网关；插件命令不监听 HTTP、不启动后台任务，也不恢复或清理正在运行的
网关准入状态。顶层 `--help/--version` 不读取配置。插件帮助只读数据库，不执行迁移、登录或账号保存，
因此需要先由正常部署完成数据库初始化。宿主诊断保留在配置的文件日志中，stdout/stderr 专用于命令结果

参数支持 `--名称=值` 或 `--名称 值`；布尔参数可直接写 `--名称`，关闭时写 `--名称=false`。
duration 使用 `500ms`、`1m30s` 等带单位形式。参数可能进入 shell 历史和系统进程列表，不能因为插件声明
了敏感参数就认为命令行是安全的密钥输入通道；真实凭据优先使用对应插件提供的受控登录或导入入口

成功命令原样返回插件退出码与输出。命令受总期限约束，`Ctrl-C` 或终止信号会取消调用并回收插件进程。
若命令产生账号，只有成功结果才按顺序通过 Admin 保存；调用仍检查期限、实例版本和当前资源事实，
模型执行 Key 由具体调用选择，不在实例中预绑定。
部分保存后失败或取消不会自动重放，先核对账号和审计记录，再决定是否重新执行

独立仓库 `codex-proxy-plugins` 的 `examples/workbench` 提供命令行、请求处理与管理页面示例；模型相关操作会产生真实上游用量

## 公网访问

Compose 默认只绑定 `127.0.0.1`。从其他设备访问时，在应用前配置反向代理，
不要把 PostgreSQL 或 Redis 暴露到公网

同源登录页的管理员与 API Key 登录都支持 HTTP 和 HTTPS 登录。反向代理应原样保留浏览器的 `Origin`，
不要清除它或改写 Cookie 的 `Secure` 属性；HTTPS 反代可以使用 HTTP 回源。
HTTP 传输不加密，公网部署仍建议使用 HTTPS。
会话 Cookie 合同见 [管理接口鉴权](../docs/api.md#管理接口)

反向代理需要保留 `Authorization`，支持 `/v1/responses` 的 WebSocket Upgrade，
并关闭 SSE 响应缓冲。读取超时应覆盖长时间生成任务。
客户端使用部署地址下的 `/v1`，协议与部署一致，不要使用前端开发服务的 `5173/dev/v1`。
当前应用只支持单副本，不能通过复制容器扩容

Responses 流式请求完成准入后，每 15 秒无输出会发送一次 SSE 注释保活，覆盖首个业务事件前的等待，
并设置 `X-Accel-Buffering: no` 和 `Cache-Control: no-cache, no-transform`。
反向代理仍需允许这些响应头生效；准入前的等待需要足够的读取超时。
心跳仅覆盖客户端到 CPR 的下游链路，不会延长账号出站 HTTP/SOCKS 代理的读取期限。
提前发送心跳后的状态码与错误行为见 [Responses 交付合同](../docs/api.md#容量拒绝与重试)

网关默认不限制模型请求的总执行时长；OpenAI 上游流默认有 300 秒空闲超时。
`api.request_timeout_seconds` 默认 `null`，只控制 HTTP 路由返回响应前的等待，不是流式正文或 WebSocket 每轮执行的总时限。
排队、插件显式执行期限与客户端断开的边界见 [请求期限](../docs/api.md#请求期限)

OpenAI 上游池化 WebSocket 默认每 25 秒发送一次 Ping，发出后允许等待 30 秒；
收到 Pong 或其他入站帧即解除本次心跳截止，持续无响应则以 `pong_timeout` 关闭连接。
此策略也覆盖正在生成的请求，与等待下一条上游消息的 `stream_idle_timeout_ms` 分别计时

每条已保存代理可在管理端配置“上游 WS 连接上限”，默认留空、不限制连接数。
预算限制单个 CPR 进程使用该代理的 OpenAI Responses 上游 WS 总数，关联账号共享，包含建连中、在途、空闲及非池化连接；直连不受影响。
上限应按代理实际容量、其他流量与 CPR 实例数设置。例如 32 槽代理可配置为 24，为 HTTP 等流量留出 8 槽，但这不是统一默认值。
不同代理 URL（含认证）分别计数；不同 URL 若共用服务商额度，需自行分配预算，各 CPR 实例之间也不协调容量。
修改上限对后续请求生效，已存在的连接继续计数；调低上限不会强制中断在途或绑定下游 WS 的连接。
预算仅覆盖宿主原生 OpenAI Responses 连接，插件自建连接及其他 HTTP/WS 流量仍需预留容量

有容量时继续复用，连接总寿命仍由 `max_age_ms` 控制，默认 55 分钟，不设置固定空闲回收时间。
满额时优先关闭同代理下最久未使用的普通空闲 WS；在途、建连中和绑定下游 WS 的连接不参与容量淘汰。
没有可回收连接时等待容量，受原请求期限、客户端取消和连接池关闭约束；无显式请求期限时不会额外设置排队超时。
被回收连接上的精确续接沿用既有客户端完整回放合同，可能多一次恢复往返

若 Codex 在压缩或长时间生成时出现 `error decoding response body`，这表示
客户端读取 HTTP 响应体失败。请结合网关请求诊断中的 `upstream.read.failed`、
`downstream.body.closed` 和反向代理日志判断断开位置，不能仅凭此消息认定是 JSON 格式错误。
SSE 注释保活用于防止传输链路空闲断开，不会重置 Codex 等待完整 SSE 事件的超时；
若报错为 `idle timeout waiting for SSE`，再检查客户端的 `stream_idle_timeout_ms`

## 客户端配置

在管理端创建客户端密钥，打开「使用密钥」，按操作系统复制 `config.toml`。
已有配置先备份，合并后完全退出并重启 Codex。代理密钥由 Provider 配置提供，
不需要另写 `auth.json`，已有官方登录文件可以保留

管理端生成的 CC Switch 一键导入包含连接信息和当前 Key 的日／周额度查询，默认每 30 分钟刷新。
查询地址和凭据随导入生成，在 CC Switch 中修改 Provider 的地址或 Key 后，需重新导入以同步用量查询。
导入不包含下方模板的远程压缩、原生生图和 WebSocket 设置。需要这些设置时，使用下方配置直连 CPR，
避免 CC Switch 切换或接管后重写 Provider 配置

Linux/macOS 默认目录为 `~/.codex/`，Windows 为 `%USERPROFILE%\.codex\`；
设置过 `CODEX_HOME` 时以该目录为准。Provider 设置应写入用户配置，不要只写到项目目录。
配置层级见 [官方配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)

### config.toml

以下示例与当前管理端模板的配置项一致。替换地址和密钥，模型可改成该密钥有权限使用的模型：

```toml
model_provider = "OpenAI"
model = "gpt-5.6-terra"
review_model = "gpt-5.6-terra"
model_reasoning_effort = "max"
service_tier = "default"

[model_providers.OpenAI]
name = "OpenAI"
base_url = "http://127.0.0.1:8080/v1"
wire_api = "responses"
supports_websockets = false
requires_openai_auth = false
# 填写代理密钥，真实账号由服务端管理。
experimental_bearer_token = "<client-api-key>"

[model_providers.OpenAI.capabilities]
# Codex 0.162.0+ 显式启用 V2 远程压缩，不依赖 Provider 名称识别
remote_compaction = "v2"

[model_providers.OpenAI.http_headers]
# 供客户端识别服务端托管认证，本身不是密钥。
X-OpenAI-Actor-Authorization = "proxy-managed"

[features]
image_generation = true
goals = true
```

`OpenAI` 是自定义 Provider ID，大小写要与 `model_provider` 一致。合并配置时修改已有表，
不要重复添加 `[features]` 或 Provider 表。更换模型时也要检查其支持的推理强度。
密钥以明文保存，文件仅供本人读取，不要提交到 Git。接入代理不需要扩大命令沙箱的联网或文件权限

V2 远程压缩通过 Responses 接口发送 `compaction_trigger`，网关和上游需支持该协议。
若 Provider ID 改为 `custom`，相应能力配置也要改到 `[model_providers.custom.capabilities]`

需要指定完整模型目录时，在账号的模型列表中导出所选 Codex 模型，并在 `config.toml` 顶层设置
`model_catalog_json = "/absolute/path/to/cpr-model-catalog.json"`。导出文件不含账号凭据；
它是一次目录快照，调整选择或上游模型能力变化后需重新导出

### 生图和 WebSocket

模板启用原生生图。Codex 可在任务需要图片时调用 `image_gen.imagegen`，
再通过代理的 Images 接口生成或编辑图片；也可以在需求中明确要求生成并使用图片。
需要支持该能力的客户端、支持图片输入的对话模型，以及有生图权限和额度的 Codex 账号。
代理不会增加上游权限，xAI 账号不能承接这些 Images 请求

生图不要求开启 WebSocket。要启用客户端 WebSocket，把当前 Provider 的
`supports_websockets` 改为 `true`，并检查反向代理是否允许 Upgrade

客户端到代理、代理到上游是两段独立连接。客户端关闭 WebSocket 后，
服务端仍可能用 WebSocket 访问上游；客户端开关不控制服务端连接池和 HTTP 回退策略。
Codex OAuth 账号的上游传输方式默认 WS；固定为 SSE 的账号不承接必须依赖 WS 的预热及连接内续接。
仅使用这类账号时，客户端保持 `supports_websockets = false`，避免先尝试 WS 再回退

### 时区与客户端环境信息

要覆盖模型请求中的位置和时区，在管理端启用全局自定义位置，或为账号关联的代理设置位置；
代理位置优先。`host.timezone` 控制部署日历和管理端时间展示，不代替请求位置设置。
覆盖字段与环境消息识别条件见 [代理位置合同](../docs/api.md#独立代理管理--managed-proxies)

Codex 回答仍显示本机时区时，先核对请求是否使用了预期账号和代理，再区分时间上下文与工具结果：

- 使用新会话询问环境上下文中的时区，避免已有工具输出或历史回答影响判断
- 检查 `environment_context` 和 Desktop 的 `codex_apps_client_time_context` 是否满足[时间上下文识别条件](../docs/api.md#独立代理管理--managed-proxies)；显式普通文本分类或非法 XML 不会被覆盖
- 若模型调用 `date` 等工具查询本机，工具结果仍反映真实系统时区，覆盖不会修改客户端操作系统

网关的请求头画像和位置覆盖不提供完整匿名化。正文与工作区 metadata 的保留范围见
[正文兼容](../docs/api.md#正文兼容)，xAI 请求不应用 OpenAI 的请求位置覆盖

Codex 客户端还有独立的出站请求。analytics 使用 `chatgpt_base_url`，可以包含真实系统类型、
系统版本和架构；模型 Provider 的 `base_url` 不改变该地址。官方发行配置默认启用 Statsig 指标，目标为
`https://ab.chatgpt.com/otlp/v1/metrics`，不由网关上游画像控制

需要关闭这两类遥测时，将以下字段合并到客户端 `config.toml`，避免重复创建已有表，完全退出并重启 Codex：

```toml
[analytics]
enabled = false

[otel]
metrics_exporter = "none"
```

这两个开关不清理模型请求中的工作目录、Git metadata、用户内容或工具结果。

### 登录与生图排查

- 仍提示登录：确认实际读取的用户配置目录、选中的 Provider 和客户端版本，再完全退出重启。
  新 Provider 使用 `requires_openai_auth = false`，不依赖本地 ChatGPT 登录
- 没有生图工具：核对 Codex 实际读取的 Provider 配置，确认 Actor 标记、`requires_openai_auth = false` 和图片输入能力，
  不只检查导出模板；CC Switch 导入不包含生图设置。客户端缓存的本地 Free 账号也可能影响工具展示。
  先备份并区分本地真实账号文件与代理密钥文件，不要直接删除全部登录状态
- 已调用生图但失败：查看服务端账号的凭据、权限、额度和请求错误，不能只凭文本对话成功判断
- `401`：检查代理密钥是否正确、是否启用；Actor 标记不能代替密钥
- `426`：检查客户端版本及反向代理是否保留版本头，低于管理员设置的最低版本时需要更新客户端。
  手机远程控制的版本识别范围见 [版本门禁规则](../docs/api.md#1-鉴权与公共约定)
- 地址包含 `5173/dev/v1`：这是开发代理地址，依赖 Vite 服务。日常使用改成后端或 HTTPS 地址；
  验证 WebSocket 时直接连接后端，当前 Vite 代理未显式开启 WebSocket 转发

其他客户端使用 Responses API、`/v1` Base URL 和代理密钥即可，不需要 Actor 标记。
路由与请求格式见 [API 参考](../docs/api.md#3-openai-数据面与模型目录)

## 停止与重启

管理端重启、SIGTERM 和 SIGINT 使用同一关闭策略：停止接收新连接，不等待现有 HTTP、SSE、
WebSocket 会话完成，进程退出时强制断开剩余连接。已入队的观测写入与后台清理保留
`host.worker_shutdown_timeout_seconds`（默认 30 秒）的收尾预算；客户端在新进程就绪后重新连接

管理端重启受理后，到进程退出期间，更新、回滚和再次重启均被拒绝；可继续查询更新状态

Compose 的 `stop_grace_period` 为 75 秒，覆盖默认 30 秒后台收尾及进程清理余量。
调大后台关闭预算时，也必须同步预留容器停止宽限期，否则 Docker 会在宽限期结束时 SIGKILL

## 本地开发

源码环境、前后端启动与组件库联调见 [开发与源码联调](../docs/development.md)

本机开发可使用 Compose 提供 PostgreSQL 和 Redis，集成测试另用[专用测试库](../backend/migrations/README.md#本地测试库)

## 持久化与备份

### 数据目录

Compose 使用以下绑定目录：

| 目录 | 内容 |
| --- | --- |
| `.runtime/data` | OpenAI 会话锚点密钥、更新状态、临时更新目录与备份暂存区 |
| `.runtime/logs` | 应用文件日志、按需启用的 OAuth 恢复记录与请求转储 |
| `.runtime/postgres` | PostgreSQL 持久化数据 |
| `.runtime/redis` | Redis AOF |

普通 `docker compose down` 不会删除这些目录。删除 `.runtime` 会永久清除本地状态

应用容器以 `10001:10001` 运行，`.runtime/data` 与 `.runtime/logs` 的顶层目录使用 `0770` 权限和 `10001` 属组。
安装脚本对这两个目录仅设置顶层所有权与权限，内部文件及私有目录保留应用创建时的所有权与权限

PostgreSQL 是账号、Client Key、运行设置、请求记录与审计的权威存储；账号 credential 按
Provider schema 以明文 JSON 保存于 PostgreSQL。Redis 只保存可重建、可过期的协调状态，例如
会话亲和、lease、cooldown、OAuth pending flow 与套餐模型目录 cache

备份应覆盖哪些目录、如何生成一致性数据库归档，见 [备份与恢复](#备份与恢复)

### 日志与保留窗口

文件日志以时间完整性为清理依据，**没有文件数量淘汰上限**：

| 文件集 | 保留配置 | 默认完整窗口 |
| --- | --- | --- |
| 普通日志、OAuth 恢复日志 | `host.logging.file.retention_days` | 前 7 个完整自然日及当天 |
| 全量请求/响应报文 | `host.logging.request_dump_retention_days` | 最近 24 小时 |

文件名统一为 `codex-proxy-rs-<类别>.YYYY-MM-DD[.N].log[.gz]`，类别分别为
`application`、`oauth-recovery`、`request-dump`。专用 tracing target 为 `oauth_recovery` 和
`request_dump`；普通日志保留各 Rust 模块的 target，便于按模块过滤。
程序只管理上述规范名称的日志，其他命名的文件由运维手动清理。

日志时间戳采用部署时区并携带数字偏移。普通日志与 OAuth 恢复日志按该时区的日期轮转和整组保留：例如 9 月 8 日配置 1 天，会保留 9 月 7 日全天及 9 月 8 日的所有分片，
到 9 月 9 日才允许清理 9 月 7 日，避免留下半天日志。夏令时日期允许为 23 或 25 小时。切换时区不重命名历史文件；
按文件名日期与部署时区中的实际修改日期取较近值，旧日期分片近期被写入时整组延后删除。

请求转储保留期支持正小数，按每天 24 小时换算，例如 `0.5` 为 12 小时、`0.25` 为 6 小时。
已关闭分片以最后修改时间判断过期，整片删除，不受时区或夏令时影响。
时间轮转间隔取一小时与保留期中的较小值，在下一条记录写入时触发。
实际保存时间可能长于配置值，取决于分片的最后写入时间和下次清理时机

`max_file_size_mb` 仅决定分片大小（默认 20 MiB），不决定保存时长，单条大记录不会被截断。
关闭的分片压缩为 `.log.gz`；成功压缩、同步并发布归档后才删除原文件，保留原修改时间。
清理发生在启动和轮转时；空闲期间过期文件可能暂时多保留。请求转储重启时新建分片，避免反复追加延后旧记录的过期点。
检索时须同时读取 `.log` 与 `.log.gz`

报文开关为 `host.logging.request_dump`，默认关闭；开启时原始报文按块写入独立文件，
缓冲拒收或写入失败可能造成缺块，需结合 `file_logging` 健康状态判断完整性。
数据库请求诊断仍是有界摘要，不能用摘要事件数代替全量报文完整性

OAuth 恢复开关为 `host.logging.oauth_recovery`，默认关闭，与普通文件日志开关分别控制：

- 开启后，OpenAI 每次成功取得 AT 和可用 RT 时，在资料补全及数据库写入前写入独立的 `oauth-recovery` 文件集
- 该通道不受普通日志级别过滤，关闭时也不会改写到普通日志或 stdout；接收端不等待后台写入

恢复记录和请求转储含原始凭据，`.runtime/logs` 须按敏感数据控制访问和备份

每个文件输出使用非阻塞队列，普通额度为 4096 条 / 64 MiB，WARN / ERROR 另有 512 条 / 8 MiB 预留，
额度包含正在写入的记录。后台批量缓冲写文件，正常退出会排空已接收的队列并同步。
持续拥堵、超大记录或输出关闭导致的拒收会损失整条记录，所有接口均不会为日志等待；
`file_logging` 健康探针在拒收或写入/同步失败后报告 `Unhealthy`，
本次进程内恢复写入也不会清除已有缺口；压缩或清理失败报告 `Degraded`。
断电、强杀或磁盘故障可能造成日志丢失；需要恢复已删除文件时，应使用独立备份。
容量不足时不会提前删除保留窗口内日志，必须根据完整日期的压缩后实际用量规划空间，并监控磁盘余量和
健康探针。Docker stdout 的独立轮转不承担应用文件日志的完整保留承诺

完整运行时、Provider、revision 与恢复边界见 [架构文档](../docs/architecture.md)

## 请求错误排查

1. 先记录故障时间和时区、网关版本/提交、客户端名称与版本，以及 HTTP/SSE/WebSocket 传输。
   区分“上游返回”“网关实际响应”和“客户端终端展示”，不要只凭终端的统一文案推断根因
2. 收集响应中的 `x-gateway-request-id`、`x-request-id` / `x-oai-request-id`；配置了
   `api.request_id_header` 时也记录该入口头。WebSocket 合成错误的关联头位于本条错误的 `headers`。
   在管理端错误列表按 ID 和时间搜索；检查平台条件并主动刷新，翻页不会推进查询时间。
   只有入口 ID 时，改用入口日志和时间定位
3. 打开错误详情，核对上游/客户端状态、发送状态、attempt、失败阶段及后续恢复关联。
   下载诊断包作为反馈材料，先检查 `availability.gaps`、`exportTruncated` 以及尝试和时间线的完整性。
   该包包含经服务端脱敏的已存上游错误与时间线，分享前仍应检查业务内容及内部信息
4. 需要更细上下文时，在 `codex-proxy-rs-application.*.log` 及 `.log.gz` 中按网关/上游 ID
   和时间检索，结合 `attempt.started`、`attempt.failed`、`request.finished` 与 transport 阶段判断。
   **开启 `host.logging.file.enabled` 时，stdout 仅保留 `gateway_startup` 通道**；
   `docker logs` 看不到业务错误不代表没有错误。关闭普通文件日志且开启 `host.logging.stdout`
   时，普通日志才按级别输出到 stdout；专用 dump/OAuth 恢复通道不会混入
5. 查不到请求记录时，先用模型执行 ID 在“全部平台”下搜索：已进入执行会话的建流前失败也会记录，
   但其 attempt 为零，尚未确认的 Provider/账号/传输为空，不会命中具体平台或账号筛选。
   再检查观测队列丢弃/写入失败告警及 `file_logging` 健康状态。鉴权、解析、路由和准入等入口拒绝
   仍不保证进入错误列表；应结合入口状态与日志，不能据此认定请求未发生。异步投影有延迟，
   刷新后仍需核对缺口，而不是反复重放可能已发送的请求
6. 仅在上述信息不足且能够控制访问范围时临时开启 `host.logging.request_dump` 复现一次。
   它会记录完整凭据与用户正文，应限定访问、摘取最小片段并人工脱敏；复现后关闭开关，
   按配置的保留窗口及组织的数据处理要求管理已生成文件。不要为普通请求排查开启 OAuth 恢复记录，
   也不要直接上传整个日志目录或完整转储

请求问题反馈使用 [接口问题反馈表单](../.github/ISSUE_TEMPLATE/api-bug-report.yml)；错误诊断与查询合同见
[API 文档](../docs/api.md#10-dashboard用量与错误)

### 插件取文与 DNS

插件受管 HTTP 由实际运行网关的主机或容器解析 DNS，再通过直连或账号代理访问目标。插件拥有完整网络访问能力，宿主不按插件身份限制私网、回环或保留地址

域名解析失败时先核对网关运行环境的 DNS。Clash / Mihomo 的 Fake-IP 地址需要相应代理接管；无法连通时可让目标域名返回真实地址，并检查所选代理。代理失败不会自动回退直连

排障区分域名解析失败、超时、连接失败和响应读取失败，避免在日志或截图中输出真实请求头、正文及凭据

## 密码语义

- `admin.default_password` 只在首次创建管理员时使用
- 已有管理员在「系统设置 → 安全与访问 → 管理员密码」修改登录密码，需要验证当前密码；修改后所有管理员会话失效，使用新密码重新登录
- PostgreSQL 官方镜像只在空数据目录初始化时使用 `database.password`
- Redis 在每次容器创建时使用 `redis.password`

已有 PostgreSQL 数据目录后，直接修改 `database.password` 不会修改数据库用户密码，只会导致
应用无法连接。轮换时必须先在 PostgreSQL 中修改用户密码，再同步更新 `config.yaml`。Redis
密码变更后需要用新配置重新创建 Redis 和应用容器，不需要删除 Redis 数据目录。
安排维护窗口，避免应用和 Redis 在过渡期间使用不同密码

## 镜像升级与源码构建

每个 Release 独立提供 `config.example.yaml`、默认镜像固定到该版本的 `compose.yaml` 和校验和；
各平台归档也包含 `deploy/config.example.yaml`。配置模板来自构建该版本的同一提交。
使用二进制归档手动部署时，将模板中的 `api.asset_directory` 改为 `../web/dist`，指向归档内的静态资源。
在线更新默认使用同一目录；如显式设置 `host.system_update.web_dist_dir`，应确保它指向实际提供页面的目录。
升级时先阅读目标版本说明，下载同一 Release 的部署附件，对比模板并合并必要配置，保留已有凭据
和 Compose 自定义项。不要用模板覆盖 `config.yaml`，也不要从 `main` 下载模板搭配旧镜像

更新部署文件后，从安装目录拉取目标版本镜像并重建应用容器：

```bash
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build --wait codex-proxy-rs
```

源码构建需要克隆源码仓库并准备配置与数据目录，构建过程自动准备前端依赖，无需初始化子模块或准备额外 build context。以下命令从仓库根目录执行：

```bash
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build --wait
```

升级后通过管理端版本接口或容器 image digest 确认运行实例的版本和 revision

构建元数据通过一次性进程环境传入：

```bash
CPR_VERSION="$(ruby -ryaml -e 'puts YAML.load_file("release/version.yaml").fetch("version").delete_prefix("v")')" \
CPR_GIT_SHA="$(git rev-parse HEAD)" \
CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
docker compose -f deploy/compose.yaml build codex-proxy-rs
```

### 版本命名与升级规则

管理端「系统更新」可临时选择通道检查更新，不保存选择，不会自动下载或安装。
每次打开弹窗按当前运行版本匹配通道：正式版为 Stable，预发行按版本后缀匹配。
升级并重启后按新运行版本匹配；侧栏更新提示始终使用当前运行通道。预发行编号 `N` 从 1 开始递增

| 通道 | 命名示例 | 接收的版本 |
| --- | --- | --- |
| Stable | `X.Y.Z` | 正式版 |
| RC | `X.Y.Z-rc.N` | RC、正式版 |
| Beta | `X.Y.Z-beta.N` | Beta、RC、正式版 |
| Alpha | `X.Y.Z-alpha.N` | Alpha、Beta、RC、正式版 |
| Exp | `X.Y.Z-exp.N` | 同一轮实验中编号更高的 exp，仅实验实例可选 |

普通通道在同一大版本内持续接收更高版本，包括后续小版本和补丁版本的预发行。
切换通道不会降级，Stable 候选也必须高于当前运行版本。
下载、安装或待重启期间不能切换通道

同一 `X.Y.Z-exp.N` 系列只用于同一轮实验，不能混用不同实验分支；变更基线即进入另一实验线，需手动迁移。
exp 不进入正式版、alpha、beta 或 rc，普通实例也不能选择 Exp。
从 exp 切换正式版时需先备份，在目标版本的独立数据库中迁移业务数据，不能直接复用或整库还原实验数据库

版本大小遵循 [SemVer](https://semver.org/)，构建元数据 `+...` 不参与比较。未知预发行标识不提供在线更新。
更新器先过滤不允许的目标，再选择版本最高的候选；其他通道或更高大版本领先，不影响当前发行线的更新。
只有当前官方构建满足在线更新条件且存在允许安装的新版本时，才返回“有可用更新”。否则不显示更新标记、
其他通道的版本及发布说明，手动检查显示“当前没有可用更新”。检查失败单独报告，不能当作没有更新

alpha、beta、rc、exp 在 GitHub 标记为 Pre-release，不覆盖 GitHub Latest 或镜像 `latest`。
升级前核对目标版本的数据库要求；可连续升级的发行线与约束见[迁移冻结规则](../backend/migrations/README.md#冻结规则)

### 管理端在线更新

在线更新要求官方构建且起始版本不低于 **v3.18.1**，版本选择遵循上述规则；源码等非官方构建返回不支持原因，不提供可用更新。
不满足在线更新条件时，使用完整发行包或镜像[手动部署](#镜像升级与源码构建)。
点击「下载并更新」后，操作日志显示下载、校验与安装过程，完成后显示「待重启」及待生效版本，重启后才使用新程序。
「当前版本」始终表示正在运行的版本，「通道最新」表示所选通道的检查结果

手动更换镜像或程序后，更新器会核对安装文件并校准历史状态，不以旧成功记录推断需要重启。
运行期间检测到外部文件变动或文件缺失时会报告异常，需要核对完整发行包并重启服务后再检查。
回滚入口只接受与记录指纹一致的完整备份；缺少指纹，或备份被替换、丢失时，不提供在线回滚，
但不会删除备份文件。此时需要按人工部署流程恢复已经核实的发行包

直接运行二进制时，若未设置 `CPR_ENABLE_SELF_RESTART=true`，管理端自重启默认关闭。
在现有 `deploy/config.yaml` 的 `host` 下合并以下配置：

```yaml
host:
  system_update:
    deployment_mode: binary
    self_restart_enabled: true
    executable_path: '/opt/codex-proxy-rs/codex-proxy-rs'
```

`executable_path` 为可选项：未指定时，服务在启动时解析并固定当前程序路径；
显式配置时，将示例改为实际程序的绝对路径。`self_restart_enabled` 控制管理端的「立即重启」操作；
修改配置后需手动重启当前服务一次才会生效。
以上三个字段也可分别通过 `CPR_DEPLOYMENT_MODE`、`CPR_ENABLE_SELF_RESTART` 和 `CPR_UPDATE_EXE_PATH` 设置，
YAML 中显式配置的值优先于对应环境变量

Compose 提供以下在线更新运行参数：

- `CPR_UPDATE_REPOSITORY`：只接受 `owner/repository`；默认 `zyycn/codex-proxy-rs`
- `CPR_GITHUB_API_BASE`：正式环境必须为 `https://api.github.com/repos`
- `CPR_UPDATE_EXE_PATH`、`CPR_WEB_DIST_DIR`：分别指向容器内二进制和前端静态目录；
  `CPR_WEB_DIST_DIR` 同时供页面服务与更新器使用，相对路径以 `deploy/config.yaml` 所在目录为基准
- 更新临时目录、状态文件和锁文件默认由 `host.runtime_data_dir` 派生；
  `CPR_UPDATE_TEMP_DIR`、`CPR_UPDATE_STATE_FILE`、`CPR_UPDATE_LOCK_FILE` 仅用于显式覆盖
- `CPR_ENABLE_SELF_RESTART=true`：更新或回滚完成后允许管理端请求重启；Docker 进程退出后由
  Compose 的 `restart: unless-stopped` 拉起新进程

管理端自重启遵循统一的[停止与重启](#停止与重启)策略，客户端需在新进程完成迁移与初始化后重新连接

Release 必须提供当前 OS/架构的 `codex-proxy-rs_<version>_<os>_<arch>.tar.gz` 与
`checksums.txt`。服务会在替换前再次查询远端最新版本，校验下载 host、声明大小、SHA-256 和
归档路径；二进制或静态资源任一替换失败时恢复旧文件。成功后的旧二进制和旧静态目录分别保留为
`*.backup`，管理端 rollback 会交换当前文件与这份备份。更新状态和跨进程锁可在以下位置排查：

```text
.runtime/data/update-state.json
.runtime/data/update.lock
.runtime/data/update-tmp/
```

### 插件兼容与发行目录

教学示例在 `codex-proxy-plugins` 独立构建和发布，通过[插件管理](../docs/plugins.md)安装。
宿主发行物不内置插件，包含用于兼容检查的封口清单

| 文件或目录 | 用途 |
| --- | --- |
| `plugin-release-manifest.json` | 固定宿主版本、提交、平台与支持的插件合同；插件列表当前为空 |
| 可执行文件同目录的 `plugins/official` | 随受信宿主发行物部署的只读目录，与二进制、Web 资源成套更新或恢复 |

- 下载更新时校验发行身份，插件合同不兼容不阻止安装。点击重启时检查已安装目标版本，列出不兼容插件的名称和原因；确认后保留启用配置并重启，逐个尝试启动插件，版本警告持续显示
- 回滚同样在重启前提示插件兼容风险，不因版本不匹配批量停用插件。更新或回滚期间的并发配置变更会阻止切换，文件替换失败或取消时恢复整组文件
- 非空官方清单中的包经身份、平台和摘要校验后幂等导入；导入不确认信任、不创建配置、不启用，也不删除旧包。管理员确认信任后才执行默认配置流程；重复摘要保留首次安装出处
- `sealed` 是构建封口标记，不是密码学签名。普通上传、URL 或 GitHub 安装不能获得 `builtin` 身份，独立教学示例也不例外

部署须保留完整的二进制、Web 资源与官方插件目录；不要手动修改制品元数据或迁移 checksum 绕过校验。
macOS arm64 提供构建产物；部署前需验证目标平台的插件运行与上游调用，不能只以打包成功判断可用

## 备份与恢复

管理端 S3/R2 提供业务备份；需要完整请求历史时，另做 PostgreSQL 整库备份或基础备份与 WAL 归档

不要在 PostgreSQL 写入期间直接复制数据目录作为一致性备份

| 需要保留的内容 | 备份方式 |
| --- | --- |
| 账号、Key、设置、插件、已用额度与用量汇总 | 管理端 S3/R2 业务备份 |
| 完整请求、诊断和审计历史 | 独立整库备份，或停库后备份 `.runtime/postgres` |
| 部署配置与凭据 | 单独备份 `deploy/config.yaml` 及 Compose 自定义配置 |
| 应用日志与已启用的 OAuth 恢复记录 | 单独备份 `.runtime/logs`，不包含在数据库归档中 |
| 会话锚点与节点运行文件 | 按需备份 `.runtime/data` |
| 短期协调状态 | 按需备份 `.runtime/redis`，数据库冷恢复时使用空 Redis |

OpenAI 主动额度重置卡及消费结果由上游持有，不属于本地备份内容

数据库、配置、插件敏感设置和 OAuth 恢复日志均按凭据保护

### 备份内容与限制

业务归档为 `.sql.gz`，保留完整数据库结构和迁移记录，但不包含请求明细、诊断与审计日志、
逐请求费用去重记录和可重建的统计明细。Key 当前日、周的已用额度独立保留

原生用量按 UTC 小时保留全局请求数、成功/失败数、Token 和实际费用，每小时至多一行；
插件维护的用量贡献按其原维度保留。原生汇总只覆盖备份快照中已发布且尚未被历史清理撤销的统计，
不是永久累计账本，异步投影尚未处理的请求不计入。统计重建期间备份失败，统计就绪后可重试

恢复后的原生历史可用于全局总量与小时趋势，不支持请求详情、按 Key/账号等维度筛选或历史延迟分布；
小时内不完整时间范围无法还原到请求精度。若需这些能力，应使用整库灾备

- 插件包体、固定来源、下载凭据、制品接受事实、实例配置、版本配置快照、敏感配置、功能范围绑定和私有状态都保存在
  PostgreSQL，随数据库一起备份。插件通过宿主回调读写的内置平台账号保存在同库的通用账号表。
  来源使用的出站代理及其认证也须随库恢复，不能只备份 `plugin_*` 表；来源代理与账号代理独立选择，
  不依赖进程代理环境，故障时不会自动回退直连。
  包体单个上限为 32 MiB，需要计入数据库及备份容量；解压运行目录和 Redis 协调状态是可重建数据，
  不作为插件安装或制品接受事实。恢复时保留制品接受状态、实例配置、账号引用和状态代次的一致性；插件降级仍须
  通过状态兼容校验，不通过手工替换缓存目录回滚
- 官方运行镜像内置与 Compose 数据库服务版本一致的 `pg_dump`，无需额外安装。
  自行替换 PostgreSQL 版本时，应同步核对备份工具版本
- 备份暂存目录为 `host.runtime_data_dir/backup-staging`；Compose 默认对应
  `/app/.runtime/data/backup-staging`，由 `.runtime/data` 卷持久化，权限 `0700`（仅 `cpr`
  用户可读写）。归档流式压缩，只暂存一份压缩文件；部署卷至少预留一个最大业务归档的空间
- S3/R2 存储、Cron 计划、保留策略与备份记录都保存在 PostgreSQL，备份记录在删除成功后移除。
  人工创建和删除操作写入管理审计，自动保留清理不生成管理员操作审计
- 手工备份的 `expiresInDays` 在创建时生成独立 `expires_at`；计划备份按当前 `retentionDays` 生成
  `expires_at`，并同时受 `retentionCount` 清理规则约束。到期只进入删除流程，不构成在线恢复点

### 人工恢复数据库

业务归档是 gzip 压缩的 SQL，使用与数据库版本匹配的 `psql` 恢复到空目标库：

```bash
set -o pipefail
gzip -dc backup.sql.gz | psql -X --single-transaction --set=ON_ERROR_STOP=1 --password \
  --dbname='postgresql://restore_user@127.0.0.1:5432/restore_db'
```

示例中的账号、空目标库和归档路径需按实际情况替换，密码由命令交互输入。
先核对文件 SHA-256 与备份记录一致，并执行 `gzip -t backup.sql.gz` 校验归档完整性

恢复期间与恢复后的边界见 [架构文档](../docs/architecture.md#11-生命周期安全与恢复)。
当前没有在线恢复 API，也没有供部署者直接启用的“维护模式”开关：

1. 先备份现有数据库，并停止应用；在独立的空数据库中恢复归档，检查数据和迁移版本
2. 应用保持停止，离线核对备份设置与记录。禁用快照中的旧计划，处理非终态记录
   （`queued/dumping/uploading/deleting`）、旧调度游标和到期清理条件，再决定哪些记录保留
3. 使用空 Redis 和空插件解包缓存做一次冷启动；逐一核对已启用实例的包摘要/平台、制品接受状态、配置与绑定、账号引用、
   私有状态 schema/代次，并执行实际插件操作。缺少或损坏必需包体、引用不完整或状态不兼容时，恢复不能
   记为成功，也不能用相同插件 ID 的其他包替代
4. 确认不会误执行旧任务或删除恢复前的远端对象后，再接入业务流量；重新测试 S3 连接并设置计划

只关闭计划备份不会停止 Worker 的任务恢复和到期删除。无法确认这些记录的影响时，
不要把恢复后的数据库直接接入运行中的应用。具体处理应根据目标库数据制定，不提供清空生产记录的通用命令
