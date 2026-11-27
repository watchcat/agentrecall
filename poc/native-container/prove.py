#!/usr/bin/env python3
"""Local-only unchanged native AgentRecall proof; Python 3.9 standard library.

Exit 0: every required runtime check passed; 1: failed assertion; 2: blocked.
No cloud access, real memories, inherited service env, or global Docker cleanup.
"""
import argparse
import gzip
import hashlib
import http.client
import io
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid

MODULE = "poc-native-container"
PREFIX = "agentrecall-poc-native-"
LABEL = "agentrecall-poc-native-owned"
IMAGE = "agentrecall-poc:native"
BUILDER = "rust:1.100.0-trixie@sha256:1e626a07e55b67d7fe19de7a590519e0cf4b75cf3788b058fa75cf4d599d9b5a"
RUNTIME = "debian:trixie-slim@sha256:ab3b3c8c5159eebfca4376430132d53c4d425f869c765ecbe47177a030da575d"
ASSETS = {
    "cli": ("https://github.com/duckdb/duckdb/releases/download/v2.0.1/duckdb-cli-linux-amd64.tar.gz", "b676268be4d39e4e41a904442c9796c9914e645cc085c7505f048f2c992b5eca"),
    "fts": ("https://extensions.duckdb-backup.org/v2.0.1/linux_amd64/fts.duckdb_extension.gz", "50978aea0a8ef2599cefc87fed1cb65920362430a6cad0070d24946103605b3c"),
    "vss": ("https://extensions.duckdb-backup.org/v2.0.1/linux_amd64/vss.duckdb_extension.gz", "26ef35a7d52f4be1f4c4fdbde84f890b0da896e5318a871e14e546225c478c36"),
    "onnx": ("https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/x86_64-unknown-linux-gnu.tar.lzma2", "02aac0d3b4927026292088986d7a377c657c88761b1c068be217a67b35447678"),
}
REQUIRED = ("artifact_gate", "locked_build", "image_architecture", "nonroot", "native_libraries", "no_baked_credentials_models", "ready", "auth", "initialize", "tools_list", "save_search_get_delete", "recreation", "idempotent_result", "idempotent_metadata", "100_cycles", "rss", "cleanup")
ENV_KEYS = frozenset({"AGENTRECALL_BIND_ADDR", "AGENTRECALL_DATABASE_PATH", "AGENTRECALL_TOKENS_FILE", "AGENTRECALL_CURSOR_HMAC_SECRET", "AGENTRECALL_ALLOWED_HOSTS", "AGENTRECALL_ROLE", "AGENTRECALL_QUOTA_PER_OWNER"})


class Blocked(RuntimeError):
    pass


class Failed(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise Failed(message)


def require_status(actual, expected, operation):
    require(actual == expected, "%s: expected HTTP %s, got %s" % (operation, expected, actual))


def redact(value, secrets=()):
    """Redact recursively; never save HTTP headers, raw token CLI output or env."""
    if isinstance(value, dict):
        return {k: "[REDACTED]" if re.search(r"token|secret|authorization|password", k, re.I) else redact(v, secrets) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v, secrets) for v in value]
    if isinstance(value, str):
        for secret in sorted((s for s in secrets if s), key=len, reverse=True):
            value = value.replace(secret, "[REDACTED]")
        value = re.sub(r"(?i)(Bearer\s+)[^\s\"']+", r"\1[REDACTED]", value)
        value = re.sub(r"(?i)(Token \(shown once\):\s*)\S+", r"\1[REDACTED]", value)
    return value


def parse_rpc(payload, request_id):
    require(isinstance(payload, dict) and payload.get("jsonrpc") == "2.0", "invalid JSON-RPC envelope")
    require(payload.get("id") == request_id, "JSON-RPC response ID mismatch")
    require("error" not in payload, "JSON-RPC error response")
    require(isinstance(payload.get("result"), dict), "missing JSON-RPC result")
    return payload["result"]


def parse_tool(result):
    require(result.get("isError") is not True, "MCP tool returned isError=true")
    require(isinstance(result.get("structuredContent"), dict), "missing structured tool result")
    require("error" not in result["structuredContent"], "structured tool error")
    return result["structuredContent"]


def memory_identifier(memory):
    # domain::Memory serializes `id`; request DTOs and DeleteOutput use memory_id.
    require(isinstance(memory, dict) and isinstance(memory.get("id"), str), "Memory result missing id")
    try:
        uuid.UUID(memory["id"])
    except ValueError:
        raise Failed("Memory id is not a UUID") from None
    return memory["id"]


def require_idempotent_save(original, retried):
    # Store::save_record returns the ledger Memory and `false` on a retry.
    require(original.get("created") is True, "retained save was not created")
    require(retried == {"memory": original["memory"], "created": False}, "idempotent save did not return identical Memory and created=false")


def private_write(path, content):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    fd = os.open(str(path), flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(content)


def safe_output(root, requested):
    base = root / "target" / "cloud-poc"
    base.mkdir(parents=True, exist_ok=True)
    path = Path(requested)
    if not path.is_absolute():
        path = root / path
    if path.parent.resolve() != base.resolve() or not re.fullmatch(r"native-container(?:[-A-Za-z0-9_.]*)\.json", path.name) or path.is_symlink():
        raise Blocked("output must be a non-symlink target/cloud-poc/native-container*.json")
    return path


def percentiles(samples):
    if not samples:
        return {"count": 0, "p50_ms": None, "p95_ms": None}
    ordered = sorted(samples)
    return {"count": len(ordered), "p50_ms": ordered[max(0, math.ceil(len(ordered) * .50) - 1)], "p95_ms": ordered[max(0, math.ceil(len(ordered) * .95) - 1)]}


def download_verified(name, directory):
    url, expected = ASSETS[name]
    started = time.monotonic()
    chunks, total = [], 0
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            while True:
                if time.monotonic() - started > 180:
                    raise Blocked("%s artifact download exceeded 180s" % name)
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                total += len(chunk)
                if total > 128 * 1024 * 1024:
                    raise Blocked("%s artifact exceeds 128MiB limit" % name)
                chunks.append(chunk)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        # No response bodies, credential-bearing headers, or URLs supplied by user.
        raise Blocked("%s exact artifact unavailable: %s" % (name, str(error))) from None
    blob = b"".join(chunks)
    actual = hashlib.sha256(blob).hexdigest()
    if actual != expected:
        raise Blocked("%s exact artifact SHA256 mismatch (%s)" % (name, actual))
    if name == "cli":
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as archive:
            files = [m for m in archive.getmembers() if m.isfile() and Path(m.name).name == "duckdb"]
            if len(files) != 1:
                raise Blocked("CLI archive lacks one unambiguous duckdb executable")
            binary = archive.extractfile(files[0]).read()
        path = directory / "duckdb"
        path.write_bytes(binary)
        path.chmod(0o700)
    elif name != "onnx":
        binary = gzip.decompress(blob)
        if len(binary) > 128 * 1024 * 1024:
            raise Blocked("extension expanded size exceeds limit")
        (directory / (name + ".duckdb_extension")).write_bytes(binary)
    return {"url": url, "sha256": actual, "bytes": total}


class Docker:
    def __init__(self, run_id):
        self.run_id = run_id
        self.containers = set()
        self.networks = set()
        self.secrets = []
        self.environment = {k: os.environ[k] for k in ("PATH", "HOME", "TMPDIR") if k in os.environ}
        self.environment["DOCKER_CLI_HINTS"] = "false"

    def call(self, args, timeout=30, check=True, input_text=None, binary_output=False):
        require(isinstance(args, list) and all(isinstance(a, str) for a in args), "Docker arguments must be a string list")
        try:
            stdin = input_text.encode("utf-8") if binary_output and input_text is not None else input_text
            result = subprocess.run(["docker"] + args, shell=False, capture_output=True, text=not binary_output, env=self.environment, timeout=timeout, input=stdin)
        except subprocess.TimeoutExpired:
            raise Blocked("Docker %s timed out after %ss; cleanup will run" % (args[0], timeout)) from None
        except OSError as error:
            raise Blocked("Docker unavailable: %s" % error.__class__.__name__) from None
        if check and result.returncode:
            stderr = result.stderr.decode("utf-8", errors="replace") if isinstance(result.stderr, bytes) else result.stderr
            raise Blocked("Docker %s failed (%s): %s" % (args[0], result.returncode, redact(stderr[-2000:], self.secrets)))
        return result

    def owned_name(self, name):
        require(name.startswith(PREFIX + self.run_id + "-"), "refusing unowned Docker name")

    def run(self, name, args, command, image=IMAGE, detach=False, timeout=60):
        self.owned_name(name)
        self.containers.add(name)  # registered before launch, including timeout paths
        base = ["run", "--name", name, "--label", LABEL + "=" + self.run_id, "--platform", "linux/amd64", "--cpus", "2", "--memory", "2g", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true"]
        base += ["--detach"] if detach else ["--rm"]
        return self.call(base + args + [image] + command, timeout=timeout)

    def remove_owned(self, kind, name):
        self.owned_name(name)
        item = self.call([kind, "inspect", name], check=False)
        if item.returncode:
            # Only a precise not-found diagnostic counts as already removed.
            require("No such" in item.stderr or "not found" in item.stderr, "cannot verify cleanup object")
            return
        detail = json.loads(item.stdout)[0]
        labels = detail.get("Config", {}).get("Labels", {}) if kind == "container" else detail.get("Labels", {})
        require(labels.get(LABEL) == self.run_id, "ownership label mismatch during cleanup")
        self.call((["rm", "--force", name] if kind == "container" else ["network", "rm", name]))

    def cleanup(self):
        failures = []
        for kind, names in (("container", self.containers), ("network", self.networks)):
            for name in sorted(names):
                try:
                    self.remove_owned(kind, name)
                except (Blocked, Failed, ValueError) as error:
                    failures.append(redact(str(error), self.secrets))
        return failures


def env_values(cursor):
    return {"AGENTRECALL_BIND_ADDR": "0.0.0.0:8789", "AGENTRECALL_DATABASE_PATH": "/state/data/proof.duckdb", "AGENTRECALL_TOKENS_FILE": "/state/credentials/tokens.json", "AGENTRECALL_CURSOR_HMAC_SECRET": cursor, "AGENTRECALL_ALLOWED_HOSTS": "127.0.0.1,localhost", "AGENTRECALL_ROLE": "local", "AGENTRECALL_QUOTA_PER_OWNER": "1000"}


def env_file(path, values):
    require(set(values) == ENV_KEYS, "environment must exactly match approved allowlist")
    require(all("\n" not in v and "\r" not in v for v in values.values()), "env-file newline injection")
    private_write(path, "".join("%s=%s\n" % item for item in sorted(values.items())))


class Mcp:
    def __init__(self, port, token):
        self.url = "http://127.0.0.1:%d" % port
        self.token = token
        self.request_id = 0
        self.last_request = 0.0
        # Never honor inherited proxy settings for private bearer requests.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self.samples = {name: [] for name in ("initialize", "tools/list", "memory_save", "memory_search", "memory_get", "memory_delete")}

    def http(self, path, payload=None, token=None, protocol=True):
        headers = {"Accept": "application/json, text/event-stream"}
        if payload is not None:
            headers["Content-Type"] = "application/json"
        if token is not None:
            headers["Authorization"] = "Bearer " + token
        if protocol:
            headers["mcp-protocol-version"] = "2025-03-26"
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(self.url + path, data=data, headers=headers)
        try:
            with self.opener.open(request, timeout=10) as response:
                return response.status, json.loads(response.read(2 * 1024 * 1024))
        except urllib.error.HTTPError as error:
            body = error.read(2 * 1024 * 1024)
            try:
                parsed = json.loads(body)
            except ValueError:
                parsed = {}
            return error.code, parsed

    def rpc(self, method, params):
        # Source RateLimiter is 120 requests/minute per credential. Pace below it.
        time.sleep(max(0, .56 - (time.monotonic() - self.last_request)))
        self.request_id += 1
        payload = {"jsonrpc": "2.0", "id": self.request_id, "method": method, "params": params}
        self.last_request = time.monotonic()
        start = time.perf_counter()
        status, body = self.http("/mcp", payload, self.token, protocol=method != "initialize")
        elapsed = (time.perf_counter() - start) * 1000
        name = params.get("name", method)
        self.samples.setdefault(name, []).append(elapsed)
        require_status(status, 200, method)
        return parse_rpc(body, self.request_id)

    def tool(self, name, arguments):
        return parse_tool(self.rpc("tools/call", {"name": name, "arguments": arguments}))

    def initialize(self):
        result = self.rpc("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "native-container-poc", "version": "0.1.0"}})
        require(isinstance(result.get("serverInfo", {}).get("name"), str), "initialize missing server identity")
        return result

    def ready(self, timeout=60):
        started = time.monotonic()
        while time.monotonic() - started < timeout:
            try:
                status, body = self.http("/health/ready", protocol=False)
                if status == 200 and body.get("status") == "ready":
                    return body, time.monotonic() - started
            except (urllib.error.URLError, OSError, ValueError):
                pass
            time.sleep(.25)
        raise Blocked("service readiness exceeded %ss" % timeout)


class ContainerLoopbackMcp(Mcp):
    """Actual HTTP over Docker exec stdin, with no host port or external route.

    Docker 29 suppresses published ports on internal networks. Keeping that
    isolation is more important than direct host networking. Latency includes
    Docker-exec/relay overhead; credentials never enter process arguments.
    """

    def __init__(self, docker, name, token):
        super().__init__(8789, token)
        docker.owned_name(name)
        self.docker = docker
        self.name = name

    def http(self, path, payload=None, token=None, protocol=True):
        require(path in ("/mcp", "/health/ready"), "unapproved relay path")
        body = b"" if payload is None else json.dumps(payload).encode("utf-8")
        headers = {"Host": "127.0.0.1:8789", "Accept": "application/json, text/event-stream", "Connection": "close", "Content-Length": str(len(body))}
        if payload is not None:
            headers["Content-Type"] = "application/json"
        if token is not None:
            require("\r" not in token and "\n" not in token, "relay header injection")
            headers["Authorization"] = "Bearer " + token
        if protocol:
            headers["mcp-protocol-version"] = "2025-03-26"
        request = ("%s %s HTTP/1.1\r\n" % ("GET" if payload is None else "POST", path)) + "".join("%s: %s\r\n" % item for item in headers.items()) + "\r\n" + body.decode("utf-8")
        relay = "exec 3<>/dev/tcp/127.0.0.1/8789 || exit 1; cat >&3; cat <&3"
        result = self.docker.call(["exec", "-i", self.name, "timeout", "10", "bash", "-c", relay], timeout=15, check=False, input_text=request, binary_output=True)
        if result.returncode:
            raise urllib.error.URLError("container-loopback HTTP relay unavailable")
        class ResponseSocket:
            def makefile(self, mode):
                return io.BytesIO(result.stdout)
        response = http.client.HTTPResponse(ResponseSocket())
        try:
            response.begin()
            raw = response.read(2 * 1024 * 1024 + 1)
            require(len(raw) <= 2 * 1024 * 1024, "relay response exceeds size limit")
            try:
                parsed = json.loads(raw)
            except ValueError:
                if response.status == 200:
                    raise
                parsed = {}
            return response.status, parsed
        finally:
            response.close()


def check(report, name, detail=None, status="passed"):
    report["checks"][name] = {"status": status, "detail": detail}


def artifact_gate(docker, directory, report):
    assets = {name: download_verified(name, directory) for name in ASSETS}
    report["artifacts"]["downloads"] = assets
    uid = os.getuid()
    if uid == 0:
        raise Blocked("runner requires a non-root host UID for private mounts")
    name = PREFIX + docker.run_id + "-gate"
    sql = "SELECT version() AS version; PRAGMA platform; LOAD '/gate/fts.duckdb_extension'; LOAD '/gate/vss.duckdb_extension'; SELECT extension_name, extension_version, loaded FROM duckdb_extensions() WHERE extension_name IN ('fts','vss');"
    output = docker.run(name, ["--network", "none", "--user", "%d:%d" % (uid, os.getgid()), "--mount", "type=bind,src=%s,dst=/gate,readonly" % directory], ["/gate/duckdb", "-json", "-c", sql], image=RUNTIME, timeout=120)
    # CLI prints one independent JSON array per statement.
    require('"v2.0.1"' in output.stdout and '"linux_amd64"' in output.stdout, "wrong exact DuckDB platform/version")
    require('"fts"' in output.stdout and '"vss"' in output.stdout and output.stdout.count('"loaded":true') == 2, "signed FTS/VSS load did not succeed")
    report["artifacts"]["signed_cli_gate"] = output.stdout.strip()
    toolchain = docker.run(PREFIX + docker.run_id + "-rust", ["--network", "none"], ["rustc", "--version"], image=BUILDER, timeout=180)
    require(toolchain.stdout.startswith("rustc 1.100.0 "), "exact Rust 1.100.0 unavailable")
    report["artifacts"]["rustc"] = toolchain.stdout.strip()
    check(report, "artifact_gate", "Exact AMD64 CLI, normal signed FTS/VSS load offline, pinned ONNX SHA256 and Rust verified")


def build_image(docker, root, directory, report):
    # O_EXCL host lock; never remove someone else's lock or run a second heavy build.
    lock = root / "target" / "cloud-poc" / "native-container-build.lock"
    try:
        fd = os.open(str(lock), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        raise Blocked("native build lock exists; another heavy build may be active") from None
    start = time.monotonic()
    process = None
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(docker.run_id + "\n")
        free = shutil.disk_usage(root).free
        report["measurements"]["disk_free_before_build_bytes"] = free
        if free < 12 * 1024 ** 3:
            raise Blocked("less than 12GiB free; refusing heavy native build without escalation")
        args = ["docker", "buildx", "build", "--platform", "linux/amd64", "--load", "--progress", "plain", "-f", "poc/native-container/Dockerfile", "-t", IMAGE, "."]
        log = directory / "build.log"
        with log.open("w", encoding="utf-8") as handle:
            try:
                process = subprocess.Popen(args, cwd=str(root), stdout=handle, stderr=subprocess.STDOUT, env=docker.environment, start_new_session=True)
            except OSError:
                raise Blocked("cannot start bounded Docker build") from None
            deadline = start + 45 * 60
            last_progress = 0
            while process.poll() is None:
                now = time.monotonic()
                if now >= deadline:
                    process.terminate()
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=15)
                    raise Blocked("native Docker build exceeded 45-minute bound; client terminated")
                if now - last_progress >= 30:
                    print("native build running: %.0fs elapsed, private log %s bytes" % (now - start, log.stat().st_size), flush=True)
                    last_progress = now
                time.sleep(1)
        report["measurements"]["build_seconds"] = time.monotonic() - start
        report["measurements"]["disk_free_after_build_bytes"] = shutil.disk_usage(root).free
        # Build context contains no credentials; still retain only a sanitized bounded tail.
        lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
        report["artifacts"]["build_returncode"] = process.returncode
        report["artifacts"]["build_log_tail"] = redact("\n".join(lines[-75:]), docker.secrets)
        if process.returncode:
            raise Blocked("native Docker image build failed; see sanitized exact build tail for the failing stage")
        check(report, "locked_build", "cargo build --release --locked -j 2, single <=45minute attempt")
    finally:
        # Includes exceptions/interrupts, not just the ordinary deadline path.
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=15)
        # Lock was created by this invocation, never inherited.
        lock.unlink()


def inspect_image(docker, report, built=False):
    info = json.loads(docker.call(["image", "inspect", IMAGE]).stdout)[0]
    require(info["Architecture"] == "amd64" and info["Os"] == "linux", "image must be Linux AMD64")
    config = info["Config"]
    require(config.get("Labels", {}).get("agentrecall-poc.module") == MODULE, "unverified native image provenance")
    require(config.get("User", "").split(":")[0] not in ("", "0", "root"), "image configured for root")
    allowed_env = {"PATH", "HOME"}
    require(all(e.partition("=")[0] in allowed_env for e in config.get("Env", [])), "image contains unexpected environment settings")
    report["artifacts"]["image"] = {"id": info["Id"], "architecture": info["Architecture"], "os": info["Os"], "size_bytes": info["Size"], "user": config["User"], "repo_digests": info.get("RepoDigests", [])}
    check(report, "image_architecture", report["artifacts"]["image"])
    if not built:
        check(report, "locked_build", "Preexisting image: build provenance cannot be independently established", "blocked")


def diagnostic(docker, directory):
    # Only stopped disposable state, read-only mount AND read-only exact CLI connection.
    sql = "SELECT (SELECT value FROM service_meta WHERE key='replica_id') AS replica_id, (SELECT CAST(value AS BIGINT) FROM service_meta WHERE key='local_sequence') AS local_sequence, (SELECT count(*) FROM memories) AS memories, (SELECT count(*) FROM mutation_log) AS mutation_events, (SELECT count(*) FROM idempotency_ledger) AS ledger_entries, (SELECT count(*) FROM applied_events) AS applied_events;"
    name = PREFIX + docker.run_id + "-diag-" + uuid.uuid4().hex[:8]
    out = docker.run(name, ["--network", "none", "--user", "%d:%d" % (os.getuid(), os.getgid()), "--entrypoint", "/usr/local/bin/duckdb", "--mount", "type=bind,src=%s,dst=/state/data,readonly" % (directory / "data")], ["-readonly", "-json", "/state/data/proof.duckdb", "-c", sql])
    rows = json.loads(out.stdout)
    require(len(rows) == 1 and isinstance(rows[0].get("local_sequence"), int), "diagnostic missing exact sequence")
    return rows[0]


def service_start(docker, directory, network, report):
    name = PREFIX + docker.run_id + "-serve-" + uuid.uuid4().hex[:8]
    started = time.monotonic()
    docker.run(name, ["--network", network, "--user", "%d:%d" % (os.getuid(), os.getgid()), "--read-only", "--tmpfs", "/tmp:rw,noexec,nosuid,size=64m", "--env-file", str(directory / "service.env"), "--mount", "type=bind,src=%s,dst=/state/data" % (directory / "data"), "--mount", "type=bind,src=%s,dst=/state/credentials,readonly" % (directory / "credentials")], ["serve"], detach=True)
    detail = json.loads(docker.call(["container", "inspect", name]).stdout)[0]
    require(not detail["HostConfig"].get("PortBindings") and not any(detail["NetworkSettings"].get("Ports", {}).values()), "service unexpectedly publishes a host port")
    token = (directory / "cleartext-token").read_text().strip()
    client = ContainerLoopbackMcp(docker, name, token)
    report["measurements"]["http_transport"] = "Docker exec stdin to container 127.0.0.1:8789, internal network, no host port; latency includes exec/relay overhead"
    try:
        ready, _ = client.ready()
    except (Blocked, ValueError):
        report["runtime_failure_log_tail"] = redact(docker.call(["logs", "--tail", "25", name], check=False).stderr[-4000:], docker.secrets)
        raise
    report["measurements"].setdefault("startup_seconds", []).append(time.monotonic() - started)
    require(ready.get("replication_configured") is False and ready.get("search_mode") == "lexical" and ready.get("hnsw_active") is False, "runtime isolation/readiness mismatch")
    require(ready.get("search_engine") == "duckdb_fts_bm25", "preprovisioned FTS was not loaded")
    return name, client, ready


def service_stop(docker, name):
    docker.call(["stop", "--time", "20", name], timeout=30)
    docker.remove_owned("container", name)


def runtime_suite(docker, directory, report):
    uid = os.getuid()
    if uid == 0:
        raise Blocked("non-root host UID required")
    for part in ("data", "credentials"):
        (directory / part).mkdir(mode=0o700)
    cursor = os.urandom(32).hex()
    docker.secrets.append(cursor)
    env_file(directory / "service.env", env_values(cursor))
    generated = docker.run(PREFIX + docker.run_id + "-credential", ["--network", "none", "--user", "%d:%d" % (uid, os.getgid()), "--mount", "type=bind,src=%s,dst=/state/credentials" % (directory / "credentials")], ["create-token", "--owner-id", "native-poc-owner", "--client-id", "native-poc-client", "--scopes", "memory:read,memory:write,memory:delete", "--namespaces", "shared", "--tokens-file", "/state/credentials/tokens.json"])
    match = re.fullmatch(r"Token \(shown once\): ([a-f0-9]{64})\n?", generated.stdout)
    require(match is not None, "create-token CLI output shape mismatch")
    token = match.group(1)
    docker.secrets.append(token)
    private_write(directory / "cleartext-token", token + "\n")
    credentials = directory / "credentials" / "tokens.json"
    require((credentials.stat().st_mode & 0o777) == 0o600, "token hash file must be private")
    require(token not in credentials.read_text(), "cleartext token leaked into service credentials")
    network = PREFIX + docker.run_id + "-internal"
    docker.networks.add(network)
    docker.call(["network", "create", "--internal", "--label", LABEL + "=" + docker.run_id, network])
    internal = json.loads(docker.call(["network", "inspect", network]).stdout)[0]
    require(internal.get("Internal") is True, "outbound network not disabled")
    name, client, ready = service_start(docker, directory, network, report)
    check(report, "ready", ready)
    # Actual attempted public-IP TCP connection, no DNS or remote request content.
    denied = docker.call(["exec", name, "timeout", "3", "bash", "-c", "echo > /dev/tcp/1.1.1.1/443"], timeout=10, check=False)
    if denied.returncode and "Network is unreachable" in denied.stderr:
        check(report, "outbound_denied_startup", {"internal_network": True, "probe_exit": denied.returncode, "diagnostic": "Network is unreachable", "ready": True})
    else:
        check(report, "outbound_denied_startup", {"internal_network": True, "probe_exit": denied.returncode, "limitation": "No unambiguous Network is unreachable proof"}, "blocked")
    actual_uid = docker.call(["exec", name, "id", "-u"]).stdout.strip()
    require(actual_uid != "0", "service ran as root")
    check(report, "nonroot", {"uid": actual_uid, "mount_owner_uid": uid})
    closure = docker.call(["exec", name, "cat", "/usr/share/agentrecall-poc/ldd.txt"]).stdout
    require("not found" not in closure and "libc" in closure, "unresolved native library")
    report["artifacts"]["native_library_closure"] = closure
    check(report, "native_libraries", "Builder-discovered ldd closure; service executable running")
    scan = docker.call(["exec", name, "sh", "-c", "find /home /usr/local /usr/share/agentrecall-poc -type f | sort"]).stdout
    require(not re.search(r"(?:tokens\.json|\.onnx|\.safetensors|cleartext-token|service\.env)", scan), "credential/model artifact baked into image")
    check(report, "no_baked_credentials_models", "allowlisted context, image env inspected, packaged paths scanned; private state mounted separately")
    payload = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "auth-poc", "version": "0.1"}}}
    for bearer in (None, "synthetic-invalid-credential"):
        status, _ = client.http("/mcp", payload, bearer, protocol=False)
        require_status(status, 401, "missing/incorrect bearer")
    check(report, "auth", "missing and incorrect bearer each HTTP 401")
    initialized = client.initialize()
    check(report, "initialize", {"serverInfo": initialized["serverInfo"], "protocolVersion": initialized.get("protocolVersion")})
    tools = client.rpc("tools/list", {})
    expected = {"memory_save", "memory_search", "memory_get", "memory_update", "memory_delete"}
    require({t["name"] for t in tools["tools"]} == expected, "tools/list mismatch")
    check(report, "tools_list", sorted(expected))
    saved = client.tool("memory_save", {"content": "Synthetic native container deletion proof", "tags": ["native-delete"], "client_request_id": "delete-proof"})
    require(saved.get("created") is True, "save did not create memory")
    memory = saved["memory"]
    memory_id = memory_identifier(memory)
    found = client.tool("memory_search", {"query": "deletion proof", "tags_any": ["native-delete"]})
    require(any(memory_identifier(m) == memory_id for m in found["results"]), "search missed saved memory")
    require(client.tool("memory_get", {"memory_id": memory_id})["memory"] == memory, "get mismatch")
    deleted = client.tool("memory_delete", {"memory_id": memory_id, "expected_revision": memory["revision"]})
    require(deleted == {"memory_id": memory_id, "deleted": True}, "delete mismatch")
    require(client.tool("memory_get", {"memory_id": memory_id})["memory"] is None, "deleted memory still fetched")
    require(not client.tool("memory_search", {"query": "deletion proof", "tags_any": ["native-delete"]})["results"], "deleted memory still searchable")
    check(report, "save_search_get_delete", "structured content and exact identity/deletion asserted")
    # Measurements contain exactly 100 samples per operation, distinct from protocol samples.
    cycles = {n: [] for n in ("save", "search", "get")}
    for i in range(100):
        for op in ("save", "search", "get"):
            tool = "memory_" + op
            if op == "save":
                args = {"content": "Synthetic native measurement cycle %03d" % i, "tags": ["cycle-%03d" % i], "client_request_id": "cycle-%03d" % i}
            elif op == "search":
                args = {"query": "native measurement", "tags_any": ["cycle-%03d" % i]}
            else:
                args = {"memory_id": memory_identifier(cycle_memory)}
            result = client.tool(tool, args)
            cycles[op].append(client.samples[tool][-1])
            if op == "save":
                require(result.get("created") is True, "cycle save not created")
                cycle_memory = result["memory"]
            elif op == "search":
                require(any(memory_identifier(m) == memory_identifier(cycle_memory) for m in result["results"]), "cycle search mismatch")
            else:
                require(result["memory"] == cycle_memory, "cycle get mismatch")
        if (i + 1) % 20 == 0:
            print("native MCP cycles: %d/100" % (i + 1), flush=True)
    report["measurements"]["cycle_latency_ms"] = {key: {"raw": samples, **percentiles(samples)} for key, samples in cycles.items()}
    report["measurements"]["protocol_and_tool_latency_ms"] = {key: {"raw": values, **percentiles(values)} for key, values in client.samples.items()}
    check(report, "100_cycles", "100 asserted synthetic save/search/get cycles; paced below unchanged source rate limiter")
    rss = docker.call(["exec", name, "cat", "/proc/1/status"]).stdout
    match = re.search(r"^VmRSS:\s+(\d+) kB$", rss, re.M)
    require(match is not None, "process RSS unavailable")
    report["measurements"]["runtime_rss_bytes"] = int(match.group(1)) * 1024
    check(report, "rss", "independent /proc/1/status VmRSS")
    retained_request = {"content": "Synthetic retained native persistence sentinel", "tags": ["native-retained"], "client_request_id": "stable-retained-request"}
    retained_result = client.tool("memory_save", retained_request)
    retained = retained_result["memory"]
    service_stop(docker, name)
    before = diagnostic(docker, directory)
    name, fresh, ready_after = service_start(docker, directory, network, report)
    fresh.initialize()
    require(ready_after["replica_id"] == ready["replica_id"] == before["replica_id"], "replica identity changed across recreation")
    require(fresh.tool("memory_get", {"memory_id": memory_identifier(retained)})["memory"] == retained, "retained memory changed")
    rows = fresh.tool("memory_search", {"query": "persistence sentinel", "tags_any": ["native-retained"]})["results"]
    require(len(rows) == 1 and memory_identifier(rows[0]) == memory_identifier(retained), "retained search continuity mismatch")
    check(report, "recreation", {"readiness_before": ready, "readiness_after": ready_after, "retained_memory_id": memory_identifier(retained)})
    retried = fresh.tool("memory_save", retained_request)
    require_idempotent_save(retained_result, retried)
    check(report, "idempotent_result", "exact same request returns identical Memory and created=false, per existing Store::save_record")
    service_stop(docker, name)
    after = diagnostic(docker, directory)
    require(after == before, "idempotent retry changed replica/sequence/memory/event/ledger/applied counts")
    check(report, "idempotent_metadata", {"before": before, "after": after, "diagnostic": "exact v2.0.1 CLI -readonly, service stopped, mount readonly"})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default=IMAGE, choices=[IMAGE])
    parser.add_argument("--platform", default="linux/amd64", choices=["linux/amd64"])
    parser.add_argument("--output", default="target/cloud-poc/native-container.json")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--preflight", action="store_true", help="verify exact native artifact/signature/toolchain gate only")
    mode.add_argument("--build", action="store_true", help="gate then one locked bounded image build, then runtime proof")
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[2]
    # This module is two directories below root: poc/native-container/prove.py.
    output = safe_output(root, args.output)
    report = {"module_id": MODULE, "status": "not_run", "checks": {n: {"status": "not_run", "detail": None} for n in REQUIRED}, "artifacts": {"builder": BUILDER, "runtime": RUNTIME}, "measurements": {"host": {"system": platform.system(), "architecture": platform.machine(), "python": platform.python_version()}, "emulation": "explicit linux/amd64 on ARM64 Docker Desktop; not production performance"}, "safety_findings": [], "limitations": ["Local filesystem-backed service only; not R2, object-backed Rust service, or Cloudflare deployment.", "No unsigned extensions, source/Cargo/config/CI modifications, or model/peer configuration."]}
    run_id = uuid.uuid4().hex[:12]
    directory = Path(tempfile.mkdtemp(prefix="native-container-private-", dir=str(output.parent)))
    directory.chmod(0o700)
    docker = Docker(run_id)
    exit_code = 2
    try:
        if args.preflight or args.build:
            artifact_gate(docker, directory, report)
        else:
            check(report, "artifact_gate", "Runtime image packaging must carry signed artifact manifest; independent gate not rerun", "blocked")
        if args.preflight:
            report["status"] = "not_run"
            report["limitations"].append("Preflight only: full build and runtime acceptance not executed.")
            exit_code = 0
        else:
            if args.build:
                build_image(docker, root, directory, report)
            inspect_image(docker, report, built=args.build)
            runtime_suite(docker, directory, report)
            report["status"] = "passed" if all(report["checks"][n]["status"] == "passed" for n in REQUIRED if n != "cleanup") else "blocked"
            exit_code = 0 if report["status"] == "passed" else 2
    except KeyboardInterrupt:
        report["status"] = "blocked"
        report["blocker"] = "Proof interrupted; owned subprocess/resource cleanup executed"
        exit_code = 2
    except Blocked as error:
        report["status"] = "blocked"
        report["blocker"] = redact(str(error), docker.secrets)
        exit_code = 2
    except (Failed, KeyError, ValueError, OSError, http.client.HTTPException) as error:
        report["status"] = "failed"
        report["failure"] = redact(str(error), docker.secrets)
        exit_code = 1
    finally:
        failures = docker.cleanup()
        try:
            shutil.rmtree(directory)
        except OSError as error:
            failures.append("private state cleanup: " + error.__class__.__name__)
        if failures:
            check(report, "cleanup", failures, "failed")
            report["status"] = "failed"
            report["safety_findings"].append("Owned cleanup not fully verified; see cleanup check")
            exit_code = 1
        else:
            check(report, "cleanup", "All tracked labeled containers/networks removed or verified absent; private run directory deleted")
        output.write_text(json.dumps(redact(report, docker.secrets), indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print("%s: %s; sanitized report %s" % (MODULE, report["status"], output), flush=True)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
