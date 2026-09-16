#!/usr/bin/env bash
set -Eeuo pipefail
umask 0077

# codex-proxy-rs 一键安装脚本。
# 新安装使用同一个 Release 的 compose.yaml 与 config.example.yaml；
# 已存在 deploy/config.yaml 时保留整套部署文件，不执行版本升级。

REPO="zyycn/codex-proxy-rs"
CPR_RELEASE_TAG="${CPR_RELEASE_TAG:-}"
INSTALL_DIR="${INSTALL_DIR:-$PWD/codex-proxy-rs}"
DEPLOY_DIR="${INSTALL_DIR}/deploy"
RUNTIME_DIR="${INSTALL_DIR}/.runtime"

CONFIG_EXAMPLE="${DEPLOY_DIR}/config.example.yaml"
CONFIG_FILE="${DEPLOY_DIR}/config.yaml"
COMPOSE_FILE="${DEPLOY_DIR}/compose.yaml"

ADMIN_PASSWORD="${ADMIN_PASSWORD:-}"

GREEN='\033[1;32m'
YELLOW='\033[1;33m'
RED='\033[1;31m'
CYAN='\033[1;36m'
NC='\033[0m'

log() {
  printf '%b[+]%b %s\n' "$GREEN" "$NC" "$*"
}

info() {
  printf '%b[*]%b %s\n' "$CYAN" "$NC" "$*"
}

warn() {
  printf '%b[!]%b %s\n' "$YELLOW" "$NC" "$*" >&2
}

die() {
  printf '%b[x]%b %s\n' "$RED" "$NC" "$*" >&2
  exit 1
}

on_error() {
  local code=$?

  printf '\n' >&2
  warn "部署失败，退出码：${code}"
  warn "安装目录已保留：${INSTALL_DIR}"
  if [[ -f "$COMPOSE_FILE" ]]; then
    warn "可执行以下命令查看状态："
    printf '    cd %q && docker compose -f deploy/compose.yaml ps\n' "$INSTALL_DIR" >&2
    printf '    cd %q && docker compose -f deploy/compose.yaml logs --tail=200\n' "$INSTALL_DIR" >&2
  fi
  exit "$code"
}
trap on_error ERR

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "缺少命令：$1"
}

run_root() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    die "该步骤需要 root 权限，但系统没有 sudo；请使用 root 用户运行。"
  fi
}

prepare_install_dir() {
  if ! mkdir -p -- "$INSTALL_DIR" 2>/dev/null; then
    run_root install -d -m 0750 -o "$(id -u)" -g "$(id -g)" -- "$INSTALL_DIR"
  fi
  mkdir -p -- "$DEPLOY_DIR"
}

resolve_release_tag() {
  local release_url

  if [[ -z "$CPR_RELEASE_TAG" ]]; then
    release_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
      "https://github.com/${REPO}/releases/latest")"
    CPR_RELEASE_TAG="${release_url%/}"
    CPR_RELEASE_TAG="${CPR_RELEASE_TAG##*/}"
  fi

  [[ "$CPR_RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] \
    || die "无法确定有效的发布版本：${CPR_RELEASE_TAG:-<空>}"
}

download() {
  local url="$1"
  local output="$2"

  curl \
    --fail \
    --location \
    --silent \
    --show-error \
    --retry 3 \
    --connect-timeout 15 \
    "$url" \
    --output "$output"
}

download_release_files() {
  local base_url

  resolve_release_tag
  base_url="https://github.com/${REPO}/releases/download/${CPR_RELEASE_TAG}"

  log "下载 ${CPR_RELEASE_TAG} compose.yaml"
  download "${base_url}/compose.yaml" "$COMPOSE_FILE"
  log "下载 ${CPR_RELEASE_TAG} config.example.yaml"
  download "${base_url}/config.example.yaml" "$CONFIG_EXAMPLE"

  grep -q '^name: codex-proxy-rs$' "$COMPOSE_FILE" \
    || die "下载到的 compose.yaml 内容异常。"
  grep -q '^schema_version:' "$CONFIG_EXAMPLE" \
    || die "下载到的 config.example.yaml 内容异常。"
}

random_password() {
  openssl rand -hex 24
}

validate_admin_password() {
  local password="$1"
  local normalized

  if (( ${#password} < 12 )); then
    die "ADMIN_PASSWORD 至少需要 12 个字符。"
  fi
  if [[ "$password" == *'$'* ]]; then
    die 'ADMIN_PASSWORD 不能包含 $ 字符。'
  fi
  normalized="$(printf '%s' "$password" | tr '[:upper:]' '[:lower:]')"
  if [[ "$normalized" == "codex-proxy-rs" ]]; then
    die "ADMIN_PASSWORD 不能使用常见弱口令。"
  fi
}

patch_config() {
  local postgres_password="$1"
  local redis_password="$2"
  local admin_password="$3"

  POSTGRES_PASSWORD_VALUE="$postgres_password" \
    REDIS_PASSWORD_VALUE="$redis_password" \
    ADMIN_PASSWORD_VALUE="$admin_password" \
    python3 - "$CONFIG_EXAMPLE" "$CONFIG_FILE" <<'PY'
import os
import re
import sys
from pathlib import Path

source = Path(sys.argv[1])
destination = Path(sys.argv[2])
content = source.read_text(encoding="utf-8")


def yaml_single_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


replacements = (
    (r"(?m)^(\s*password:\s*&postgres_password\s*)''\s*$", os.environ["POSTGRES_PASSWORD_VALUE"]),
    (r"(?m)^(\s*password:\s*&redis_password\s*)''\s*$", os.environ["REDIS_PASSWORD_VALUE"]),
    (r"(?m)^(\s*default_password:\s*)''\s*$", os.environ["ADMIN_PASSWORD_VALUE"]),
)
for pattern, value in replacements:
    content, count = re.subn(
        pattern,
        lambda match, replacement=value: match.group(1) + yaml_single_quote(replacement),
        content,
        count=1,
    )
    if count != 1:
        raise SystemExit(
            "config.example.yaml 结构与安装脚本预期不一致，已停止以免生成错误配置。"
        )

destination.write_text(content, encoding="utf-8")
PY
}

service_user_ids() {
  local service="$1"
  local user="$2"
  local uid
  local gid

  uid="$(
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml run --rm --no-deps --entrypoint id "$service" -u "$user"
  )"
  gid="$(
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml run --rm --no-deps --entrypoint id "$service" -g "$user"
  )"

  [[ "$uid" =~ ^[0-9]+$ ]] || die "无法读取 ${service} 容器用户 ${user} 的 UID。"
  [[ "$gid" =~ ^[0-9]+$ ]] || die "无法读取 ${service} 容器用户 ${user} 的 GID。"
  printf '%s:%s\n' "$uid" "$gid"
}

fix_runtime_permissions() {
  local postgres_ids
  local redis_ids
  local app_ids
  local app_gid

  log "读取容器用户 UID/GID"
  postgres_ids="$(service_user_ids postgres postgres)"
  redis_ids="$(service_user_ids redis redis)"
  app_ids="$(service_user_ids codex-proxy-rs cpr)"
  app_gid="${app_ids#*:}"

  info "PostgreSQL postgres = ${postgres_ids}"
  info "Redis redis         = ${redis_ids}"
  info "codex-proxy-rs cpr  = ${app_ids}"

  log "设置运行目录权限"
  run_root chown -R "$postgres_ids" -- "${RUNTIME_DIR}/postgres"
  run_root chmod 0750 -- "${RUNTIME_DIR}/postgres"
  run_root chown -R "$redis_ids" -- "${RUNTIME_DIR}/redis"
  run_root chmod 0750 -- "${RUNTIME_DIR}/redis"

  # 保留安装用户对应用目录的所有权，并向容器实际运行组授予读写权限。
  run_root chown -R "$(id -u):${app_gid}" -- "${RUNTIME_DIR}/data" "${RUNTIME_DIR}/logs"
  run_root chmod 0770 -- "${RUNTIME_DIR}/data" "${RUNTIME_DIR}/logs"
  run_root chown "$(id -u):${app_gid}" -- "$CONFIG_FILE"
  run_root chmod 0640 -- "$CONFIG_FILE"
}

check_health() {
  local status

  status="$(curl \
    --silent \
    --output /dev/null \
    --write-out '%{http_code}' \
    --max-time 10 \
    http://127.0.0.1:8080/healthz || true)"
  [[ "$status" == "204" ]]
}

main() {
  local existing_config=0
  local postgres_password
  local redis_password

  printf '\n'
  printf '%s\n' '============================================='
  printf '%s\n' ' codex-proxy-rs 一键部署'
  printf '%s\n' ' 新安装使用同一 Release 的官方部署文件'
  printf '%s\n' '============================================='
  printf '\n'

  [[ "$(uname -s)" == "Linux" ]] || die "此脚本仅支持 Linux。"
  need_cmd curl
  need_cmd docker
  need_cmd install
  need_cmd openssl
  need_cmd python3

  docker info >/dev/null 2>&1 \
    || die "Docker daemon 不可用；请先安装、启动 Docker，并确认当前用户可以访问。"
  docker compose version >/dev/null 2>&1 \
    || die "未检测到 Docker Compose v2；请确保 'docker compose' 可用。"

  prepare_install_dir
  if [[ -e "$CONFIG_FILE" ]]; then
    existing_config=1
    [[ -f "$COMPOSE_FILE" && -f "$CONFIG_EXAMPLE" ]] \
      || die "已有 config.yaml，但 compose.yaml 或 config.example.yaml 缺失。"
    warn "发现已有配置：${CONFIG_FILE}"
    info "将保留现有 config.yaml、compose.yaml 和 config.example.yaml，不执行版本升级。"
    if [[ -n "$ADMIN_PASSWORD" ]]; then
      warn "已有 config.yaml，因此忽略本次传入的 ADMIN_PASSWORD。"
    fi
  else
    download_release_files
    postgres_password="$(random_password)"
    redis_password="$(random_password)"
    if [[ -z "$ADMIN_PASSWORD" ]]; then
      ADMIN_PASSWORD="$(openssl rand -hex 16)"
    fi
    validate_admin_password "$ADMIN_PASSWORD"
    log "生成 PostgreSQL、Redis 和管理员密码"
    patch_config "$postgres_password" "$redis_password" "$ADMIN_PASSWORD"
  fi

  log "创建运行目录"
  mkdir -p -- \
    "${RUNTIME_DIR}/postgres" \
    "${RUNTIME_DIR}/redis" \
    "${RUNTIME_DIR}/data" \
    "${RUNTIME_DIR}/logs"

  log "验证 Docker Compose 配置"
  (
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml config --quiet
  )

  log "拉取容器镜像"
  (
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml pull
  )

  fix_runtime_permissions

  log "启动服务"
  (
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml up -d --no-build --wait
  )

  log "检查容器状态"
  (
    cd "$INSTALL_DIR"
    docker compose -f deploy/compose.yaml ps
  )

  info "验证健康检查接口"
  if ! check_health; then
    warn "Compose 已启动，但 http://127.0.0.1:8080/healthz 未返回 204。"
    warn "最近日志："
    (
      cd "$INSTALL_DIR"
      docker compose -f deploy/compose.yaml logs --tail=100 codex-proxy-rs
    ) || true
    return 1
  fi

  printf '\n'
  printf '%b%s%b\n' "$GREEN" '=============================================' "$NC"
  printf '%b%s%b\n' "$GREEN" ' 部署完成' "$NC"
  printf '%b%s%b\n' "$GREEN" '=============================================' "$NC"
  printf '\n'
  printf '访问地址：      http://127.0.0.1:8080\n'
  printf '管理员用户名：  admin@cpr.local\n'
  if [[ "$existing_config" -eq 0 ]]; then
    printf '管理员密码：    %s\n' "$ADMIN_PASSWORD"
  else
    printf '管理员密码：    保留现有配置（未修改）\n'
  fi
  printf '安装目录：      %s\n' "$INSTALL_DIR"
  printf '配置文件：      %s\n' "$CONFIG_FILE"
  printf '\n'
  printf '常用命令：\n'
  printf '  cd %q\n' "$INSTALL_DIR"
  printf '  docker compose -f deploy/compose.yaml ps\n'
  printf '  docker compose -f deploy/compose.yaml logs -f codex-proxy-rs\n'
  printf '  docker compose -f deploy/compose.yaml restart codex-proxy-rs\n'
  printf '  docker compose -f deploy/compose.yaml down\n'
  printf '\n'
  if [[ "$existing_config" -eq 0 ]]; then
    printf '%b请立即保存管理员密码。%b\n' "$YELLOW" "$NC"
  fi
  printf '%b%s%b\n' "$YELLOW" \
    'Compose 默认只把 8080 绑定到 127.0.0.1；公网访问请配置 HTTPS 反向代理。' "$NC"
  printf '\n'
}

main "$@"
