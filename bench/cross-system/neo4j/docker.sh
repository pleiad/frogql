#!/usr/bin/env bash
# Neo4j container helper for the cross-system bench.
#
#   docker.sh up      — start (or restart) the bench container and wait
#                       until bolt accepts queries
#   docker.sh down    — stop + remove the container (data lives inside
#                       the container; setup.py reloads from CSVs)
#   docker.sh status  — one-line container + bolt readiness report
#
# Container: frogql-bench-neo4j (neo4j:5 community)
#   bolt  : localhost:7687
#   http  : localhost:7474
#   auth  : neo4j / benchbench  (NEO4J_AUTH)
#   heap  : 4G   page cache: 2G
#
# No volume is mounted on purpose: the dataset is small (SF0.1) and
# setup.py reloads it from the LDBC CSVs in ~minutes, so a fresh
# container is the cheapest "wipe" primitive (`down` + `up`).

set -euo pipefail

CONTAINER=frogql-bench-neo4j
IMAGE=neo4j:5
BOLT_PORT=7687
HTTP_PORT=7474
NEO4J_USER=neo4j
NEO4J_PASSWORD=benchbench

usage() {
    echo "usage: $0 {up|down|status}" >&2
    exit 1
}

bolt_ready() {
    # cypher-shell inside the container is the authoritative readiness
    # probe — it exercises auth + bolt + the default database, not just
    # the TCP port (which opens well before the DB accepts queries).
    docker exec "$CONTAINER" cypher-shell \
        -u "$NEO4J_USER" -p "$NEO4J_PASSWORD" "RETURN 1" \
        >/dev/null 2>&1
}

cmd_up() {
    if docker ps --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        echo "  $CONTAINER already running" >&2
    else
        # Remove a stopped leftover with the same name, if any.
        docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
        echo "  starting $CONTAINER ($IMAGE)..." >&2
        docker run -d --name "$CONTAINER" \
            -p "$BOLT_PORT:7687" -p "$HTTP_PORT:7474" \
            -e NEO4J_AUTH="$NEO4J_USER/$NEO4J_PASSWORD" \
            -e NEO4J_server_memory_heap_initial__size=4G \
            -e NEO4J_server_memory_heap_max__size=4G \
            -e NEO4J_server_memory_pagecache_size=2G \
            "$IMAGE" >/dev/null
    fi

    echo -n "  waiting for bolt" >&2
    for _ in $(seq 1 120); do
        if bolt_ready; then
            echo " ready." >&2
            return 0
        fi
        echo -n "." >&2
        sleep 1
    done
    echo " TIMEOUT (120s). Container logs:" >&2
    docker logs --tail 50 "$CONTAINER" >&2 || true
    return 1
}

cmd_down() {
    if docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        docker rm -f "$CONTAINER" >/dev/null
        echo "  $CONTAINER stopped + removed" >&2
    else
        echo "  $CONTAINER not present" >&2
    fi
}

cmd_status() {
    if ! docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER"; then
        echo "  $CONTAINER: not present"
        return 1
    fi
    state=$(docker inspect -f '{{.State.Status}}' "$CONTAINER")
    if [ "$state" = "running" ] && bolt_ready; then
        echo "  $CONTAINER: running, bolt ready (localhost:$BOLT_PORT)"
    else
        echo "  $CONTAINER: $state, bolt NOT ready"
        return 1
    fi
}

case "${1:-}" in
    up)     cmd_up ;;
    down)   cmd_down ;;
    status) cmd_status ;;
    *)      usage ;;
esac
