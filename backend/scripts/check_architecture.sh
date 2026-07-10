#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail=0

deny() {
  local description=$1
  local pattern=$2
  shift 2
  if grep -rn --include='*.rs' -E "$pattern" "$@"; then
    echo "ARCH VIOLATION: $description" >&2
    fail=1
  fi
}

domain_paths=(
  src/infra src/upstream src/telemetry src/keys src/auth src/settings src/update
  src/accounts src/models src/dispatch
)

deny "domain -> api/bootstrap" '(^|[^[:alnum:]_])(api|bootstrap)::' "${domain_paths[@]}"
deny "upstream -> domain" '(^|[^[:alnum:]_])(accounts|dispatch|telemetry|keys|auth|settings|update|models)::' src/upstream
deny "axum outside api/bootstrap" '\baxum(::|\b)' "${domain_paths[@]}"
deny "sqlx in api" '\bsqlx(::|\b)' src/api
deny "redis in api" '\bredis::' src/api
deny "legacy module path" 'crate::(admin|proxy|runtime|web|http|config)::' src
deny "Admin-prefixed domain type" '\bAdmin[A-Z][A-Za-z0-9]*\b' "${domain_paths[@]}"
deny "Repository naming" '\b[A-Za-z0-9]*Repository\b' src
deny "persistence repository naming" '\b[A-Za-z0-9_]*repository[A-Za-z0-9_]*\b' \
  src/accounts src/auth src/dispatch src/keys src/models src/settings src/telemetry \
  src/upstream/openai/fingerprint
deny "Arc alias" '\bArc[[:space:]]+as[[:space:]]+' src

storage_command_check() {
  local kind=$1
  local pattern=$2
  local file
  while IFS= read -r file; do
    case "$file" in
      */store.rs | */store/*)
        continue
        ;;
    esac
    if [ "$kind" = sql ]; then
      case "$file" in
        src/infra/database.rs | src/bootstrap/import_sqlite.rs | src/bootstrap/import_sqlite/* | src/telemetry/rebuild.rs | */query.rs)
          continue
          ;;
      esac
    else
      case "$file" in
        src/infra/redis.rs | src/accounts/refresh/lease.rs)
          continue
          ;;
      esac
    fi
    echo "$file"
    echo "ARCH VIOLATION: $kind command outside an allowed storage adapter" >&2
    fail=1
  done < <(grep -rl --include='*.rs' -E "$pattern" src || true)
}

storage_command_check sql 'sqlx::(query|query_as|query_scalar|raw_sql)'
storage_command_check redis 'redis::(cmd|pipe)|[.]query_async[[:space:]]*[(]|use redis::.*AsyncCommands'

banned=$(find src \( -name 'importing.rs' -o -name 'exporting.rs' -o -name 'testing.rs' \
  -o -name 'util.rs' -o -name 'helper.rs' -o -name 'common.rs' -o -name 'misc.rs' \))
if [ -n "$banned" ]; then
  echo "ARCH VIOLATION: banned file names: $banned" >&2
  fail=1
fi

while read -r file; do
  lines=$(wc -l < "$file")
  if [ "$lines" -gt 50 ]; then
    echo "ARCH VIOLATION: $file has $lines lines (mod.rs <= 50)" >&2
    fail=1
  fi
done < <(find src -name mod.rs)

while read -r file; do
  lines=$(wc -l < "$file")
  if [ "$lines" -gt 800 ]; then
    echo "ARCH VIOLATION: $file has $lines lines (> 800)" >&2
    fail=1
  fi
done < <(find src -name '*.rs' ! -name mod.rs)

exit "$fail"
