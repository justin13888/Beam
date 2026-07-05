#!/usr/bin/env bash
# Deployment verification script for Beam.
# Checks that the environment variables beam-server and beam-web actually
# read (see beam-server/src/config.rs and beam-web/src/env.ts) are set to
# something sane before you run `podman compose up`. Env var names/defaults
# here must stay in sync with those two files -- see docs/operations/
# configuration.md for the full reference.

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
            echo "⚠️  $var_name is not set (optional)"
        fi
    else
        echo "✅ $var_name = $var_value"
    fi
}

echo "Checking Database Configuration:"
check_var "POSTGRES_USER" true
check_var "POSTGRES_PASSWORD" true
check_var "POSTGRES_DB" true
check_var "DATABASE_URL" true
echo ""

echo "Checking Backend Server Configuration:"
check_var "BIND_ADDRESS" true
check_var "SERVER_URL" true
check_var "VIDEO_DIR" true
check_var "CACHE_DIR" true
check_var "ENABLE_METRICS"
check_var "RUST_LOG"
echo ""

echo "Checking Indexing / Filesystem Watcher Configuration:"
check_var "HASH_UNKNOWN_FILES"
check_var "SCAN_INTERVAL_SECS"
check_var "WATCH_ENABLED"
check_var "WATCH_DEBOUNCE_MS"
echo ""

echo "Checking Metadata Enrichment Configuration (cameo -> TMDB/AniList):"
check_var "ENRICH_INTERVAL_SECS"
check_var "TMDB_API_TOKEN"
check_var "ANILIST_ENABLED"
if [ -z "$TMDB_API_TOKEN" ] && [ "${ANILIST_ENABLED:-true}" = "false" ]; then
    echo "⚠️  Neither TMDB_API_TOKEN nor ANILIST_ENABLED is set -- metadata enrichment will be entirely disabled"
fi
echo ""

echo "Checking OIDC Auth Configuration (see ADR-0003):"
check_var "BEAM_OIDC_ISSUER"
check_var "BEAM_OIDC_CLIENT_ID"
check_var "BEAM_OIDC_CLIENT_SECRET"
check_var "BEAM_OIDC_SCOPES"
check_var "BEAM_WEB_URL"
check_var "BEAM_EXTRA_ALLOWED_ORIGINS"
check_var "BEAM_ADMIN_EMAILS"
check_var "BEAM_COOKIE_SECURE"
check_var "BEAM_SESSION_IDLE_DAYS"
check_var "BEAM_SESSION_MAX_DAYS"
if [ -z "$BEAM_OIDC_ISSUER" ] || [ -z "$BEAM_OIDC_CLIENT_ID" ] || [ -z "$BEAM_OIDC_CLIENT_SECRET" ]; then
    echo "⚠️  OIDC is not fully configured (issuer/client_id/client_secret must all be set) -- login will be disabled"
fi
echo ""

echo "Checking Frontend Configuration:"
check_var "C_APP_TITLE"
check_var "C_STREAM_SERVER_URL" true
echo ""

echo "Checking Port Mappings:"
check_var "STREAM_HOST_PORT"
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

if [[ "$SERVER_URL" == *"localhost"* ]] || [[ "$C_STREAM_SERVER_URL" == *"localhost"* ]]; then
    echo "⚠️  INFO: Using localhost URLs (OK for development)"
fi

if [ "${BEAM_COOKIE_SECURE:-}" = "false" ] && [[ "$SERVER_URL" != *"localhost"* ]]; then
    echo "⚠️  WARNING: BEAM_COOKIE_SECURE=false with a non-localhost SERVER_URL -- session cookies will not be marked Secure!"
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
