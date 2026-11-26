#!/usr/bin/env bash
# local-deploy.sh - install, inspect and maintain a single-node AgentRecall
# deployment on one machine.
#
# The target topology is the loopback-only one: one MCP service plus the
# optional Hermes router, both bound to 127.0.0.1, with no replication peer.
# For the two-node local+cloud topology see docs/deployment.md instead.
#
# Application files and secrets stay under one prefix; supervisor files are
# written to the standard per-user launchd/systemd directories. Secrets are
# generated locally, stored 0600, and never printed. The client bearer token
# is shown once by `agentrecall create-token`, so the script captures it into a
# 0600 file rather than echoing it to a log.
#
# Usage:
#   scripts/local-deploy.sh install [options]
#   scripts/local-deploy.sh status
#   scripts/local-deploy.sh backup
#   scripts/local-deploy.sh env
#   scripts/local-deploy.sh uninstall [--purge]
#
set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# ---------------------------------------------------------------- defaults --

PREFIX="$HOME/.local/share/agentrecall"
BIND_ADDR="127.0.0.1:8789"
ROUTER_BIND_ADDR="127.0.0.1:8788"
OWNER_ID="local-owner"
CLIENT_ID="hermes-local"
SCOPES="memory:read,memory:write"
NAMESPACES="shared"
SUPERVISOR="auto"
USE_EMBEDDINGS="yes"
PROVISION_EXTENSIONS="yes"
BINARY=""
SKIP_BUILD="no"
QUOTA_PER_OWNER="10000"
LABEL_PREFIX="com.agentrecall"

# ------------------------------------------------------------------ output --

if [ -t 1 ]; then
    C_RESET=$'\033[0m'; C_DIM=$'\033[2m'
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
else
    C_RESET=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""
fi

log()  { printf '%s==>%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
step() { printf '%s  ->%s %s\n' "$C_DIM" "$C_RESET" "$*"; }
warn() { printf '%swarn:%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    cat <<'EOF'

Options:
  --prefix DIR         Installation root (default: $HOME/.local/share/agentrecall)
  --bind ADDR          Service bind address (default: 127.0.0.1:8789)
  --router-bind ADDR   Router bind address (default: 127.0.0.1:8788)
  --owner ID           Owner id for the created client token (default: local-owner)
  --client ID          Client id for the created client token (default: hermes-local)
  --scopes LIST        Comma separated scopes (default: memory:read,memory:write)
  --namespaces LIST    Comma separated namespace allowlist (default: shared)
  --quota N            Records allowed per owner (default: 10000)
  --label-prefix P     Supervisor label prefix (default: com.agentrecall)
  --supervisor NAME    auto | launchd | systemd | none (default: auto)
  --no-router          Do not install the Hermes router
  --no-embeddings      Stay lexical-only, skip the BGE-M3 provider
  --no-extensions      Skip DuckDB FTS/VSS provisioning
  --binary PATH        Use an existing binary instead of building
  --skip-build         Do not build even if no binary is given
  --purge              With uninstall: also delete the prefix directory
EOF
}

# ------------------------------------------------------------------ helpers --

need() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }

random_hex() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    else
        od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
    fi
}

xml_escape() {
    printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

systemd_quote_unit_value() {
    local input="$1"
    local escaped="" char ordinal hex i
    local LC_ALL=C
    for ((i = 0; i < ${#input}; i++)); do
        char="${input:i:1}"
        case "$char" in
            \\) escaped+="\\\\" ;;
            '"') escaped+='\"' ;;
            '%') escaped+='%%' ;;
            '$') escaped+='$' ;;
            $'\a') escaped+='\a' ;;
            $'\b') escaped+='\b' ;;
            $'\f') escaped+='\f' ;;
            $'\n') escaped+='\n' ;;
            $'\r') escaped+='\r' ;;
            $'\t') escaped+='\t' ;;
            $'\v') escaped+='\v' ;;
            *)
                printf -v ordinal '%d' "'$char"
                if [ "$ordinal" -lt 32 ] || [ "$ordinal" -eq 127 ]; then
                    printf -v hex '\\x%02x' "$ordinal"
                    escaped+="$hex"
                else
                    escaped+="$char"
                fi
                ;;
        esac
    done
    printf '"%s"' "$escaped"
}

systemd_escape_path_value() {
    local input="$1"
    local escaped="" char ordinal hex i
    local LC_ALL=C
    for ((i = 0; i < ${#input}; i++)); do
        char="${input:i:1}"
        case "$char" in
            [A-Za-z0-9_./:-]) escaped+="$char" ;;
            '%') escaped+='%%' ;;
            *)
                printf -v ordinal '%d' "'$char"
                if [ "$ordinal" -ge 128 ]; then
                    escaped+="$char"
                else
                    printf -v hex '\\x%02x' "$ordinal"
                    escaped+="$hex"
                fi
                ;;
        esac
    done
    printf '%s' "$escaped"
}

systemd_quote_env_value() {
    local input="$1"
    local escaped="" char i
    local LC_ALL=C
    for ((i = 0; i < ${#input}; i++)); do
        char="${input:i:1}"
        case "$char" in
            \\) escaped+="\\\\" ;;
            '"') escaped+='\"' ;;
            '$') escaped+='\$' ;;
            '`') escaped+='\`' ;;
            *) escaped+="$char" ;;
        esac
    done
    printf '"%s"' "$escaped"
}

# Installs the binary through a temporary file and a rename. Overwriting a
# running Mach-O executable in place invalidates its code signature, and macOS
# AMFI then kills the next launch of that file with SIGKILL. A rename gives the
# new file a fresh inode, keeps the signature valid, and leaves a running
# process on the old inode until it is restarted.
install_binary_atomic() {
    src="$1"
    dst="$2"
    tmp="$dst.new.$$"
    rm -f "$tmp"
    cp "$src" "$tmp" || die "could not copy $src into place"
    chmod 755 "$tmp"
    sync
    mv -f "$tmp" "$dst" || { rm -f "$tmp"; die "could not move the binary into $dst"; }
}

# Confirms the installed binary can actually be launched. AMFI kills an
# unsigned or half-written Mach-O image, so a successful copy is not proof that
# the next command will run.
verify_binary_runs() {
    target="$1"
    attempt=1
    while [ "$attempt" -le 3 ]; do
        if "$target" --help >/dev/null 2>&1; then
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    die "the binary at $target cannot be executed"
}

probe_host() {
    case "$1" in
        0.0.0.0:*|:*|"") printf '127.0.0.1' ;;
        *) printf '%s' "${1%%:*}" ;;
    esac
}

validate_loopback_bind() {
    label="$1"
    address="$2"
    case "$address" in
        127.0.0.1:*) port="${address#*:}" ;;
        *) die "$label must bind to 127.0.0.1 (got '$address')" ;;
    esac
    case "$port" in
        ''|*[!0-9]*) die "$label must include a numeric port from 1 to 65535" ;;
    esac
    [ "${#port}" -le 5 ] || die "$label port must be from 1 to 65535"
    port_number=$((10#$port))
    if [ "$port_number" -lt 1 ] || [ "$port_number" -gt 65535 ]; then
        die "$label port must be from 1 to 65535"
    fi
}

physical_dir() {
    (CDPATH='' cd -P -- "$1" 2>/dev/null && pwd -P)
}

validate_prefix_directory() {
    [ -n "$PREFIX" ] || die "installation prefix must not be empty"
    prefix_real="$(physical_dir "$PREFIX")" || die "cannot resolve installation prefix: $PREFIX"
    home_real="$(physical_dir "$HOME")" || die "cannot resolve home directory: $HOME"
    [ "$prefix_real" != "/" ] || die "refusing to use the filesystem root as the installation prefix"
    case "$home_real" in
        "$prefix_real"|"$prefix_real"/*)
            die "refusing to use the home directory or one of its ancestors as the installation prefix"
            ;;
    esac
    case "$prefix_real" in
        /tmp|/private/tmp|/var|/private/var|/var/tmp|/private/var/tmp|/usr|/usr/local|/opt|/etc|/home|/Users|/root|/workspace|/System|/Library|/Applications|"$home_real/.local"|"$home_real/.local/share"|"$home_real/.config"|"$home_real/Library")
            die "refusing to use a shared or system directory as the installation prefix: $prefix_real"
            ;;
    esac
    for entry in "$prefix_real"/* "$prefix_real"/.[!.]* "$prefix_real"/..?*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        entry_name="${entry##*/}"
        case "$entry_name" in
            bin|config|data|logs|backups) ;;
            *) die "refusing to use a prefix containing an unmanaged top-level entry: $entry" ;;
        esac
    done
}

BIN="$PREFIX/bin/agentrecall"
CONFIG_DIR="$PREFIX/config"
DATA_DIR="$PREFIX/data"
LOG_DIR="$PREFIX/logs"
BACKUP_DIR="$PREFIX/backups"
TOKENS_FILE="$CONFIG_DIR/tokens.json"
ENV_FILE="$CONFIG_DIR/env"
CURSOR_SECRET_FILE="$CONFIG_DIR/cursor_hmac.secret"
ROUTER_SECRET_FILE="$CONFIG_DIR/router_ingress.secret"
SERVICE_SECRET_FILE="$CONFIG_DIR/local_service.secret"

require_installed() {
    [ -x "$BIN" ] || die "no binary at $BIN; run '$SCRIPT_NAME install' first"
}

read_secret() {
    [ -s "$1" ] || die "missing secret file: $1"
    cat "$1"
}

ensure_secret() {
    # ensure_secret <path>: create a random 64-hex secret if absent.
    if [ ! -s "$1" ]; then
        (umask 077; printf '%s' "$(random_hex)" > "$1")
        step "generated $(basename "$1")"
    fi
}

# Returns 0 when the token file holds an active credential for the client.
token_file_has_active_client() {
    client="$1"
    [ -f "$TOKENS_FILE" ] || return 1
    awk -v want="$client" '
        /"client_id"/ {
            line = $0
            sub(/.*"client_id"[[:space:]]*:[[:space:]]*"/, "", line)
            sub(/".*/, "", line)
            hit = (line == want)
        }
        /"active"/ {
            if (hit && $0 ~ /true/) { found = 1 }
            hit = 0
        }
        END { exit(found ? 0 : 1) }
    ' "$TOKENS_FILE"
}

# Prints the first 64-hex token in the given text.
first_token() {
    printf '%s\n' "$1" | grep -oE '[a-f0-9]{64}' | awk 'NR==1'
}

# Obtains a raw client token, reusing an existing one when possible.
ensure_client_token() {
    out_file="$1"
    if [ -s "$out_file" ] && token_file_has_active_client "$CLIENT_ID"; then
        step "reusing the existing token for client '$CLIENT_ID'"
        return 0
    fi
    if token_file_has_active_client "$CLIENT_ID"; then
        output="$("$BIN" rotate-token --client-id "$CLIENT_ID" --tokens-file "$TOKENS_FILE" 2>&1)" || true
        action="rotated"
    else
        output="$("$BIN" create-token --owner-id "$OWNER_ID" --client-id "$CLIENT_ID" \
            --scopes "$SCOPES" --namespaces "$NAMESPACES" --tokens-file "$TOKENS_FILE" 2>&1)" || true
        action="created"
    fi
    raw="$(first_token "$output" || true)"
    if [ -z "$raw" ]; then
        printf '%s\n' "$output" >&2
        die "could not obtain a token for client '$CLIENT_ID'"
    fi
    (umask 077; printf '%s' "$raw" > "$out_file")
    step "$action the token for client '$CLIENT_ID' -> $out_file"
}

write_env_file() {
    {
        printf '# Generated by %s. Keep 0600: it holds live credentials.\n' "$SCRIPT_NAME"
        printf 'AGENTRECALL_ROLE=local\n'
        printf 'AGENTRECALL_BIND_ADDR=%s\n' "$BIND_ADDR"
        printf 'AGENTRECALL_DATABASE_PATH=%s\n' "$(systemd_quote_env_value "$DATA_DIR/agentrecall.duckdb")"
        printf 'AGENTRECALL_TOKENS_FILE=%s\n' "$(systemd_quote_env_value "$TOKENS_FILE")"
        printf 'AGENTRECALL_ALLOWED_HOSTS=localhost,127.0.0.1\n'
        printf 'AGENTRECALL_ALLOWED_ORIGINS=\n'
        printf 'AGENTRECALL_CURSOR_HMAC_SECRET=%s\n' "$(read_secret "$CURSOR_SECRET_FILE")"
        printf 'AGENTRECALL_QUOTA_PER_OWNER=%s\n' "$QUOTA_PER_OWNER"
        if [ "$USE_EMBEDDINGS" = "yes" ]; then
            printf 'AGENTRECALL_EMBEDDING_PROVIDER=bge-m3\n'
            printf 'AGENTRECALL_EMBEDDING_CACHE_DIR=%s\n' "$(systemd_quote_env_value "$DATA_DIR/models")"
        fi
        if [ "$WITH_ROUTER" = "yes" ]; then
            printf 'AGENTRECALL_ROUTER_BIND_ADDR=%s\n' "$ROUTER_BIND_ADDR"
            printf 'AGENTRECALL_ROUTER_LOCAL_URL=http://%s/mcp\n' "$BIND_ADDR"
            printf 'AGENTRECALL_ROUTER_HERMES_TOKEN=%s\n' "$(read_secret "$ROUTER_SECRET_FILE")"
            printf 'AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN=%s\n' "$(read_secret "$SERVICE_SECRET_FILE")"
        fi
    } > "$ENV_FILE"
    chmod 600 "$ENV_FILE"
    step "wrote $ENV_FILE"
}

# ------------------------------------------------------------- supervisors --

detect_supervisor() {
    case "$SUPERVISOR" in
        auto)
            case "$(uname -s)" in
                Darwin) printf 'launchd' ;;
                Linux)
                    if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
                        printf 'systemd'
                    else
                        printf 'none'
                    fi
                    ;;
                *) printf 'none' ;;
            esac
            ;;
        *) printf '%s' "$SUPERVISOR" ;;
    esac
}

# The deployment records the supervisor it was installed with. Later commands
# use that record, so a status report describes this deployment and not
# whatever happens to be detected on the host right now.
effective_supervisor() {
    if [ -s "$SUPERVISOR_STATE_FILE" ]; then
        recorded=""
        IFS= read -r recorded < "$SUPERVISOR_STATE_FILE" || true
        if [ -n "$recorded" ]; then
            printf '%s' "$recorded"
            return 0
        fi
    fi
    detect_supervisor
}

load_deployment_options() {
    [ -s "$SUPERVISOR_STATE_FILE" ] || return 0
    saved_label_prefix="$LABEL_PREFIX"
    saved_router="$WITH_ROUTER"
    saved_bind="$BIND_ADDR"
    saved_router_bind="$ROUTER_BIND_ADDR"
    saved_namespaces="$NAMESPACES"
    line_number=0
    while IFS= read -r line || [ -n "$line" ]; do
        line_number=$((line_number + 1))
        [ "$line_number" -eq 1 ] && continue
        key="${line%%=*}"
        value="${line#*=}"
        case "$key" in
            label_prefix) saved_label_prefix="$value" ;;
            router) saved_router="$value" ;;
            bind_addr) saved_bind="$value" ;;
            router_bind_addr) saved_router_bind="$value" ;;
            namespaces) saved_namespaces="$value" ;;
        esac
    done < "$SUPERVISOR_STATE_FILE"
    [ -n "$LABEL_PREFIX_OVERRIDE" ] || LABEL_PREFIX="$saved_label_prefix"
    [ -n "$WITH_ROUTER_OVERRIDE" ] || WITH_ROUTER="$saved_router"
    [ -n "$BIND_ADDR_OVERRIDE" ] || BIND_ADDR="$saved_bind"
    [ -n "$ROUTER_BIND_ADDR_OVERRIDE" ] || ROUTER_BIND_ADDR="$saved_router_bind"
    [ -n "$NAMESPACES_OVERRIDE" ] || NAMESPACES="$saved_namespaces"
}

load_generated_config() {
    [ -r "$ENV_FILE" ] || die "generated environment file is missing or unreadable: $ENV_FILE"
    generated_bind_addr=""
    generated_router_bind_addr=""
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            AGENTRECALL_BIND_ADDR=*) generated_bind_addr="${line#*=}" ;;
            AGENTRECALL_ROUTER_BIND_ADDR=*) generated_router_bind_addr="${line#*=}" ;;
        esac
    done < "$ENV_FILE"

    [ -n "$generated_bind_addr" ] || die "generated environment file has no service bind address"
    if [ -n "$BIND_ADDR_OVERRIDE" ]; then
        generated_bind_addr="$BIND_ADDR_OVERRIDE"
    fi
    validate_loopback_bind "service" "$generated_bind_addr"
    BIND_ADDR="$generated_bind_addr"
    BIND_PORT="${BIND_ADDR##*:}"

    if [ "$WITH_ROUTER_OVERRIDE" = "no" ]; then
        WITH_ROUTER="no"
    elif [ -n "$generated_router_bind_addr" ]; then
        if [ -n "$ROUTER_BIND_ADDR_OVERRIDE" ]; then
            generated_router_bind_addr="$ROUTER_BIND_ADDR_OVERRIDE"
        fi
        validate_loopback_bind "router" "$generated_router_bind_addr"
        ROUTER_BIND_ADDR="$generated_router_bind_addr"
        ROUTER_PORT="${ROUTER_BIND_ADDR##*:}"
        WITH_ROUTER="yes"
    else
        WITH_ROUTER="no"
    fi
}

write_deployment_state() {
    {
        printf '%s\n' "$(detect_supervisor)"
        printf 'label_prefix=%s\n' "$LABEL_PREFIX"
        printf 'router=%s\n' "$WITH_ROUTER"
        printf 'bind_addr=%s\n' "$BIND_ADDR"
        printf 'router_bind_addr=%s\n' "$ROUTER_BIND_ADDR"
        printf 'namespaces=%s\n' "$NAMESPACES"
    } > "$SUPERVISOR_STATE_FILE"
    chmod 600 "$SUPERVISOR_STATE_FILE"
}

SERVICE_LABEL="$LABEL_PREFIX.service"
ROUTER_LABEL="$LABEL_PREFIX.router"
SERVICE_UNIT="$LABEL_PREFIX.service"
ROUTER_UNIT="$LABEL_PREFIX.router.service"
LAUNCHD_DIR="$HOME/Library/LaunchAgents"
SYSTEMD_DIR="$HOME/.config/systemd/user"

write_launchd_plist() {
    label="$1"; program="$2"; log_name="$3"
    plist="$LAUNCHD_DIR/$label.plist"
    mkdir -p "$LAUNCHD_DIR"
    {
        printf '<?xml version="1.0" encoding="UTF-8"?>\n'
        printf '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        printf '<plist version="1.0">\n<dict>\n'
        printf '\t<key>Label</key>\n\t<string>%s</string>\n' "$(xml_escape "$label")"
        printf '\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>%s</string>\n\t\t<string>%s</string>\n\t</array>\n' \
            "$(xml_escape "$BIN")" "$(xml_escape "$program")"
        printf '\t<key>WorkingDirectory</key>\n\t<string>%s</string>\n' "$(xml_escape "$PREFIX")"
        printf '\t<key>EnvironmentVariables</key>\n\t<dict>\n'
        printf '\t\t<key>PATH</key>\n\t\t<string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n'
        printf '\t\t<key>HOME</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$HOME")"
        printf '\t\t<key>AGENTRECALL_ROLE</key>\n\t\t<string>local</string>\n'
        printf '\t\t<key>AGENTRECALL_BIND_ADDR</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$BIND_ADDR")"
        printf '\t\t<key>AGENTRECALL_DATABASE_PATH</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$DATA_DIR/agentrecall.duckdb")"
        printf '\t\t<key>AGENTRECALL_TOKENS_FILE</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$TOKENS_FILE")"
        printf '\t\t<key>AGENTRECALL_ALLOWED_HOSTS</key>\n\t\t<string>localhost,127.0.0.1</string>\n'
        printf '\t\t<key>AGENTRECALL_CURSOR_HMAC_SECRET</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$(read_secret "$CURSOR_SECRET_FILE")")"
        printf '\t\t<key>AGENTRECALL_QUOTA_PER_OWNER</key>\n\t\t<string>%s</string>\n' "$QUOTA_PER_OWNER"
        if [ "$USE_EMBEDDINGS" = "yes" ]; then
            printf '\t\t<key>AGENTRECALL_EMBEDDING_PROVIDER</key>\n\t\t<string>bge-m3</string>\n'
            printf '\t\t<key>AGENTRECALL_EMBEDDING_CACHE_DIR</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$DATA_DIR/models")"
        fi
        if [ "$program" = "serve-router" ]; then
            printf '\t\t<key>AGENTRECALL_ROUTER_BIND_ADDR</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$ROUTER_BIND_ADDR")"
            printf '\t\t<key>AGENTRECALL_ROUTER_LOCAL_URL</key>\n\t\t<string>http://%s/mcp</string>\n' "$(xml_escape "$BIND_ADDR")"
            printf '\t\t<key>AGENTRECALL_ROUTER_HERMES_TOKEN</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$(read_secret "$ROUTER_SECRET_FILE")")"
            printf '\t\t<key>AGENTRECALL_ROUTER_LOCAL_SERVICE_TOKEN</key>\n\t\t<string>%s</string>\n' "$(xml_escape "$(read_secret "$SERVICE_SECRET_FILE")")"
        fi
        printf '\t</dict>\n'
        printf '\t<key>RunAtLoad</key>\n\t<true/>\n'
        printf '\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n'
        printf '\t<key>ThrottleInterval</key>\n\t<integer>30</integer>\n'
        printf '\t<key>ExitTimeOut</key>\n\t<integer>60</integer>\n'
        printf '\t<key>StandardOutPath</key>\n\t<string>%s</string>\n' "$(xml_escape "$LOG_DIR/$log_name.log")"
        printf '\t<key>StandardErrorPath</key>\n\t<string>%s</string>\n' "$(xml_escape "$LOG_DIR/$log_name.error.log")"
        printf '</dict>\n</plist>\n'
    } > "$plist"
    chmod 600 "$plist"
    step "wrote $plist"
}

write_systemd_unit() {
    unit_name="$1"; program="$2"
    unit="$SYSTEMD_DIR/$unit_name"
    mkdir -p "$SYSTEMD_DIR"
    {
        printf '[Unit]\nDescription=AgentRecall %s\nAfter=network.target\n\n' "$program"
        printf '[Service]\nType=simple\n'
        printf 'EnvironmentFile=%s\n' "$(systemd_escape_path_value "$ENV_FILE")"
        printf 'WorkingDirectory=%s\n' "$(systemd_escape_path_value "$PREFIX")"
        printf 'ExecStart=:/usr/bin/env %s %s\n' "$(systemd_quote_unit_value "$BIN")" "$program"
        printf 'Restart=on-failure\nRestartSec=10\n'
        printf 'TimeoutStopSec=60\n'
        printf 'StandardOutput=%s\n' "$(systemd_escape_path_value "append:$LOG_DIR/$program.log")"
        printf 'StandardError=%s\n' "$(systemd_escape_path_value "append:$LOG_DIR/$program.error.log")"
        printf '\n[Install]\nWantedBy=default.target\n'
    } > "$unit"
    chmod 600 "$unit"
    step "wrote $unit"
}

# Starts a launchd job. An already loaded job is restarted in place: booting
# it out and bootstrapping again races with launchd and can leave the job dead.
launchd_load() {
    label="$1"
    plist="$LAUNCHD_DIR/$label.plist"
    if launchctl list 2>/dev/null | grep -q "[[:space:]]$label\$"; then
        launchctl kickstart -k "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        step "restarted the loaded job $label"
        return 0
    fi
    launchctl bootstrap "gui/$(id -u)" "$plist" \
        || die "launchctl bootstrap failed for $label"
    step "bootstrapped $label"
    return 0
}

supervisor_start() {
    case "$1" in
        launchd)
            launchd_load "$SERVICE_LABEL" || return 1
            if [ "$WITH_ROUTER" = "yes" ]; then
                launchd_load "$ROUTER_LABEL" || return 1
            fi
            ;;
        systemd)
            if ! command -v systemctl >/dev/null 2>&1; then
                warn "systemctl is not available here; the unit files were written but not started"
                warn "start them later with: systemctl --user enable --now $SERVICE_UNIT"
                return 1
            fi
            systemctl --user daemon-reload || return 1
            systemctl --user enable --now "$SERVICE_UNIT" || return 1
            step "enabled $SERVICE_UNIT"
            if [ "$WITH_ROUTER" = "yes" ]; then
                systemctl --user enable --now "$ROUTER_UNIT" || return 1
                step "enabled $ROUTER_UNIT"
            fi
            ;;
        none)
            die "supervisor 'none' cannot start a background service; run the binary directly"
            ;;
    esac
}

INSTALL_CLEANUP_PENDING="no"
INSTALL_CLEANUP_SUPERVISOR=""

install_failure_cleanup() {
    install_status=$?
    trap - EXIT INT TERM
    if [ "$INSTALL_CLEANUP_PENDING" = "yes" ]; then
        warn "installation did not complete; stopping the partially started services"
        supervisor_stop "$INSTALL_CLEANUP_SUPERVISOR" >/dev/null 2>&1 || true
    fi
    exit "$install_status"
}

supervisor_stop() {
    case "$1" in
        launchd)
            stopped="no"
            for label in "$SERVICE_LABEL" "$ROUTER_LABEL"; do
                if launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1; then
                    stopped="yes"
                fi
            done
            if [ "$stopped" = "yes" ]; then
                step "stopped the launchd jobs"
            fi
            return 0
            ;;
        systemd)
            if command -v systemctl >/dev/null 2>&1; then
                systemctl --user disable --now "$SERVICE_UNIT" >/dev/null 2>&1 || true
                systemctl --user disable --now "$ROUTER_UNIT" >/dev/null 2>&1 || true
                step "stopped the systemd units"
            fi
            ;;
        none) ;;
    esac
}

BACKUP_RESTORE_PENDING="no"
BACKUP_SUPERVISOR=""
BACKUP_SERVICE_WAS_ACTIVE="no"
BACKUP_ROUTER_WAS_ACTIVE="no"
BACKUP_OUTPUT_FILE=""
BACKUP_VERIFY_FILE=""

supervisor_component_active() {
    supervisor="$1"
    component="$2"
    case "$supervisor:$component" in
        systemd:service) unit="$SERVICE_UNIT" ;;
        systemd:router) unit="$ROUTER_UNIT" ;;
        launchd:service) unit="$SERVICE_LABEL" ;;
        launchd:router) unit="$ROUTER_LABEL" ;;
        *) return 2 ;;
    esac
    if [ "$supervisor" = "systemd" ]; then
        if systemctl --user is-active --quiet "$unit"; then
            return 0
        else
            status=$?
            case "$status" in
                3|4) return 1 ;;
                *) return 2 ;;
            esac
        fi
    fi
    if launchctl print "gui/$(id -u)/$unit" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

supervisor_stop_for_backup() {
    case "$BACKUP_SUPERVISOR" in
        systemd)
            if [ "$BACKUP_SERVICE_WAS_ACTIVE" = "yes" ]; then
                systemctl --user stop "$SERVICE_UNIT" || return 1
            fi
            if [ "$BACKUP_ROUTER_WAS_ACTIVE" = "yes" ]; then
                systemctl --user stop "$ROUTER_UNIT" || return 1
            fi
            ;;
        launchd)
            if [ "$BACKUP_SERVICE_WAS_ACTIVE" = "yes" ]; then
                launchctl bootout "gui/$(id -u)/$SERVICE_LABEL" >/dev/null 2>&1 || return 1
            fi
            if [ "$BACKUP_ROUTER_WAS_ACTIVE" = "yes" ]; then
                launchctl bootout "gui/$(id -u)/$ROUTER_LABEL" >/dev/null 2>&1 || return 1
            fi
            ;;
        *) return 1 ;;
    esac
    if supervisor_component_active "$BACKUP_SUPERVISOR" service; then return 1; else [ "$?" -eq 1 ] || return 1; fi
    if supervisor_component_active "$BACKUP_SUPERVISOR" router; then return 1; else [ "$?" -eq 1 ] || return 1; fi
}

supervisor_restore_after_backup() {
    case "$BACKUP_SUPERVISOR" in
        systemd)
            if [ "$BACKUP_SERVICE_WAS_ACTIVE" = "yes" ]; then
                systemctl --user start "$SERVICE_UNIT" || return 1
            fi
            if [ "$BACKUP_ROUTER_WAS_ACTIVE" = "yes" ]; then
                systemctl --user start "$ROUTER_UNIT" || return 1
            fi
            ;;
        launchd)
            if [ "$BACKUP_SERVICE_WAS_ACTIVE" = "yes" ]; then
                launchctl bootstrap "gui/$(id -u)" "$LAUNCHD_DIR/$SERVICE_LABEL.plist" || return 1
            fi
            if [ "$BACKUP_ROUTER_WAS_ACTIVE" = "yes" ]; then
                launchctl bootstrap "gui/$(id -u)" "$LAUNCHD_DIR/$ROUTER_LABEL.plist" || return 1
            fi
            ;;
        *) return 1 ;;
    esac
}

backup_exit_cleanup() {
    backup_status=$?
    trap - EXIT INT TERM
    [ -z "$BACKUP_VERIFY_FILE" ] || rm -f "$BACKUP_VERIFY_FILE" || true
    if [ "$backup_status" -ne 0 ] && [ -n "$BACKUP_OUTPUT_FILE" ]; then
        rm -f "$BACKUP_OUTPUT_FILE" || true
    fi
    if [ "$BACKUP_RESTORE_PENDING" = "yes" ]; then
        BACKUP_RESTORE_PENDING="no"
        if ! supervisor_restore_after_backup; then
            warn "could not restore the service state after backup"
            [ "$backup_status" -ne 0 ] || backup_status=1
        fi
    fi
    exit "$backup_status"
}

supervisor_status() {
    case "$1" in
        launchd)
            launchctl list 2>/dev/null \
                | grep -E "^[-0-9]+[[:space:]]+[-0-9]+[[:space:]]+${LABEL_PREFIX//./\\.}\." \
                | sed 's/^/  /' || echo "  no launchd jobs"
            ;;
        systemd)
            systemctl --user list-units --no-pager "$LABEL_PREFIX.*" 2>/dev/null | sed 's/^/  /' || true
            ;;
        none) echo "  no supervisor" ;;
    esac
}

# ----------------------------------------------------------------- commands --

cmd_install() {
    need curl
    log "installing a single-node AgentRecall deployment into $PREFIX"

    [ -n "$PREFIX" ] || die "installation prefix must not be empty"
    mkdir -p "$PREFIX"
    validate_prefix_directory
    mkdir -p "$BIN_DIR" "$CONFIG_DIR" "$DATA_DIR/models" "$LOG_DIR" "$BACKUP_DIR"
    chmod 700 "$PREFIX" "$CONFIG_DIR" "$DATA_DIR" "$BACKUP_DIR"

    if [ -z "$BINARY" ] && [ "$SKIP_BUILD" = "no" ]; then
        log "building the release binary"
        (cd "$REPO_ROOT" && cargo build --release)
        install_binary_atomic "$REPO_ROOT/target/release/agentrecall" "$BIN"
    elif [ -n "$BINARY" ]; then
        install_binary_atomic "$BINARY" "$BIN"
        step "installed the binary from $BINARY"
    else
        [ -x "$REPO_ROOT/target/release/agentrecall" ] \
            || die "no binary available; drop --skip-build or pass --binary"
        install_binary_atomic "$REPO_ROOT/target/release/agentrecall" "$BIN"
    fi
    verify_binary_runs "$BIN"
    step "binary: $BIN"

    # Stop a running service before touching the database: DuckDB serialises
    # extension installation against a live database handle.
    supervisor_stop "$(detect_supervisor)" >/dev/null 2>&1 || true

    ensure_secret "$CURSOR_SECRET_FILE"
    ensure_secret "$SERVICE_SECRET_FILE"
    if [ "$WITH_ROUTER" = "yes" ]; then
        ensure_secret "$ROUTER_SECRET_FILE"
    fi

    ensure_client_token "$SERVICE_SECRET_FILE"

    # Stop a running service before provisioning extensions: DuckDB serialises
    # extension installation against a live database handle.
    supervisor_stop "$(detect_supervisor)" >/dev/null 2>&1 || true

    if [ "$PROVISION_EXTENSIONS" = "yes" ]; then
        log "provisioning the DuckDB extensions"
        ext_ok="yes"
        "$BIN" install-fts >/dev/null 2>&1 \
            || { warn "install-fts failed; lexical BM25 stays unavailable"; ext_ok="no"; }
        "$BIN" install-vss >/dev/null 2>&1 \
            || { warn "install-vss failed; semantic search falls back to exact vectors"; ext_ok="no"; }
        if [ "$ext_ok" = "yes" ]; then
            step "extensions provisioned"
        else
            step "extension provisioning incomplete (see the warnings above)"
        fi
    fi

    write_env_file

    # Record the supervisor this deployment uses, so that a later status or
    # uninstall reports and stops this deployment and not whatever happens to
    # be detected on the host right now.
    write_deployment_state

    started="yes"
    case "$(detect_supervisor)" in
        launchd)
            log "installing the launchd jobs"
            write_launchd_plist "$SERVICE_LABEL" serve service
            if [ "$WITH_ROUTER" = "yes" ]; then
                write_launchd_plist "$ROUTER_LABEL" serve-router router
            fi
            INSTALL_CLEANUP_SUPERVISOR="launchd"
            INSTALL_CLEANUP_PENDING="yes"
            trap 'install_failure_cleanup' EXIT
            trap 'exit 130' INT
            trap 'exit 143' TERM
            if ! supervisor_start launchd; then
                die "could not start the launchd deployment"
            fi
            ;;
        systemd)
            log "installing the systemd user units"
            write_systemd_unit "$SERVICE_UNIT" serve
            if [ "$WITH_ROUTER" = "yes" ]; then
                write_systemd_unit "$ROUTER_UNIT" serve-router
            fi
            INSTALL_CLEANUP_SUPERVISOR="systemd"
            INSTALL_CLEANUP_PENDING="yes"
            trap 'install_failure_cleanup' EXIT
            trap 'exit 130' INT
            trap 'exit 143' TERM
            if ! supervisor_start systemd; then
                die "could not start the systemd deployment"
            fi
            ;;
        none)
            started="no"
            warn "no usable supervisor found; start the service manually:"
            printf '    set -a; . %s; set +a; %s serve\n' "$ENV_FILE" "$BIN"
            ;;
    esac

    if [ "$started" = "no" ]; then
        log "the service was not started; the layout is in place"
        cmd_env
        return 0
    fi

    log "waiting for the service to become ready"
    if wait_for_ready; then
        log "service is ready"
    else
        die "the service did not become ready; stopped the deployment; check $LOG_DIR"
    fi

    if [ "$WITH_ROUTER" = "yes" ]; then
        if ! wait_for_router; then
            die "the router did not become ready; stopped the deployment; check $LOG_DIR"
        fi
    fi

    INSTALL_CLEANUP_PENDING="no"
    trap - EXIT INT TERM
    cmd_env
}

wait_for_ready() {
    host="$(probe_host "$BIND_ADDR")"
    i=0
    while [ "$i" -lt 60 ]; do
        if curl -fsS -m 5 "http://$host:$BIND_PORT/health/ready" >/dev/null 2>&1; then
            return 0
        fi
        i=$((i + 1))
        sleep 2
    done
    return 1
}

wait_for_router() {
    host="$(probe_host "$ROUTER_BIND_ADDR")"
    i=0
    while [ "$i" -lt 30 ]; do
        if curl -fsS -m 5 -o /dev/null -X POST "http://$host:$ROUTER_PORT/mcp" \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json, text/event-stream' \
            -H 'MCP-Protocol-Version: 2025-06-18' \
            -H "Authorization: Bearer $(read_secret "$ROUTER_SECRET_FILE")" \
            -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' 2>/dev/null; then
            return 0
        fi
        i=$((i + 1))
        sleep 2
    done
    warn "the router did not answer in time"
    return 1
}

cmd_status() {
    require_installed
    load_generated_config
    host="$(probe_host "$BIND_ADDR")"
    log "supervisor ($(effective_supervisor))"
    supervisor_status "$(effective_supervisor)"
    echo
    log "service readiness"
    curl -fsS -m 10 "http://$host:$BIND_PORT/health/ready" 2>/dev/null \
        || echo "  unreachable at http://$host:$BIND_PORT/health/ready"
    echo
    if [ "$WITH_ROUTER" = "yes" ] && [ -s "$ROUTER_SECRET_FILE" ]; then
        log "router MCP tools"
        curl -fsS -m 10 -X POST "http://$(probe_host "$ROUTER_BIND_ADDR"):$ROUTER_PORT/mcp" \
            -H 'Content-Type: application/json' \
            -H 'Accept: application/json, text/event-stream' \
            -H 'MCP-Protocol-Version: 2025-06-18' \
            -H "Authorization: Bearer $(read_secret "$ROUTER_SECRET_FILE")" \
            -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' 2>/dev/null \
            | grep -o '"name":"[a-z_]*"' | sort -u | sed 's/^/  /' \
            || echo "  unreachable"
        echo
    fi
    log "database"
    if [ -f "$DATA_DIR/agentrecall.duckdb" ]; then
        du -h "$DATA_DIR/agentrecall.duckdb" 2>/dev/null | awk '{print "  " $1 "  " $2}'
    else
        echo "  no database yet"
    fi
    echo
    log "backups"
    # Backup names start with a sortable timestamp, so the last entries of the
    # sorted list are the most recent ones.
    find "$BACKUP_DIR" -maxdepth 1 -type f -name '*.duckdb' 2>/dev/null \
        | sort | tail -5 | sed 's/^/  /' || echo "  none"
}

cmd_backup() {
    require_installed
    BACKUP_SUPERVISOR="$(effective_supervisor)"
    case "$BACKUP_SUPERVISOR" in
        systemd) need systemctl ;;
        launchd) need launchctl ;;
        none) die "backup requires a managed launchd/systemd service; stop an unmanaged service and run the binary's backup command directly" ;;
        *) die "unsupported supervisor for backup: $BACKUP_SUPERVISOR" ;;
    esac

    if supervisor_component_active "$BACKUP_SUPERVISOR" service; then
        BACKUP_SERVICE_WAS_ACTIVE="yes"
    else
        active_status=$?
        [ "$active_status" -eq 1 ] || die "could not determine whether the service is active"
    fi
    if supervisor_component_active "$BACKUP_SUPERVISOR" router; then
        BACKUP_ROUTER_WAS_ACTIVE="yes"
    else
        active_status=$?
        [ "$active_status" -eq 1 ] || die "could not determine whether the router is active"
    fi

    BACKUP_RESTORE_PENDING="yes"
    trap 'backup_exit_cleanup' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    log "stopping the managed service so DuckDB can be copied safely"
    if ! supervisor_stop_for_backup; then
        die "could not stop the deployment for backup"
    fi
    if [ "$BACKUP_SERVICE_WAS_ACTIVE" = "yes" ] || [ "$BACKUP_ROUTER_WAS_ACTIVE" = "yes" ]; then
        sleep 3
    fi
    stamp="$(date +%Y%m%d-%H%M%S)"
    BACKUP_OUTPUT_FILE="$BACKUP_DIR/agentrecall-$stamp.duckdb"
    BACKUP_VERIFY_FILE="$BACKUP_DIR/.verify-$stamp.duckdb"
    "$BIN" backup --database "$DATA_DIR/agentrecall.duckdb" --output "$BACKUP_OUTPUT_FILE" \
        || die "backup failed"
    log "verifying the backup"
    "$BIN" restore --backup "$BACKUP_OUTPUT_FILE" --verify --database "$BACKUP_VERIFY_FILE" \
        || die "the backup did not verify"
    step "verified $BACKUP_OUTPUT_FILE"
}

cmd_env() {
    require_installed
    load_generated_config
    log "Hermes wiring"
    if [ "$WITH_ROUTER" = "yes" ]; then
        printf '  AGENTRECALL_MCP_URL=http://%s:%s/mcp\n' "$(probe_host "$ROUTER_BIND_ADDR")" "$ROUTER_PORT"
        printf '  AGENTRECALL_MCP_TOKEN=<the contents of %s>\n' "$ROUTER_SECRET_FILE"
    else
        printf '  AGENTRECALL_MCP_URL=http://%s:%s/mcp\n' "$(probe_host "$BIND_ADDR")" "$BIND_PORT"
        printf '  AGENTRECALL_MCP_TOKEN=<the contents of %s>\n' "$SERVICE_SECRET_FILE"
    fi
    printf '  AGENTRECALL_NAMESPACE=%s\n' "$NAMESPACES"
    echo
    log "files"
    printf '  binary   %s\n' "$BIN"
    printf '  database %s\n' "$DATA_DIR/agentrecall.duckdb"
    printf '  tokens   %s\n' "$TOKENS_FILE"
    printf '  env      %s\n' "$ENV_FILE"
    printf '  logs     %s\n' "$LOG_DIR"
}

cmd_uninstall() {
    require_installed
    if [ "$PURGE" = "yes" ]; then
        validate_prefix_directory
    fi
    log "stopping the deployment"
    # Stop the recorded supervisor and the detected one: a deployment may have
    # been installed with one supervisor and later reinstalled with another.
    recorded="$(effective_supervisor)"
    detected="$(detect_supervisor)"
    supervisor_stop "$recorded"
    if [ "$recorded" != "$detected" ]; then
        supervisor_stop "$detected"
    fi
    # Unit files of both supervisors are removed: a deployment may have been
    # installed with one supervisor and later reinstalled with the other.
    if [ -f "$LAUNCHD_DIR/$SERVICE_LABEL.plist" ] || [ -f "$LAUNCHD_DIR/$ROUTER_LABEL.plist" ]; then
        rm -f "$LAUNCHD_DIR/$SERVICE_LABEL.plist" "$LAUNCHD_DIR/$ROUTER_LABEL.plist"
        step "removed the launchd plists"
    fi
    if [ -f "$SYSTEMD_DIR/$SERVICE_UNIT" ] || [ -f "$SYSTEMD_DIR/$ROUTER_UNIT" ]; then
        rm -f "$SYSTEMD_DIR/$SERVICE_UNIT" "$SYSTEMD_DIR/$ROUTER_UNIT"
        systemctl --user daemon-reload 2>/dev/null || true
        step "removed the systemd units"
    fi
    if [ "$PURGE" = "yes" ]; then
        warn "deleting $PREFIX, including the database and all secrets"
        rm -rf "$PREFIX"
    else
        step "kept $PREFIX (the database and secrets stay); use --purge to delete"
    fi
}

# -------------------------------------------------------------------- main --

WITH_ROUTER="yes"
BIND_ADDR_OVERRIDE=""
ROUTER_BIND_ADDR_OVERRIDE=""
WITH_ROUTER_OVERRIDE=""
LABEL_PREFIX_OVERRIDE=""
NAMESPACES_OVERRIDE=""
BIN_DIR="$PREFIX/bin"
PURGE="no"

# The first bare word is the command; everything after it is an option.
COMMAND=""
if [ "$#" -gt 0 ] && [ "${1#-}" = "$1" ]; then
    COMMAND="$1"
    shift
fi

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --bind) BIND_ADDR="$2"; BIND_ADDR_OVERRIDE="$2"; shift 2 ;;
        --router-bind) ROUTER_BIND_ADDR="$2"; ROUTER_BIND_ADDR_OVERRIDE="$2"; shift 2 ;;
        --owner) OWNER_ID="$2"; shift 2 ;;
        --client) CLIENT_ID="$2"; shift 2 ;;
        --scopes) SCOPES="$2"; shift 2 ;;
        --namespaces) NAMESPACES="$2"; NAMESPACES_OVERRIDE="$2"; shift 2 ;;
        --quota) QUOTA_PER_OWNER="$2"; shift 2 ;;
        --label-prefix) LABEL_PREFIX="$2"; LABEL_PREFIX_OVERRIDE="$2"; shift 2 ;;
        --supervisor) SUPERVISOR="$2"; shift 2 ;;
        --no-router) WITH_ROUTER="no"; WITH_ROUTER_OVERRIDE="no"; shift ;;
        --no-embeddings) USE_EMBEDDINGS="no"; shift ;;
        --no-extensions) PROVISION_EXTENSIONS="no"; shift ;;
        --binary) BINARY="$2"; shift 2 ;;
        --skip-build) SKIP_BUILD="yes"; shift ;;
        --purge) PURGE="yes"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

# The derived paths depend on --prefix, so recompute them after parsing.
BIN_DIR="$PREFIX/bin"
BIN="$BIN_DIR/agentrecall"
CONFIG_DIR="$PREFIX/config"
DATA_DIR="$PREFIX/data"
LOG_DIR="$PREFIX/logs"
BACKUP_DIR="$PREFIX/backups"
TOKENS_FILE="$CONFIG_DIR/tokens.json"
ENV_FILE="$CONFIG_DIR/env"
CURSOR_SECRET_FILE="$CONFIG_DIR/cursor_hmac.secret"
ROUTER_SECRET_FILE="$CONFIG_DIR/router_ingress.secret"
SERVICE_SECRET_FILE="$CONFIG_DIR/local_service.secret"
SUPERVISOR_STATE_FILE="$CONFIG_DIR/supervisor"
if [ "$COMMAND" != "install" ]; then
    load_deployment_options
fi
if [ "$COMMAND" != "status" ] && [ "$COMMAND" != "env" ]; then
    validate_loopback_bind "service" "$BIND_ADDR"
    validate_loopback_bind "router" "$ROUTER_BIND_ADDR"
    case "$WITH_ROUTER" in yes|no) ;; *) die "invalid saved router setting: $WITH_ROUTER" ;; esac
fi
case "$LABEL_PREFIX" in
    ''|*[!A-Za-z0-9._-]*) die "label prefix may contain only letters, numbers, dot, underscore, and hyphen" ;;
esac
case "$SUPERVISOR" in auto|launchd|systemd|none) ;; *) die "unsupported supervisor: $SUPERVISOR" ;; esac
BIND_PORT="${BIND_ADDR##*:}"
ROUTER_PORT="${ROUTER_BIND_ADDR##*:}"
SERVICE_LABEL="$LABEL_PREFIX.service"
ROUTER_LABEL="$LABEL_PREFIX.router"
SERVICE_UNIT="$LABEL_PREFIX.service"
ROUTER_UNIT="$LABEL_PREFIX.router.service"

case "$COMMAND" in
    install) cmd_install ;;
    status) cmd_status ;;
    backup) cmd_backup ;;
    env) cmd_env ;;
    uninstall) cmd_uninstall ;;
    ""|-h|--help) usage ;;
    *) usage; die "unknown command: $COMMAND" ;;
esac
