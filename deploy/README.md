# Deploy

Docker 部署入口集中在本目录。

## Compose

首次部署先复制样例配置：

```bash
mkdir -p .runtime/data .runtime/logs
cp deploy/config.example.yaml .runtime/config.yaml
```

`deploy/config.example.yaml` 是 Docker 部署模板。Compose 默认把配置、数据和日志都放在仓库 `.runtime/` 下：

- `.runtime/config.yaml` -> `/app/config.yaml`
- `.runtime/data` -> `/app/data`
- `.runtime/logs` -> `/app/logs`

复制后必须设置 `admin.default_password`，空值或常见弱口令会被后端拒绝启动。
如果前面还有 Nginx、Caddy、Cloudflare Tunnel 等反向代理，建议把后端实际看到的代理 peer IP 或 CIDR 写入 `server.trusted_proxies`。Docker 端口映射经宿主反代访问时，这个 peer 通常是 Compose 网络网关，例如 `172.18.0.1/32`，不是宿主 `127.0.0.1/32`。

`server.trusted_proxies` 非空时，只有这些 peer 的 `CF-Connecting-IP` / `X-Real-IP` / `X-Forwarded-For` 会被采信；为空时进入兼容自动模式，按同一组头解析真实 IP。

构建并启动：

```bash
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml up -d
```

可以通过环境变量覆盖宿主机路径；容器内会通过 `CPR_CONFIG_FILE=/app/config.yaml` 显式读取挂载后的配置：

```bash
CPR_CONFIG_FILE=/path/to/config.yaml \
CPR_DATA_DIR=/path/to/data \
CPR_LOG_DIR=/path/to/logs \
docker compose -f deploy/docker-compose.yml up -d
```

## Build

Docker 构建上下文保持仓库根目录：

```bash
docker build -f deploy/Dockerfile -t codex-proxy-rs:latest .
```

也可以通过 Compose 构建：

```bash
docker compose -f deploy/docker-compose.yml build
```

需要注入版本信息时：

```bash
CPR_VERSION="$(ruby -ryaml -e 'puts YAML.load_file("release/version.yaml").fetch("version").delete_prefix("v")')" \
CPR_GIT_SHA="$(git rev-parse HEAD)" \
CPR_BUILD_TIME="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
docker compose -f deploy/docker-compose.yml build
```
