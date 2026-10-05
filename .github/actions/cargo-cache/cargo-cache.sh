#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 restore|snapshot BUILDER DIRECTORY amd64|arm64" >&2
  exit 2
fi
operation="$1"
builder="$2"
directory="$(realpath -m "$3")"
architecture="$4"
case "$operation" in restore|snapshot) ;; *) exit 2 ;; esac
case "$architecture" in amd64|arm64) ;; *) exit 2 ;; esac
dockerfile="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/Dockerfile"

usable() {
  local root="$1" path crates=false libraries=false
  # 目录或 buildstamp 存在不代表编译发生过，必须同时有下载包与目标依赖。
  for path in "$root"/registry/cache/*/*.crate; do
    if [ -f "$path" ] && [ -s "$path" ]; then crates=true; break; fi
  done
  for path in "$root"/target/"$architecture"/release/deps/*.rlib; do
    if [ -f "$path" ] && [ -s "$path" ]; then libraries=true; break; fi
  done
  [ "$crates" = true ] && [ "$libraries" = true ]
}

command=(docker buildx build --builder "$builder" --file "$dockerfile" --progress plain)
# mount 的内容不参与层指纹，用时间戳重跑复制；--no-cache 会重置 cache mount。
command+=(--build-arg "CACHE_STAMP=$(date +%s%N)" --target "$operation")
valid=false
if [ "$operation" = restore ]; then
  if usable "$directory"; then
    mkdir -p "$directory"/{registry,git,target}
    "${command[@]}" --output type=cacheonly "$directory"
    valid=true
  else
    echo "No usable Cargo snapshot; cached image layers may still be available"
  fi
else
  # 导出只发送空上下文，新快照独立校验后替换旧目录，避免残留文件造成误判。
  mkdir -p "$(dirname "$directory")"
  temporary="$(mktemp -d "${directory}.XXXXXX")"
  trap 'rm -rf "$temporary"' EXIT
  mkdir "$temporary/context"
  "${command[@]}" --output "type=local,dest=$temporary/snapshot" "$temporary/context"
  if usable "$temporary/snapshot"; then
    rm -rf "$directory"
    mv "$temporary/snapshot" "$directory"
    valid=true
  fi
fi

echo "Cargo snapshot for $architecture: usable=$valid"
if [ "$valid" = true ]; then du -sh "$directory"; fi
if [ -n "${GITHUB_OUTPUT:-}" ]; then printf 'usable=%s\n' "$valid" >> "$GITHUB_OUTPUT"; fi
