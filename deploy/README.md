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
cargo run
```

后端会从当前目录向上查找 `deploy/config.yaml`。相对数据和日志目录以该文件所在目录解析；
Compose 只把监听地址和数据库、Redis 地址固定覆盖为容器内部服务名。

## 持久化

Compose 使用以下绑定目录：

- `.runtime/data` → 应用身份密钥和更新状态
- `.runtime/logs` → 应用文件日志
- `.runtime/postgres` → PostgreSQL
- `.runtime/redis` → Redis AOF

普通 `docker compose down` 不会删除这些目录。删除 `.runtime` 会永久清除本地状态，应在升级或
迁移前备份整个目录。

旧版命名卷不会自动迁移到绑定目录。已有部署必须先备份 PostgreSQL 和 Redis，再启用新版
`compose.yaml`，否则新容器会看到空的 `.runtime/postgres` 与 `.runtime/redis`。

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
