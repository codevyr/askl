#!/bin/sh
set -e

host="${ASKLD_WAIT_FOR_DB_HOST}"
port="${ASKLD_WAIT_FOR_DB_PORT}"

if [ -z "$host" ] && [ -n "$ASKL_DATABASE_URL" ]; then
  host_port=$(printf '%s' "$ASKL_DATABASE_URL" | sed -n 's|.*@\\([^/]*\\).*|\\1|p')
  host=${host_port%%:*}
  port=${host_port#*:}
  if [ "$host" = "$host_port" ]; then
    port=""
  fi
fi

if [ -n "$host" ]; then
  if [ -z "$port" ]; then
    port="5432"
  fi
  i=1
  while [ $i -le 60 ]; do
    if nc -z "$host" "$port" >/dev/null 2>&1; then
      exec /usr/local/bin/askld "$@"
    fi
    echo "waiting for ${host}:${port}..."
    i=$((i + 1))
    sleep 1
  done
  echo "database not ready after 60s" >&2
  exit 1
fi

exec /usr/local/bin/askld "$@"
