#!/bin/sh
# Start the MiniStack DSQL emulator (the T1 reference) and print libpq exports for
# one ACTIVE cluster. Usage:
#
#   eval "$(scripts/ministack-up.sh)"
#   tests/psql/run.sh
#
# stdout carries only `export PG...=...` lines; progress goes to stderr.
#
# MiniStack needs DSQL_STRICT=1 and the Docker socket to run its real data plane
# (one postgres:16-alpine backend per cluster behind an in-process DSQL rule
# proxy). The proxy listens inside the MiniStack container on 25432+n
# (DSQL_BASE_PORT), so those ports are published on the MiniStack container; the
# backend containers are not published.
#
# The control plane is driven with AWS CLI v2 (`aws dsql ...`) and dummy
# credentials. If `aws` is not on PATH (or MINISTACK_AWS_DOCKER=1), the official
# amazon/aws-cli image is run inside MiniStack's network namespace instead.
#
# Environment (all optional):
#   MINISTACK_NAME         container name                (micro-dsql-ministack)
#   MINISTACK_IMAGE        image                         (ministackorg/ministack)
#   MINISTACK_DOCKER_FLAGS extra `docker run` flags, e.g. --privileged
#   MINISTACK_PGHOST       host to print as PGHOST       (host of the reported endpoint)
#   MINISTACK_TIMEOUT      seconds to wait for each step (180)
#   AWS_REGION             region for the control plane  (us-east-1)
#   MINISTACK_AWS_DOCKER   1 = always use the aws-cli container

set -eu

name=${MINISTACK_NAME:-micro-dsql-ministack}
image=${MINISTACK_IMAGE:-ministackorg/ministack}
region=${AWS_REGION:-us-east-1}
timeout=${MINISTACK_TIMEOUT:-180}
cli_image=amazon/aws-cli

log() { echo "ministack-up: $*" >&2; }
die() { log "error: $*"; exit 1; }

command -v docker >/dev/null 2>&1 || die "docker not found on PATH"

awscli() {
    if [ "${MINISTACK_AWS_DOCKER:-0}" != 1 ] && command -v aws >/dev/null 2>&1; then
        AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_SESSION_TOKEN='' \
            AWS_DEFAULT_REGION="$region" AWS_PAGER='' \
            aws --endpoint-url http://localhost:4566 --output text "$@"
    else
        docker run --rm --network "container:$name" \
            -e AWS_ACCESS_KEY_ID=test -e AWS_SECRET_ACCESS_KEY=test \
            -e AWS_DEFAULT_REGION="$region" -e AWS_PAGER='' \
            "$cli_image" --endpoint-url http://localhost:4566 --output text "$@"
    fi
}

# 1. Container.
state=$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null || true)
if [ "$state" = true ]; then
    log "container $name already running"
else
    [ -n "$state" ] && docker rm -f "$name" >/dev/null
    log "starting $image as $name"
    # shellcheck disable=SC2086
    docker run -d --name "$name" \
        -p 4566:4566 -p 25432-25440:25432-25440 \
        -v /var/run/docker.sock:/var/run/docker.sock \
        -e DSQL_STRICT=1 \
        ${MINISTACK_DOCKER_FLAGS:-} \
        "$image" >/dev/null
fi

# 2. Control plane ready (polled through the CLI, so it also works when host
#    port forwarding is unavailable).
log "waiting for the control plane"
i=0
until awscli dsql list-clusters >/dev/null 2>&1; do
    i=$((i + 2)); [ "$i" -ge "$timeout" ] && die "control plane not ready after ${timeout}s (docker logs $name)"
    sleep 2
done

# 3. Reuse an ACTIVE cluster or create one.
cluster=""
for id in $(awscli dsql list-clusters --query 'clusters[].identifier'); do
    [ "$id" = None ] && continue
    if [ "$(awscli dsql get-cluster --identifier "$id" --query status)" = ACTIVE ]; then
        cluster=$id; break
    fi
done
if [ -z "$cluster" ]; then
    cluster=$(awscli dsql create-cluster --no-deletion-protection-enabled --query identifier)
    log "created cluster $cluster"
else
    log "reusing cluster $cluster"
fi

i=0
while [ "$(awscli dsql get-cluster --identifier "$cluster" --query status)" != ACTIVE ]; do
    i=$((i + 2)); [ "$i" -ge "$timeout" ] && die "cluster $cluster not ACTIVE after ${timeout}s"
    sleep 2
done
endpoint=$(awscli dsql get-cluster --identifier "$cluster" --query endpoint)
case $endpoint in
    *:*) host=${endpoint%:*}; port=${endpoint##*:} ;;
    *) die "unexpected endpoint '$endpoint' (is DSQL_STRICT=1 honoured?)" ;;
esac
host=${MINISTACK_PGHOST:-$host}
log "cluster $cluster ACTIVE at $host:$port"

# 4. Data plane ready (only checked when psql is available).
if command -v psql >/dev/null 2>&1; then
    i=0
    until PGHOST=$host PGPORT=$port PGUSER=admin PGDATABASE=postgres PGPASSWORD=any \
        PGSSLMODE=disable PGCONNECT_TIMEOUT=5 psql -X -q -At -c 'SELECT 1' >/dev/null 2>&1; do
        i=$((i + 2)); [ "$i" -ge "$timeout" ] && die "endpoint $host:$port not accepting SQL after ${timeout}s"
        sleep 2
    done
    log "endpoint accepts SQL"
else
    log "psql not on PATH; skipped the SQL readiness check"
fi

cat <<EXPORTS
export PGHOST=$host
export PGPORT=$port
export PGUSER=admin
export PGDATABASE=postgres
export PGPASSWORD=anything
export PGSSLMODE=disable
EXPORTS
