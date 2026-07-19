# 部署

本目录只有两个配置入口：

- `config.yaml`：应用行为与真实凭据，由 `config.example.yaml` 复制得到并被 Git 忽略。
- `compose.yaml`：镜像、容器网络、端口、目录映射、健康检查和资源限制。

项目不使用 `.env` 配置文件。Compose 中的少量环境变量只描述容器内部拓扑，不是用户配置入口。

## 准备

从仓库根目录执行：

```bash
mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
```

分别执行三次以下命令：

```bash
openssl rand -hex 24
```

把结果写入 `deploy/config.yaml`：

- `x-cpr.database.password`
- `x-cpr.redis.password`
- `x-cpr.admin.default_password`

PostgreSQL 与 Redis 密码必须是 48 位十六进制字符。管理员初始化密码至少 12 位、不能是
常见弱口令且不能包含 `$`。三个密码不会通过环境变量覆盖，也不能嵌入连接 URL。

Linux 上应用容器以 `10001:10001` 运行，需要允许该组写入应用数据和日志目录：

```bash
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
```

`config.yaml` 通过 Compose `configs` 只读挂载。普通 Compose 对本地文件保留宿主机的
UID/GID 和 mode，因此配置由当前用户持有，并只向容器组 `10001` 开放读取权限。

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

## 本地开发

本地 PostgreSQL 和 Redis 可继续由 Compose 启动：

```bash
docker compose -f deploy/compose.yaml up -d postgres redis
cd backend
cargo run -p codex-proxy-rs
```

后端会从当前目录向上查找 `deploy/config.yaml`。相对数据和日志目录以该文件所在目录解析；
Compose 只把监听地址和数据库、Redis 地址固定覆盖为容器内部服务名。

## 持久化

Compose 使用以下绑定目录：

- `.runtime/data` → 应用身份密钥、credential 加密 keyring 和更新状态
- `.runtime/logs` → 应用文件日志
- `.runtime/postgres` → PostgreSQL
- `.runtime/redis` → Redis AOF

普通 `docker compose down` 不会删除这些目录。删除 `.runtime` 会永久清除本地状态。

本架构从全新的 `0001_initial.sql` 起步，不支持把旧版本数据库原地升级到当前结构，也不提供
旧表或旧 Redis 数据的兼容读取。首次启动必须使用空的 PostgreSQL 数据库和空 Redis；需要保留
旧环境时应独立保存其目录，不能让新旧二进制共享同一份数据目录。

## Credential 加密 keyring

首次启动会在 `.runtime/data/credential_keyring` 生成：

- `active_key_id`：新 envelope 唯一使用的 active key ID。
- `envelope_keys/<key-id>.key`：active key 与只读历史解密 key。
- `credential_fingerprint_hmac_key`：稳定 credential 去重域，不能随 envelope key 轮换。
- `resource_pseudonym_hmac_key`：稳定资源伪名域，不能随 envelope key 轮换。

目录权限固定为 `0700`，文件权限固定为 `0600`；符号链接、组/其他用户可读权限、无效 key ID
或重复 key 材料都会让进程拒绝启动。Keyring 不进入 PostgreSQL，必须和 PostgreSQL 备份分开保存；
丢失任何仍被数据库引用的 envelope key 后，对应 credential、Cookie 或 continuation 将无法恢复。

Envelope key 使用“读旧写新”两阶段轮换。多节点必须为每个节点执行相同步骤：

1. 生成相同的新 key ID 和 32-byte key，写入 `envelope_keys/<key-id>.key`，保持当前
   `active_key_id` 不变。
2. 滚动重启全部节点，使每个旧进程都先把新 key 加载为历史解密 key。
3. 在全部节点原子替换 `active_key_id`，再滚动重启。已重启节点只用新 key 写入，尚未重启节点
   仍能用第二步加载的 key 解密新 envelope。

单机 Compose 可按以下方式准备和激活新 key；不要把命令输出或 key 文件内容写入日志：

```bash
new_key_id="ek-$(openssl rand -hex 16)"
openssl rand -hex 32 | sudo sh -c \
  'umask 077; cat > ".runtime/data/credential_keyring/envelope_keys/$1.key"' sh "$new_key_id"
sudo chown 10001:10001 ".runtime/data/credential_keyring/envelope_keys/$new_key_id.key"

# 先重启一次，让当前进程把新 key 作为历史 key 加载。
docker compose -f deploy/compose.yaml restart codex-proxy-rs

sudo env NEW_KEY_ID="$new_key_id" sh -c '
  umask 077
  printf "%s" "$NEW_KEY_ID" > .runtime/data/credential_keyring/active_key_id.next
  chown 10001:10001 .runtime/data/credential_keyring/active_key_id.next
  mv -f .runtime/data/credential_keyring/active_key_id.next \
    .runtime/data/credential_keyring/active_key_id
'
docker compose -f deploy/compose.yaml restart codex-proxy-rs
unset new_key_id
```

历史 key 只有在所有引用计数均为零、备份也满足同一退役策略后才能删除：

```sql
select 'upstream_credentials' as source, count(*) from upstream_credentials where secret_key_id = $1
union all
select 'codex_account_cookies', count(*) from codex_account_cookies where secret_key_id = $1
union all
select 'conversation_items', count(*) from conversation_items where secret_key_id = $1
union all
select 'continuation_bindings', count(*) from continuation_bindings where secret_key_id = $1;
```

删除仍被引用的历史 key 不会降级或尝试其他 key；解密会以 unknown key fail closed。确认零引用后，
从全部节点删除对应 `.key` 文件并滚动重启，才算完成退役。Fingerprint 与 resource pseudonym 两个
HMAC key 不属于此轮换流程，不能用 envelope key 替换。

## 密码语义

- `admin.default_password` 只在首次创建管理员时使用。
- PostgreSQL 官方镜像只在空数据目录初始化时使用 `database.password`。
- Redis 在每次容器创建时使用 `redis.password`。

已有 PostgreSQL 数据目录后，直接修改 `database.password` 不会修改数据库用户密码，只会导致
应用无法连接。轮换时必须先在 PostgreSQL 中修改用户密码，再同步更新 `config.yaml`。

## 构建与升级

```bash
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

拉取发布镜像：

```bash
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build
```

构建元数据仍可作为一次性进程环境传入，不需要 `.env` 文件：

```bash
CPR_VERSION="$(ruby -ryaml -e 'puts YAML.load_file("release/version.yaml").fetch("version").delete_prefix("v")')" \
CPR_GIT_SHA="$(git rev-parse HEAD)" \
CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
docker compose -f deploy/compose.yaml build codex-proxy-rs
```

### 管理端 Release 更新

Compose 已显式装配正式更新所需的运行参数：

- `CPR_UPDATE_REPOSITORY`：只接受 `owner/repository`；默认 `zyycn/codex-proxy-rs`。
- `CPR_GITHUB_API_BASE`：正式环境必须为 `https://api.github.com/repos`。
- `CPR_UPDATE_CHANNEL`：`stable` 会拒绝 prerelease。
- `CPR_UPDATE_EXE_PATH`、`CPR_WEB_DIST_DIR`：分别指向容器内二进制和旧前端静态目录。
- `CPR_UPDATE_TEMP_DIR`、`CPR_UPDATE_STATE_FILE`、`CPR_UPDATE_LOCK_FILE`：全部位于持久化的
  `.runtime/data`。
- `CPR_ENABLE_SELF_RESTART=true`：更新或回滚完成后允许管理端请求重启；Docker 进程退出后由
  Compose 的 `restart: unless-stopped` 拉起新进程。

Release 必须提供当前 OS/架构的 `codex-proxy-rs_<version>_<os>_<arch>.tar.gz` 和
`checksums.txt`。服务会在替换前再次查询远端最新版本，校验下载 host、声明大小、SHA-256 和 tar
路径；二进制或静态资源任一替换失败时恢复旧文件。成功后的旧二进制和旧静态目录分别保留为
`*.backup`，管理端 rollback 会交换当前文件与这份备份。更新状态和跨进程锁可在以下位置排查：

```text
.runtime/data/update-state.json
.runtime/data/update.lock
.runtime/data/update-tmp/
```
