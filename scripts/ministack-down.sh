#!/bin/sh
# Remove the MiniStack container started by scripts/ministack-up.sh and the
# per-cluster backend containers (ministack-dsql-*) it created. Idempotent.
set -u
name=${MINISTACK_NAME:-micro-dsql-ministack}
docker rm -f "$name" >/dev/null 2>&1 && echo "ministack-down: removed $name" >&2
for c in $(docker ps -aq --filter name=ministack-dsql-); do
    docker rm -f "$c" >/dev/null 2>&1 && echo "ministack-down: removed backend $c" >&2
done
exit 0
