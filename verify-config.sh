#!/usr/bin/env bash
# Deployment preflight for Beam.
# Checks that the environment variables beam-server and beam-web actually
# read are set to something sane before you run `podman compose up`.
# beam-server/src/config.rs is the single authority for server variable
# names and defaults -- keep this script in sync with it (full reference:
# docs/operations/configuration.md).

set -e

echo "=== Beam Deployment Configuration Verification ==="
echo ""

if [ ! -f .env ]; then
    echo "❌ .env file not found!"
    echo "   Please copy .env.example to .env and configure it:"
    echo "   cp .env.example .env"
    exit 1
fi

echo "✅ .env file found"
echo ""

set -a
source .env
set +a

ERRORS=0

check_var() {
    local var_name=$1
    local var_value=${!var_name}
    local is_critical=${2:-false}

    if [ -z "$var_value" ]; then
        if [ "$is_critical" = "true" ]; then
            echo "❌ $var_name is not set (CRITICAL)"
            ERRORS=$((ERRORS + 1))
        else
            echo "⚠️  $var_name is not set (optional; server default applies)"
        fi
    else
        echo "✅ $var_name = $var_value"
    fi
}

# Like check_var, but never echoes the value -- for secrets.
check_secret() {
    local var_name=$1
    local var_value=${!var_name}

    if [ -z "$var_value" ]; then
        echo "⚠️  $var_name is not set (optional)"
    else
        echo "✅ $var_name is set (value hidden)"
    fi
}

echo "Checking Database Configuration:"
check_var "POSTGRES_USER" true
check_secret "POSTGRES_PASSWORD"
check_var "POSTGRES_DB" true
check_secret "BEAM_DATABASE_URL"
echo ""

echo "Checking Backend Server Configuration:"
check_var "BEAM_BIND_ADDRESS"
check_var "BEAM_SERVER_URL" true
check_var "BEAM_VIDEO_DIR"
check_var "BEAM_DATA_DIR"
check_var "BEAM_AUTO_MIGRATE"
check_var "BEAM_ENABLE_METRICS"
check_var "RUST_LOG"
echo ""

echo "Checking Indexing / Filesystem Watcher Configuration:"
check_var "BEAM_HASH_UNKNOWN_FILES"
check_var "BEAM_SCAN_INTERVAL_SECS"
check_var "BEAM_WATCH_ENABLED"
check_var "BEAM_WATCH_DEBOUNCE_MS"
echo ""

echo "Checking Metadata Enrichment Configuration (cameo -> TMDB/AniList):"
check_var "BEAM_ENRICH_INTERVAL_SECS"
check_secret "BEAM_TMDB_API_TOKEN"
check_var "BEAM_ANILIST_ENABLED"
if [ -z "$BEAM_TMDB_API_TOKEN" ] && [ "${BEAM_ANILIST_ENABLED:-true}" = "false" ]; then
    echo "⚠️  No TMDB token and AniList disabled -- metadata enrichment will be entirely disabled"
fi
echo ""

echo "Checking OIDC Auth Configuration (see ADR-0003):"
check_var "BEAM_OIDC_ISSUER"
check_var "BEAM_OIDC_CLIENT_ID"
check_secret "BEAM_OIDC_CLIENT_SECRET"
check_var "BEAM_OIDC_SCOPES"
check_var "BEAM_WEB_URL"
check_var "BEAM_EXTRA_ALLOWED_ORIGINS"
check_var "BEAM_ADMIN_EMAILS"
check_var "BEAM_COOKIE_SECURE"
check_var "BEAM_SESSION_IDLE_DAYS"
check_var "BEAM_SESSION_MAX_DAYS"
# All three must be set together; a partial set is the most common
# misconfiguration and the server treats it as "not configured".
OIDC_SET=0
[ -n "$BEAM_OIDC_ISSUER" ] && OIDC_SET=$((OIDC_SET + 1))
[ -n "$BEAM_OIDC_CLIENT_ID" ] && OIDC_SET=$((OIDC_SET + 1))
[ -n "$BEAM_OIDC_CLIENT_SECRET" ] && OIDC_SET=$((OIDC_SET + 1))
if [ "$OIDC_SET" -eq 0 ]; then
    echo "⚠️  OIDC is not configured -- login will be disabled"
elif [ "$OIDC_SET" -lt 3 ]; then
    echo "❌ OIDC is partially configured ($OIDC_SET of 3): issuer, client_id, and client_secret must all be set -- login will be disabled"
    ERRORS=$((ERRORS + 1))
fi
echo ""

echo "Checking Frontend Configuration:"
check_var "C_APP_TITLE"
check_var "C_STREAM_SERVER_URL" true
echo ""

echo "Checking Port Mappings:"
check_var "BEAM_SERVER_HOST_PORT"
check_var "WEB_HOST_PORT"
check_var "POSTGRES_HOST_PORT"
check_var "DEX_HOST_PORT"
echo ""

# Security warnings
echo "=== Security Checks ==="
if [ "$POSTGRES_PASSWORD" = "password" ]; then
    echo "⚠️  WARNING: Using default PostgreSQL password!"
    echo "   Please change POSTGRES_PASSWORD in production!"
fi

if [[ "$BEAM_SERVER_URL" == *"localhost"* ]] || [[ "$C_STREAM_SERVER_URL" == *"localhost"* ]]; then
    echo "⚠️  INFO: Using localhost URLs (OK for development)"
fi

# Mirrors beam-server's startup check (ServerConfig::cookie_security_verdict):
# an HTTPS-looking deployment whose cookies would resolve insecure refuses to
# boot unless BEAM_COOKIE_SECURE is set explicitly.
if [[ "${BEAM_WEB_URL:-}" == https://* ]] && [[ "${BEAM_SERVER_URL:-http://localhost:8000}" != https://* ]] && [ -z "${BEAM_COOKIE_SECURE:-}" ]; then
    echo "❌ BEAM_WEB_URL is HTTPS but BEAM_SERVER_URL is not, and BEAM_COOKIE_SECURE is unset."
    echo "   beam-server will refuse to start. Set BEAM_SERVER_URL to the externally-visible"
    echo "   HTTPS URL, or set BEAM_COOKIE_SECURE explicitly."
    ERRORS=$((ERRORS + 1))
fi
if [ "${BEAM_COOKIE_SECURE:-}" = "false" ] && [[ "${BEAM_SERVER_URL:-}" != *"localhost"* ]]; then
    echo "⚠️  WARNING: BEAM_COOKIE_SECURE=false with a non-localhost BEAM_SERVER_URL -- session cookies will not be marked Secure!"
fi

echo ""
echo "=== Summary ==="
if [ $ERRORS -eq 0 ]; then
    echo "✅ Configuration is valid! You can start the services with:"
    echo "   podman compose up -d"
    echo "   # or"
    echo "   docker compose up -d"
    exit 0
else
    echo "❌ Found $ERRORS critical configuration error(s)"
    echo "   Please fix the errors above before deploying"
    exit 1
fi
