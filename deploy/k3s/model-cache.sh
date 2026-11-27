#!/usr/bin/env bash
# Uses only public synthetic fixtures and a disposable /tmp database.
set +x
set -euo pipefail
export LC_ALL=C
export NO_COLOR=1
CACHE=/models
REPO="$CACHE/$MODEL_CACHE_REPOSITORY"
SNAP="$REPO/snapshots/$MODEL_REVISION"
MARKER="$CACHE/.agentrecall-model.complete"
MANIFEST="$CACHE/.agentrecall-model.sha256"
MODE="${1:---verify-only}"
fail() {
    echo "Model cache/probe refused: $1" >&2
    if [[ -f /tmp/native.log ]]; then tail -n 30 /tmp/native.log >&2; fi
    exit 1
}

check_blob() {
    local file="$1" expected="$2" size="$3" actual
    [[ -f "$file" && "$(stat -Lc %s "$file")" == "$size" ]] || fail 'missing or wrong-sized pinned tokenizer asset'
    actual="$({ printf 'blob %s\0' "$size"; cat "$file"; } | sha1sum | cut -d' ' -f1)"
    [[ "$actual" == "$expected" ]] || fail 'pinned tokenizer Git blob hash mismatch'
}
verify_assets() {
    [[ -f "$REPO/refs/main" && "$(cat "$REPO/refs/main")" == "$MODEL_REVISION" ]] || fail 'model revision mismatch'
    local file resolved
    for file in model_quantized.onnx tokenizer.json config.json special_tokens_map.json tokenizer_config.json; do
        resolved="$(readlink -f "$SNAP/$file")"
        [[ "$resolved" == "$CACHE/"* && -f "$resolved" ]] || fail 'missing asset or cache symlink escape'
    done
    [[ "$(stat -Lc %s "$SNAP/model_quantized.onnx")" == "$MODEL_ONNX_SIZE" ]] || fail 'wrong model size'
    [[ "$(stat -Lc %s "$SNAP/tokenizer.json")" == "$MODEL_TOKENIZER_SIZE" ]] || fail 'wrong tokenizer size'
    printf '%s  %s\n%s  %s\n' "$MODEL_ONNX_SHA256" "$SNAP/model_quantized.onnx" "$MODEL_TOKENIZER_SHA256" "$SNAP/tokenizer.json" | sha256sum --strict -c - >/dev/null || fail 'upstream model/tokenizer SHA-256 mismatch'
    check_blob "$SNAP/config.json" "$MODEL_CONFIG_SHA1" "$MODEL_CONFIG_SIZE"
    check_blob "$SNAP/special_tokens_map.json" "$MODEL_SPECIAL_SHA1" "$MODEL_SPECIAL_SIZE"
    check_blob "$SNAP/tokenizer_config.json" "$MODEL_TOKENIZER_CONFIG_SHA1" "$MODEL_TOKENIZER_CONFIG_SIZE"
}
verify_cache() {
    [[ -f "$MARKER" && ! -L "$MARKER" && "$(cat "$MARKER")" == "$MODEL_CONTRACT" ]] || fail 'missing or mismatching completion marker'
    [[ -s "$MANIFEST" && ! -L "$MANIFEST" ]] || fail 'missing checksum manifest'
    verify_assets
    # The five snapshot files and main reference must match the completion manifest.
    (cd "$CACHE"; sha256sum --strict -c .agentrecall-model.sha256 >/dev/null) || fail 'cached content changed'
    echo "MODEL_CACHE_VERIFIED contract=$MODEL_CONTRACT revision=$MODEL_REVISION"
}
case "$MODE" in
    --verify-only) verify_cache; exit 0 ;;
    --probe-offline) verify_cache ;;
    --provision)
        if [[ -e "$MARKER" ]]; then verify_cache; exit 0; fi
        [[ -z "$(find "$CACHE" -mindepth 1 -maxdepth 1 -print -quit)" ]] || fail 'partial cache retained; inspect and explicitly recover it before retrying'
        ;;
    *) fail 'unsupported helper mode' ;;
esac

# Public, loopback-only synthetic credential. Never mount the live runtime Secret.
TOKEN=agentrecall-public-disposable-probe-token-not-a-live-credential
HASH="$(printf %s "$TOKEN" | sha256sum | cut -d' ' -f1)"
printf '[{"token_hash":"%s","owner_id":"semantic-probe","client_id":"semantic-probe","scopes":["memory:read","memory:write","memory:delete"],"allowed_namespaces":["shared"],"active":true}]' "$HASH" > /tmp/probe-tokens.json
export AGENTRECALL_DATABASE_PATH=/tmp/semantic-probe.duckdb
export AGENTRECALL_TOKENS_FILE=/tmp/probe-tokens.json
export AGENTRECALL_BIND_ADDR=127.0.0.1:8790
export AGENTRECALL_REPLICA_ID=semantic-probe
export AGENTRECALL_ROLE=cloud
export AGENTRECALL_ALLOWED_HOSTS=localhost,127.0.0.1
export AGENTRECALL_CURSOR_HMAC_SECRET=public-disposable-probe-cursor-not-live
export AGENTRECALL_EMBEDDING_CACHE_DIR="$CACHE"
PID=
cleanup() { if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then kill -INT "$PID"; wait "$PID" || true; fi; }
trap cleanup EXIT TERM INT
stop_native() { kill -INT "$PID"; wait "$PID"; PID=; }
start_native() {
    local profile="$1" deadline=$((SECONDS + 600))
    : > /tmp/native.log
    if [[ "$profile" == lexical ]]; then
        env -u AGENTRECALL_EMBEDDING_PROVIDER agentrecall serve > /tmp/native.log 2>&1 &
    else
        AGENTRECALL_EMBEDDING_PROVIDER=bge-m3 agentrecall serve > /tmp/native.log 2>&1 &
    fi
    PID=$!
    while ! grep -q 'HTTP service listening' /tmp/native.log; do
        kill -0 "$PID" 2>/dev/null || fail 'native initialization exited; inspect the synthetic Job log privately'
        (( SECONDS < deadline )) || fail 'native startup deadline exceeded'
        sleep 1
    done
    if [[ "$profile" == semantic ]]; then
        grep -q 'semantic_ready=true' /tmp/native.log || fail 'native process started with lexical fallback'
    fi
}
rpc() {
    local payload="$1"
    exec 3<>/dev/tcp/127.0.0.1/8790
    printf 'POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer %s\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-03-26\r\nContent-Length: %s\r\nConnection: close\r\n\r\n%s' "$TOKEN" "${#payload}" "$payload" >&3
    cat <&3 > /tmp/response
    exec 3<&- 3>&-
    grep -q 'HTTP/1.1 200' /tmp/response || fail 'loopback MCP HTTP failure'
    grep -q '"structuredContent"' /tmp/response || fail 'missing structured tool result'
    if grep -Eq '"isError"[[:space:]]*:[[:space:]]*true|"error"[[:space:]]*:' /tmp/response; then fail 'synthetic MCP tool failed'; fi
}
query_garden() {
    rpc "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"memory_search\",\"arguments\":{\"query\":\"$1\",\"limit\":1}}}"
    grep -Eq '"search_mode"[[:space:]]*:[[:space:]]*"hybrid"' /tmp/response || fail 'search fell back to lexical'
    grep -Fq "$GARDEN_ID" /tmp/response || fail 'expected semantic top result absent'
}
START=$SECONDS
start_native lexical
rpc '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"memory_save","arguments":{"content":"The elderly scientist cultivates roses in a botanical garden.","client_request_id":"probe-garden"}}}'
GARDEN_ID="$(grep -oE '"id":"[0-9a-f-]{36}"' /tmp/response | head -1 | cut -d'"' -f4)"
[[ -n "$GARDEN_ID" ]] || fail 'cannot identify synthetic saved record'
rpc '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_save","arguments":{"content":"Astronomers observe distant galaxies with a telescope.","client_request_id":"probe-space"}}}'
stop_native
SEMANTIC_START=$SECONDS
start_native semantic
STARTUP=$((SECONDS - SEMANTIC_START))
verify_assets
query_garden 'an old researcher raising flowers'
query_garden '老人喺花園種玫瑰'
rpc "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"memory_update\",\"arguments\":{\"memory_id\":\"$GARDEN_ID\",\"expected_revision\":1,\"content\":\"The researcher repairs bicycles in a workshop.\"}}}"
query_garden 'fixing a bike'
grep -q '"revision":2' /tmp/response || fail 'updated vector revision not fresh'
stop_native
RESTART_START=$SECONDS
start_native semantic
RESTART=$((SECONDS - RESTART_START))
query_garden 'fixing a bike'
grep -q '"revision":2' /tmp/response || fail 'updated record/vector not retained after restart'
rpc "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"memory_delete\",\"arguments\":{\"memory_id\":\"$GARDEN_ID\",\"expected_revision\":2}}}"
rpc '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"fixing a bike","limit":10}}}'
grep -Eq '"search_mode"[[:space:]]*:[[:space:]]*"hybrid"' /tmp/response || fail 'delete search fell back to lexical'
if grep -Fq "$GARDEN_ID" /tmp/response; then fail 'deleted record/vector remains searchable'; fi
stop_native
if [[ "$MODE" == --provision ]]; then
    (cd "$CACHE"; sha256sum "$MODEL_CACHE_REPOSITORY/refs/main" "$MODEL_CACHE_REPOSITORY/snapshots/$MODEL_REVISION/"{model_quantized.onnx,tokenizer.json,config.json,special_tokens_map.json,tokenizer_config.json}) > "$MANIFEST.tmp"
    mv "$MANIFEST.tmp" "$MANIFEST"
    printf %s "$MODEL_CONTRACT" > "$MARKER.tmp"
    mv "$MARKER.tmp" "$MARKER"
    verify_cache
fi
PEAK=unavailable
if [[ -r /sys/fs/cgroup/memory.peak ]]; then PEAK="$(cat /sys/fs/cgroup/memory.peak)"; fi
grep 'local BGE-M3 embedding provider configured' /tmp/native.log
echo "MODEL_PROBE_PASSED mode=$MODE contract=$MODEL_CONTRACT startup_seconds=$STARTUP restart_seconds=$RESTART total_seconds=$((SECONDS - START)) peak_bytes=$PEAK checks=backfill,paraphrase,cantonese,update,delete,restart"
