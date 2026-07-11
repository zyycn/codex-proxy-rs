# 部署

本目录包含 Dockerfile、Compose 文件和 Docker 配置模板。

## Compose

首次部署先复制样例配置：

```bash
mkdir -p .runtime/data .runtime/logs
cp deploy/config.example.yaml .runtime/config.yaml
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
sudo chown "$(id -u):10001" .runtime/config.yaml
chmod 0640 .runtime/config.yaml
cp deploy/.env.example deploy/.env
chmod 0600 deploy/.env
```

`deploy/config.example.yaml` 是 Docker 部署模板。Compose 默认使用仓库根目录的 `.runtime/`：

- `.runtime/config.yaml` -> `/app/config.yaml`
- `.runtime/data` -> `/app/data`
- `.runtime/logs` -> `/app/logs`

应用日志同时写入 `docker logs` 与 `.runtime/logs`。Compose 对应用、PostgreSQL、Redis
统一启用 `json-file` 的 `10m × 5` 轮转；应用文件日志还受配置中的自然日、单文件大小和文件总数限制。

`deploy/.env` 是 Docker 部署的密钥配置文件，已被 Git 忽略且必须保持 mode `0600`。其中
`CPR_ADMIN_DEFAULT_PASSWORD`、`CPR_POSTGRES_PASSWORD`、`CPR_REDIS_PASSWORD` 三项均为必填，
留空时 Compose 会在启动前失败。管理员密码必须至少 12 位且不能是常见弱口令。

Compose 会把 PostgreSQL、Redis 密码的同一个值同时传给服务端和应用；应用使用 URL API 覆盖连接串
密码，并用管理员密码覆盖 `admin.default_password`。因此无需修改 `.runtime/config.yaml` 中的密码，
特殊字符也无需手工编码。本地 Rust 集成测试若复用该 Compose 服务，应使用与 `deploy/.env` 一致的
测试连接串。
编辑密码时保留 `deploy/.env` 中的单引号，使 `#`、`$` 等 Compose 语法字符按字面值传递。

如果前面还有 Nginx、Caddy、Cloudflare Tunnel 等反向代理，确保反代正常传递 `CF-Connecting-IP`、`X-Real-IP` 或 `X-Forwarded-For`。后端会按这组头自动解析真实客户端 IP；没有这些头时回落到直连 peer IP。

构建并启动：

```bash
# 先编辑 deploy/.env，设置全部三项密码。
docker compose --env-file deploy/.env -f deploy/docker-compose.yml build
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

可以通过环境变量覆盖宿主机路径。容器内始终通过 `CPR_CONFIG_FILE=/app/config.yaml` 读取挂载后的配置：

```bash
CPR_CONFIG_FILE=/path/to/config.yaml \
CPR_DATA_DIR=/path/to/data \
CPR_LOG_DIR=/path/to/logs \
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

## 构建

Docker 构建上下文保持仓库根目录：

```bash
docker build -f deploy/Dockerfile -t codex-proxy-rs:latest .
```

也可以用 Compose 构建：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml build
```

需要写入版本元数据时：

```bash
CPR_VERSION="$(ruby -ryaml -e 'puts YAML.load_file("release/version.yaml").fetch("version").delete_prefix("v")')" \
CPR_GIT_SHA="$(git rev-parse HEAD)" \
CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
docker compose --env-file deploy/.env -f deploy/docker-compose.yml build
```
