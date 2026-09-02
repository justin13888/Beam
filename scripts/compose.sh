# Shared by the dev:* tasks in mise.toml. Source it; it is not a program.
#
# Two things every dev task needs and none should restate: which compose
# provider to drive, and how to read a published port back off it.

# BEAM_COMPOSE wins when set (docs/operations/configuration.md); otherwise
# podman when it is on PATH, else docker. mise runs tasks from the repo root,
# so `. scripts/compose.sh` resolves without a path prefix.
compose_cmd=${BEAM_COMPOSE:-}
[ -n "$compose_cmd" ] || compose_cmd=$(command -v podman >/dev/null 2>&1 && echo "podman compose" || echo "docker compose")
compose() { $compose_cmd "$@"; }

# host_url: one line of `compose port` output on stdin, an http:// URL on
# localhost on stdout. The two providers print the host side differently --
# Docker Compose `0.0.0.0:8000` (or `[::]:8000`), podman-compose the bare
# `8000` -- and the bare form fed to a naive `http://` prefix yields
# `http://8000`, a URL nothing listens on. Empty input stays empty so a caller
# can fall back.
host_url() {
    sed -E 's#^(0\.0\.0\.0|\[::\]):#localhost:#; s#^[0-9]+$#localhost:&#; s#^.+$#http://&#'
}

# published_url SERVICE CONTAINER_PORT: where the running stack publishes
# SERVICE's CONTAINER_PORT, or nothing when it is not up. Read off the container
# rather than restated from the compose defaults: the *_HOST_PORT variables are
# documented overrides, and printing a URL the stack is not on is worse than
# printing none.
published_url() {
    compose port "$1" "$2" 2>/dev/null | tail -n1 | host_url
}
