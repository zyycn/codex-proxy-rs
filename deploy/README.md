# 部署

本目录只有两个配置入口：

- `config.yaml`：应用行为与真实凭据，由 `config.example.yaml` 复制得到并被 Git 忽略。
- `compose.yaml`：镜像、容器网络、端口、目录映射、健康检查和资源限制。

项目不使用 `.env` 配置文件。Compose 中的少量环境变量只描述容器内部拓扑与发布链路装配，不是用户配置入口。

## 准备

从仓库根目录执行：

```bash
mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
```

为 PostgreSQL 与 Redis 分别生成一个密码：

```bash
openssl rand -hex 24
```

把两个 48 位十六进制结果分别写入 `deploy/config.yaml` 的：

- `store.database.password`
- `store.redis.password`

另行设置 `admin.default_password`。它至少需要 12 个字符，不能是常见弱口令，也不能包含 `$`。

PostgreSQL 与 Redis 密码必须是 48 位十六进制字符。Compose 通过 `config.yaml` 的凭据桥接区
引用同一密码；三个值都不需要额外导出为环境变量，数据库和 Redis 密码也不能嵌入连接 URL。

Linux 上应用容器以 `10001:10001` 运行，需要允许该组写入应用数据和日志目录：

```bash
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
```

`config.yaml` 通过 Compose `configs` 只读挂载。普通 Compose 对本地文件保留宿主机的
UID/GID 和 mode，因此配置由当前用户持有，并只向容器组 `10001` 开放读取权限。

模板中的 `openai` / `xai` 只保留请求画像启动基线。OpenAI 的上游地址、WebSocket 池、额度刷新
与 OAuth 设置，以及 xAI 的 OAuth、额度和模型目录策略，均由各自 Provider 使用代码内默认值管理；
模板不重复列出这些默认项。运行后，OpenAI CLI/Desktop 与 xAI CLI 的官方发布检查只更新进程内
版本字段，不回写 `config.yaml`；检查失败时继续使用上一份有效画像。

## 启动

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build
docker compose -f deploy/compose.yaml ps
```

健康检查：

```bash
curl -i http://127.0.0.1:8080/healthz
```

`204 No Content` 表示应用、PostgreSQL 和 Redis 均可用。

不要把未脱敏的 `docker compose config` 或 `docker inspect` 输出上传到工单；它们会包含
PostgreSQL/Redis 启动密码。日常校验使用 `config --quiet`。

## 优雅关停

收到停止信号后，应用先停止接收新连接并 drain 存量连接；整个 drain 共享一个从停止信号
起算的绝对截止点（`host.drain_timeout_seconds`，默认 30 秒），逾期放弃等待，存量连接随
进程退出终止。drain 结束后才关停后台 worker，预算为
`host.worker_shutdown_timeout_seconds`（默认 30 秒），两段预算按最坏情况串联。

Compose 的 `stop_grace_period` 为 75 秒，覆盖默认 30 秒 HTTP drain、30 秒 worker 收尾和额外调度
余量。若调大任一应用超时，也必须把 `stop_grace_period` 调到大于两段超时之和；否则 Docker 会在
宽限期结束时 SIGKILL。

## 本地开发

本地 PostgreSQL 和 Redis 可继续由 Compose 启动：

```bash
docker compose -f deploy/compose.yaml up -d postgres redis
cd backend
cargo run -p codex-proxy-rs
```

后端会从当前目录向上查找 `deploy/config.yaml`。相对数据和日志目录以该文件所在目录解析；
Compose 把监听地址和数据库、Redis 地址固定覆盖为容器内部服务名，并把前端静态目录指向
容器内构建产物。PG/Redis 集成测试所需的 `CPR_TEST_DATABASE_URL` / `CPR_TEST_REDIS_URL`
约定见 [迁移文档](../backend/migrations/README.md)。

## 持久化与备份

Compose 使用以下绑定目录：

- `.runtime/data` → OpenAI 会话锚点密钥、更新状态与临时更新目录
- `.runtime/logs` → 应用文件日志
- `.runtime/postgres` → PostgreSQL
- `.runtime/redis` → Redis AOF

普通 `docker compose down` 不会删除这些目录。删除 `.runtime` 会永久清除本地状态。

PostgreSQL 是账号、Client Key、运行设置、请求记录与审计的权威存储；账号 credential 当前按
Provider schema 以明文 JSON 保存于 PostgreSQL。Redis 只保存可重建、可过期的协调状态，例如
会话亲和、lease、cooldown、OAuth pending flow 与套餐模型目录 cache。

备份必须包含 `.runtime/postgres`。若希望保留短期 Redis 状态、OpenAI 会话亲和和更新状态，
同时备份 `.runtime/redis` 与 `.runtime/data`；后两者丢失不会造成账号 credential 丢失，但会使
缓存和会话锚点重新建立。

完整运行时、Provider、revision 与恢复边界见 [架构文档](../docs/architecture.md)。

## 密码语义

- `admin.default_password` 只在首次创建管理员时使用。
- PostgreSQL 官方镜像只在空数据目录初始化时使用 `database.password`。
- Redis 在每次容器创建时使用 `redis.password`。

已有 PostgreSQL 数据目录后，直接修改 `database.password` 不会修改数据库用户密码，只会导致
应用无法连接。轮换时必须先在 PostgreSQL 中修改用户密码，再同步更新 `config.yaml`。Redis
密码变更后需要使用新密码重建或重新配置 Redis 数据目录。

## 镜像升级与源码构建

> [!WARNING]
> 以下命令只适用于同一大版本内的升级，不支持跨大版本在线升级。跨大版本请使用全新的
> `.runtime/` 数据目录重新部署，并重新导入或重新授权 Provider 账号与客户端 Key。

```bash
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

拉取发布镜像：

```bash
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build
```

仓库维护者发布新版本时必须从干净且已同步上游的分支运行：

```bash
release/publish <version>
```

该脚本负责更新 `release/version.yaml`、创建版本提交和带注释 tag，并原子推送分支与 tag。
它不会登录任何服务器，也不会拉取镜像或调用管理端在线更新。

源码提交、仓库发版和运行实例升级是三种独立状态：本地 commit 不等于 Release，Release/tag 和镜像
已生成也不等于实例已升级。判断某项修复是否在线前，应先通过管理端版本接口或容器 image digest
确认运行实例的实际 revision；实例只有在执行上面的 Compose pull/up，或成功完成管理端在线更新后
才会改变。

构建元数据仍可作为一次性进程环境传入，不需要 `.env` 文件：

```bash
CPR_VERSION="$(ruby -ryaml -e 'puts YAML.load_file("release/version.yaml").fetch("version").delete_prefix("v")')" \
CPR_GIT_SHA="$(git rev-parse HEAD)" \
CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
docker compose -f deploy/compose.yaml build codex-proxy-rs
```

### 管理端在线更新

Compose 已显式装配正式发布构建所需的运行参数：

- `CPR_UPDATE_REPOSITORY`：只接受 `owner/repository`；默认 `zyycn/codex-proxy-rs`。
- `CPR_GITHUB_API_BASE`：正式环境必须为 `https://api.github.com/repos`。
- `CPR_UPDATE_CHANNEL`：`stable` 会拒绝 prerelease。
- `CPR_UPDATE_EXE_PATH`、`CPR_WEB_DIST_DIR`：分别指向容器内二进制和前端静态目录。
- `CPR_UPDATE_TEMP_DIR`、`CPR_UPDATE_STATE_FILE`、`CPR_UPDATE_LOCK_FILE`：全部位于持久化的
  `.runtime/data`。
- `CPR_ENABLE_SELF_RESTART=true`：更新或回滚完成后允许管理端请求重启；Docker 进程退出后由
  Compose 的 `restart: unless-stopped` 拉起新进程。

Release 必须提供当前 OS/架构的 `codex-proxy-rs_<version>_<os>_<arch>.tar.gz` 与
`checksums.txt`。服务会在替换前再次查询远端最新版本，校验下载 host、声明大小、SHA-256 和
归档路径；二进制或静态资源任一替换失败时恢复旧文件。成功后的旧二进制和旧静态目录分别保留为
`*.backup`，管理端 rollback 会交换当前文件与这份备份。更新状态和跨进程锁可在以下位置排查：

```text
.runtime/data/update-state.json
.runtime/data/update.lock
.runtime/data/update-tmp/
```
