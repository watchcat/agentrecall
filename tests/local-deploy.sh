#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEPLOY_SCRIPT="$REPO_ROOT/scripts/local-deploy.sh"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/agentrecall-deploy-test.XXXXXX")"
MOCK_BIN="$TEST_ROOT/mock-bin"
FAKE_BINARY="$TEST_ROOT/agentrecall"
mkdir -p "$MOCK_BIN"

cleanup() {
    rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

fail() {
    printf 'not ok: %s\n' "$*" >&2
    exit 1
}

pass() {
    printf 'ok: %s\n' "$*"
}

cat > "$FAKE_BINARY" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
    --help) exit 0 ;;
    create-token) printf '%064d\n' 0 ;;
    backup)
        if [ "${FAKE_BACKUP_FAIL:-0}" = "1" ]; then
            exit 23
        fi
        output=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --output) output="$2"; shift 2 ;;
                *) shift ;;
            esac
        done
        mkdir -p "$(dirname "$output")"
        printf 'backup fixture\n' > "$output"
        ;;
    restore)
        if [ "${FAKE_RESTORE_FAIL:-0}" = "1" ]; then
            exit 24
        fi
        database=""
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --database) database="$2"; shift 2 ;;
                *) shift ;;
            esac
        done
        mkdir -p "$(dirname "$database")"
        printf 'verified fixture\n' > "$database"
        ;;
    *) exit 0 ;;
esac
EOF
chmod 755 "$FAKE_BINARY"

cat > "$MOCK_BIN/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
active_file="${FAKE_ACTIVE_UNITS:?FAKE_ACTIVE_UNITS is required}"
touch "$active_file"
[ "${1:-}" = "--user" ] || exit 1
shift
command_name="${1:-}"
[ "$#" -eq 0 ] || shift
unit=""
for arg in "$@"; do
    case "$arg" in --now|--quiet) ;; *) unit="$arg" ;; esac
done
set_unit_state() {
    target="$1"
    state="$2"
    temporary="${active_file}.tmp.$$"
    : > "$temporary"
    while IFS= read -r existing; do
        [ "$existing" = "$target" ] || printf '%s\n' "$existing" >> "$temporary"
    done < "$active_file"
    if [ "$state" = "active" ]; then
        printf '%s\n' "$target" >> "$temporary"
    fi
    mv "$temporary" "$active_file"
}
case "$command_name" in
    show-environment|daemon-reload) exit 0 ;;
    is-active)
        if grep -Fqx "$unit" "$active_file"; then exit 0; else exit 3; fi
        ;;
    enable)
        case " $* " in *" --now "*) set_unit_state "$unit" active ;; esac
        ;;
    start) set_unit_state "$unit" active ;;
    stop|disable) set_unit_state "$unit" inactive ;;
    *) exit 0 ;;
esac
EOF
chmod 755 "$MOCK_BIN/systemctl"

cat > "$MOCK_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
request="$*"
if [ -n "${FAKE_CURL_LOG:-}" ]; then
    printf '%s\n' "$request" >> "$FAKE_CURL_LOG"
fi
case "${FAKE_CURL_MODE:-ready}" in
    service-fails) exit 22 ;;
    router-fails)
        case "$request" in *"/health/ready"*) exit 0 ;; *) exit 22 ;; esac
        ;;
    ready) exit 0 ;;
    *) exit 22 ;;
esac
EOF
chmod 755 "$MOCK_BIN/curl"

cat > "$MOCK_BIN/sleep" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod 755 "$MOCK_BIN/sleep"

create_prefix() {
    target="$1"
    supervisor="$2"
    router="$3"
    mkdir -p "$target/bin" "$target/config" "$target/data" "$target/logs" "$target/backups"
    cp "$FAKE_BINARY" "$target/bin/agentrecall"
    chmod 755 "$target/bin/agentrecall"
    {
        printf '%s\n' "$supervisor"
        printf 'label_prefix=com.agentrecall\n'
        printf 'router=%s\n' "$router"
        printf 'bind_addr=127.0.0.1:8789\n'
        printf 'router_bind_addr=127.0.0.1:8788\n'
        printf 'namespaces=shared\n'
    } > "$target/config/supervisor"
}

# Both service and router binds must be loopback-only and rejected before install creates its prefix.
for bind_kind in service router; do
    case "$bind_kind" in
        service) bind_args=(--bind 0.0.0.0:8789) ;;
        router) bind_args=(--router-bind '::1:8788') ;;
    esac
    prefix="$TEST_ROOT/invalid-${bind_kind}-bind"
    home="$TEST_ROOT/home-invalid-bind"
    mkdir -p "$home"
    if env HOME="$home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/invalid-bind.active" \
        "$DEPLOY_SCRIPT" install --prefix "$prefix" --binary "$FAKE_BINARY" --no-extensions \
        --supervisor none "${bind_args[@]}" > "$TEST_ROOT/invalid-${bind_kind}-bind.log" 2>&1; then
        fail "accepted non-loopback $bind_kind bind"
    fi
    [ ! -e "$prefix" ] || fail "created files before rejecting the $bind_kind bind"
done
pass "non-loopback service and router binds are rejected before installation side effects"

# Explicitly paired installations must not be silently turned into single-node ones.
paired_prefix="$TEST_ROOT/paired-prefix"
paired_home="$TEST_ROOT/home-paired"
mkdir -p "$paired_prefix/config" "$paired_home"
printf 'AGENTRECALL_PEER_TOKEN=synthetic-private-pairing-token\n' > "$paired_prefix/config/replication.env"
cp "$paired_prefix/config/replication.env" "$TEST_ROOT/paired-before.env"
if env HOME="$paired_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/paired.active" \
    "$DEPLOY_SCRIPT" install --prefix "$paired_prefix" --binary "$FAKE_BINARY" --no-extensions \
    --supervisor none > "$TEST_ROOT/paired.log" 2>&1; then
    fail "silently replaced a paired installation"
fi
cmp -s "$paired_prefix/config/replication.env" "$TEST_ROOT/paired-before.env" || fail "changed pairing configuration"
[ ! -e "$paired_prefix/bin" ] || fail "changed paired prefix before rejecting single-node install"
if grep -q 'synthetic-private-pairing-token' "$TEST_ROOT/paired.log"; then
    fail "leaked pairing token in install rejection"
fi
pass "paired deployment reinstall is refused without side effects or secret output"

# A broad prefix such as ~/.local/share must not be recursively purged.
purge_home="$TEST_ROOT/home-purge"
broad_prefix="$purge_home/.local/share"
mkdir -p "$purge_home" "$broad_prefix/bin" "$broad_prefix/config" \
    "$broad_prefix/data" "$broad_prefix/logs" "$broad_prefix/backups"
cp "$FAKE_BINARY" "$broad_prefix/bin/agentrecall"
printf 'none\n' > "$broad_prefix/config/supervisor"
printf 'preserve this unrelated marker\n' > "$broad_prefix/data/keep.txt"
if env HOME="$purge_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/purge.active" \
    "$DEPLOY_SCRIPT" uninstall --prefix "$broad_prefix" --purge > "$TEST_ROOT/purge.log" 2>&1; then
    fail "purged a shared home directory"
fi
[ -f "$broad_prefix/data/keep.txt" ] || fail "purge safety check ran after deletion"
pass "purge refuses a shared prefix and preserves its contents"

# The selected non-default settings should be used by later env invocations.
state_home="$TEST_ROOT/home-state"
state_prefix="$TEST_ROOT/state-prefix"
mkdir -p "$state_home"
env HOME="$state_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/state.active" \
    "$DEPLOY_SCRIPT" install --prefix "$state_prefix" --binary "$FAKE_BINARY" --no-extensions \
    --no-embeddings --no-router --supervisor none --bind 127.0.0.1:8890 --namespaces shared,dev \
    > "$TEST_ROOT/state-install.log" 2>&1
{
    printf 'none\n'
    printf 'label_prefix=com.agentrecall\n'
    printf 'router=yes\n'
    printf 'bind_addr=127.0.0.1:8789\n'
    printf 'router_bind_addr=127.0.0.1:8788\n'
    printf 'namespaces=shared,dev\n'
} > "$state_prefix/config/supervisor"
printf 'stale router secret fixture\n' > "$state_prefix/config/router_ingress.secret"
env HOME="$state_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/state.active" \
    "$DEPLOY_SCRIPT" env --prefix "$state_prefix" > "$TEST_ROOT/state-env.log" 2>&1
grep -Fq 'AGENTRECALL_MCP_URL=http://127.0.0.1:8890/mcp' "$TEST_ROOT/state-env.log" \
    || fail "env did not reuse the saved service bind"
grep -Fq 'AGENTRECALL_NAMESPACE=shared,dev' "$TEST_ROOT/state-env.log" \
    || fail "env did not reuse the saved namespace"
if grep -Fq 'router_ingress.secret' "$TEST_ROOT/state-env.log"; then
    fail "env selected the router even though the generated configuration disables it"
fi
env HOME="$state_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/state.active" \
    FAKE_CURL_LOG="$TEST_ROOT/state-status-curl.log" \
    "$DEPLOY_SCRIPT" status --prefix "$state_prefix" > "$TEST_ROOT/state-status.log" 2>&1
grep -Fq 'http://127.0.0.1:8890/health/ready' "$TEST_ROOT/state-status-curl.log" \
    || fail "status did not probe the generated service bind"
if grep -Fq '8789' "$TEST_ROOT/state-status-curl.log" || grep -Fq 'router MCP tools' "$TEST_ROOT/state-status.log"; then
    fail "status used stale service/router metadata instead of the generated configuration"
fi
pass "status and env read the generated custom bind and no-router configuration"

systemd_path_expected() {
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

# Unit directive paths and EnvironmentFile values must survive systemd parsing.
unit_home="$TEST_ROOT/home-systemd-path"
unit_prefix="$TEST_ROOT/prefix with space %percent \$dollar \"quoted\" \\backslash"
unit_active="$TEST_ROOT/systemd-path.active"
mkdir -p "$unit_home"
: > "$unit_active"
env HOME="$unit_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$unit_active" \
    "$DEPLOY_SCRIPT" install --prefix "$unit_prefix" --binary "$FAKE_BINARY" \
    --no-extensions --no-embeddings --supervisor systemd > "$TEST_ROOT/systemd-path.log" 2>&1
unit_file="$unit_home/.config/systemd/user/com.agentrecall.service"
unit_escaped_prefix="$(systemd_path_expected "$unit_prefix")"
unit_exec_prefix="${unit_prefix//\\/\\\\}"
unit_exec_prefix="${unit_exec_prefix//\"/\\\"}"
unit_exec_prefix="${unit_exec_prefix//%/%%}"
grep -Fqx "EnvironmentFile=$unit_escaped_prefix/config/env" "$unit_file" \
    || fail "systemd EnvironmentFile path was not escaped"
grep -Fqx "WorkingDirectory=$unit_escaped_prefix" "$unit_file" \
    || fail "systemd WorkingDirectory path was not escaped"
grep -Fqx "ExecStart=:/usr/bin/env \"$unit_exec_prefix/bin/agentrecall\" serve" "$unit_file" \
    || fail "systemd ExecStart path was not quoted and escaped"
grep -Fqx "StandardOutput=append:$unit_escaped_prefix/logs/serve.log" "$unit_file" \
    || fail "systemd StandardOutput path was not escaped"
grep -Fqx "StandardError=append:$unit_escaped_prefix/logs/serve.error.log" "$unit_file" \
    || fail "systemd StandardError path was not escaped"
env_escaped_prefix="${unit_prefix//\\/\\\\}"
env_escaped_prefix="${env_escaped_prefix//\"/\\\"}"
env_escaped_prefix="${env_escaped_prefix//\$/\\\$}"
grep -Fqx "AGENTRECALL_DATABASE_PATH=\"$env_escaped_prefix/data/agentrecall.duckdb\"" \
    "$unit_prefix/config/env" || fail "generated EnvironmentFile did not escape its custom database path"
if command -v systemd-analyze >/dev/null 2>&1; then
    if ! systemd-analyze --user verify "$unit_file" > "$TEST_ROOT/systemd-verify.log" 2>&1; then
        cat "$TEST_ROOT/systemd-verify.log" >&2
        fail "systemd-analyze rejected the generated custom-path unit"
    fi
fi
pass "systemd unit and environment paths are escaped"

run_readiness_failure() {
    name="$1"
    curl_mode="$2"
    router_option="$3"
    router_args=()
    [ "$router_option" = "--no-router" ] && router_args+=(--no-router)
    home="$TEST_ROOT/home-$name"
    prefix="$TEST_ROOT/prefix-$name"
    active_file="$TEST_ROOT/$name.active"
    mkdir -p "$home"
    : > "$active_file"
    if env HOME="$home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$active_file" FAKE_CURL_MODE="$curl_mode" \
        "$DEPLOY_SCRIPT" install --prefix "$prefix" --binary "$FAKE_BINARY" --no-extensions \
        --no-embeddings --supervisor systemd "${router_args[@]}" > "$TEST_ROOT/$name.log" 2>&1; then
        fail "$name install unexpectedly succeeded"
    fi
    [ ! -s "$active_file" ] || fail "$name readiness failure left systemd units active"
}

run_readiness_failure service-readiness-failure service-fails --no-router
pass "service readiness failure stops the started service"
run_readiness_failure router-readiness-failure router-fails ""
pass "router readiness failure stops both started services"

# A failed backup must restore the prior active state rather than leave units stopped.
backup_home="$TEST_ROOT/home-backup"
backup_prefix="$TEST_ROOT/prefix-backup"
backup_active="$TEST_ROOT/backup.active"
mkdir -p "$backup_home"
create_prefix "$backup_prefix" systemd no
printf 'com.agentrecall.service\n' > "$backup_active"
if env HOME="$backup_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$backup_active" FAKE_BACKUP_FAIL=1 \
    "$DEPLOY_SCRIPT" backup --prefix "$backup_prefix" > "$TEST_ROOT/backup-failure.log" 2>&1; then
    fail "injected backup failure unexpectedly succeeded"
fi
grep -Fqx 'com.agentrecall.service' "$backup_active" || fail "service was not restored after backup failure"
pass "backup command failure restores the previously active service"

# A failed verification follows the same restoration path and removes its temporary output.
printf 'com.agentrecall.service\n' > "$backup_active"
if env HOME="$backup_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$backup_active" FAKE_RESTORE_FAIL=1 \
    "$DEPLOY_SCRIPT" backup --prefix "$backup_prefix" > "$TEST_ROOT/verify-failure.log" 2>&1; then
    fail "injected backup verification failure unexpectedly succeeded"
fi
grep -Fqx 'com.agentrecall.service' "$backup_active" || fail "service was not restored after verification failure"
if find "$backup_prefix/backups" -maxdepth 1 -name '.verify-*.duckdb' -print | grep -q .; then
    fail "verification failure left temporary database files"
fi
pass "verification failure restores the service and removes temporary database files"

# Without a supervisor the script cannot stop a manually launched process safely.
manual_prefix="$TEST_ROOT/prefix-manual"
create_prefix "$manual_prefix" none no
if env HOME="$backup_home" PATH="$MOCK_BIN:$PATH" FAKE_ACTIVE_UNITS="$TEST_ROOT/manual.active" \
    "$DEPLOY_SCRIPT" backup --prefix "$manual_prefix" > "$TEST_ROOT/manual-backup.log" 2>&1; then
    fail "backup unexpectedly proceeded with supervisor none"
fi
grep -Fq 'backup requires a managed launchd/systemd service' "$TEST_ROOT/manual-backup.log" \
    || fail "unmanaged backup refusal did not explain the limitation"
pass "backup refuses unmanaged service mode"

printf 'All local deployment safety tests passed.\n'
