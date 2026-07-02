# Deploy

Docker 部署入口集中在本目录。

## Compose

首次部署先复制样例配置：

```bash
cp deploy/config.example.yaml deploy/config.yaml
```

`deploy/config.example.yaml` 是 Docker 部署模板，默认将数据写入容器内 `/app/data`、日志写入 `/app/logs`，由 Compose 挂载到宿主机 `deploy/data` 和 `deploy/logs`。

构建并启动：

```bash
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml up -d
```

默认挂载：

- `deploy/config.yaml` -> `/app/config.yaml`
- `deploy/data` -> `/app/data`
- `deploy/logs` -> `/app/logs`

可以通过环境变量覆盖：

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
