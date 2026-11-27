import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("native_prove", Path(__file__).with_name("prove.py"))
p = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(p)


class ProtocolTests(unittest.TestCase):
    def test_http_status_asserted(self):
        p.require_status(401, 401, "auth")
        with self.assertRaises(p.Failed):
            p.require_status(200, 401, "auth")

    def test_rpc_error_and_id_rejected(self):
        for body in ({"jsonrpc": "2.0", "id": 2, "result": {}}, {"jsonrpc": "2.0", "id": 1, "error": {"code": -1}}, {"id": 1, "result": {}}):
            with self.assertRaises(p.Failed):
                p.parse_rpc(body, 1)
        self.assertEqual(p.parse_rpc({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}}, 1), {"tools": []})

    def test_tool_errors_not_http_success(self):
        for result in ({"isError": True, "structuredContent": {}}, {"content": [{"type": "text", "text": "success"}]}, {"structuredContent": {"error": "bad"}}):
            with self.assertRaises(p.Failed):
                p.parse_tool(result)
        self.assertEqual(p.parse_tool({"isError": False, "structuredContent": {"memory": None}}), {"memory": None})

    def test_percentiles_and_empty_not_invented(self):
        self.assertEqual(p.percentiles([]), {"count": 0, "p50_ms": None, "p95_ms": None})
        self.assertEqual(p.percentiles(list(range(1, 101))), {"count": 100, "p50_ms": 50, "p95_ms": 95})

    def test_http_protocol_and_no_proxy(self):
        c = p.Mcp(1234, "private")
        with patch.object(c, "http", return_value=(200, {"jsonrpc": "2.0", "id": 1, "result": {"serverInfo": {"name": "test"}}})) as http, patch.object(p.time, "sleep"):
            c.initialize()
        self.assertEqual(http.call_args.args[2], "private")
        self.assertFalse(http.call_args.kwargs["protocol"])

    def test_memory_result_uses_id_not_request_memory_id(self):
        value = "00000000-0000-4000-8000-000000000000"
        self.assertEqual(p.memory_identifier({"id": value}), value)
        for memory in ({"memory_id": value}, {"id": "not-a-uuid"}, {"id": 1}):
            with self.assertRaises(p.Failed):
                p.memory_identifier(memory)

    def test_retry_preserves_memory_but_reports_not_created(self):
        original = {"memory": {"id": "same", "revision": 1}, "created": True}
        p.require_idempotent_save(original, {"memory": original["memory"], "created": False})
        for result in (original, {"memory": {"id": "changed"}, "created": False}):
            with self.assertRaises(p.Failed):
                p.require_idempotent_save(original, result)

    def test_container_loopback_relay_parses_chunked_and_hides_credential_from_args(self):
        docker = p.Docker("unit")
        client = p.ContainerLoopbackMcp(docker, p.PREFIX + "unit-serve", "private-credential")
        wire = b'HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nC\r\n{"ok": true}\r\n0\r\n\r\n'
        with patch.object(docker, "call", return_value=subprocess.CompletedProcess([], 0, wire, b"")) as call:
            self.assertEqual(client.http("/mcp", {"synthetic": "unicode \u00e9"}, "private-credential"), (200, {"ok": True}))
        self.assertNotIn("private-credential", " ".join(call.call_args.args[0]))
        self.assertIn("Authorization: Bearer private-credential\r\n", call.call_args.kwargs["input_text"])
        self.assertTrue(call.call_args.kwargs["binary_output"])
        self.assertFalse(call.call_args.kwargs["check"])

    def test_container_relay_rejects_header_and_path_injection(self):
        docker = p.Docker("unit")
        client = p.ContainerLoopbackMcp(docker, p.PREFIX + "unit-serve", "private")
        with patch.object(docker, "call") as call:
            with self.assertRaises(p.Failed):
                client.http("/mcp", {}, "bad\r\nInjected: 1")
            with self.assertRaises(p.Failed):
                client.http("http://external.example/")
        call.assert_not_called()


class SecretSafetyTests(unittest.TestCase):
    def test_recursive_redaction(self):
        result = p.redact({"authorization": "s", "cursor_secret": "s", "detail": ["Bearer s", "Token (shown once): s", "s"]}, ["s"])
        self.assertNotIn('"s"', json.dumps(result))
        self.assertEqual(result["authorization"], "[REDACTED]")
        self.assertEqual(p.redact("Bearer secret-value"), "Bearer [REDACTED]")

    def test_private_file_no_overwrite(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "credential"
            p.private_write(path, "private")
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                p.private_write(path, "replaced")

    def test_allowlist_omits_models_peers_replication(self):
        values = p.env_values("c" * 64)
        self.assertEqual(set(values), p.ENV_KEYS)
        self.assertFalse(any(x in k for k in values for x in ("PEER", "EMBEDDING", "REPLICATION", "TRUSTED")))
        with tempfile.TemporaryDirectory() as d:
            p.env_file(Path(d) / "good", values)
            bad = dict(values, AGENTRECALL_PEER_URL="https://live")
            with self.assertRaises(p.Failed):
                p.env_file(Path(d) / "bad", bad)
            values["AGENTRECALL_ROLE"] = "local\nINJECTED=1"
            with self.assertRaises(p.Failed):
                p.env_file(Path(d) / "newline", values)

    def test_output_refuses_other_locations_and_symlinks(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            self.assertEqual(p.safe_output(root, "target/cloud-poc/native-container.json").name, "native-container.json")
            for value in ("target/cloud-poc/object-storage.json", "Cargo.toml", "target/cloud-poc/native-container.json/../x"):
                with self.assertRaises(p.Blocked):
                    p.safe_output(root, value)
            link = root / "target/cloud-poc/native-container-link.json"
            link.symlink_to(root / "other")
            with self.assertRaises(p.Blocked):
                p.safe_output(root, str(link))


class DockerSafetyTests(unittest.TestCase):
    def setUp(self):
        self.docker = p.Docker("unit")
        self.name = p.PREFIX + "unit-serve"

    def test_shell_false_and_host_environment_not_inherited(self):
        with patch.dict(os.environ, {"AGENTRECALL_PEER_TOKEN": "live", "HTTP_PROXY": "https://proxy", "DOCKER_HOST": "tcp://remote"}):
            docker = p.Docker("unit")
        with patch.object(p.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "ok", "")) as run:
            docker.call(["version"])
        self.assertFalse(run.call_args.kwargs["shell"])
        self.assertEqual(run.call_args.args[0], ["docker", "version"])
        self.assertNotIn("AGENTRECALL_PEER_TOKEN", run.call_args.kwargs["env"])
        self.assertNotIn("HTTP_PROXY", run.call_args.kwargs["env"])
        self.assertNotIn("DOCKER_HOST", run.call_args.kwargs["env"])
        self.assertEqual(run.call_args.kwargs["timeout"], 30)

    def test_binary_stdin_preserves_http_framing(self):
        with patch.object(p.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"", b"")) as run:
            self.docker.call(["exec", "-i", self.name, "bash"], input_text="private\r\n", binary_output=True)
        self.assertFalse(run.call_args.kwargs["text"])
        self.assertEqual(run.call_args.kwargs["input"], b"private\r\n")
        self.assertNotIn("private", " ".join(run.call_args.args[0]))

    def test_service_start_never_publishes_host_port(self):
        directory = Path("/synthetic")
        detail = [{"HostConfig": {"PortBindings": {}}, "NetworkSettings": {"Ports": {"8789/tcp": None}}}]
        ready = {"replication_configured": False, "search_mode": "lexical", "hnsw_active": False, "search_engine": "duckdb_fts_bm25"}
        with patch.object(self.docker, "run") as run, patch.object(self.docker, "call", return_value=subprocess.CompletedProcess([], 0, json.dumps(detail), "")), patch.object(p.Path, "read_text", return_value="private"), patch.object(p.ContainerLoopbackMcp, "ready", return_value=(ready, 0)):
            p.service_start(self.docker, directory, "internal", {"measurements": {}})
        self.assertNotIn("--publish", run.call_args.args[1])
        self.assertEqual(run.call_args.args[1][:2], ["--network", "internal"])

    def test_caps_and_ownership_registered_before_failure(self):
        with patch.object(self.docker, "call", side_effect=p.Blocked("timeout")) as call:
            with self.assertRaises(p.Blocked):
                self.docker.run(self.name, [], ["serve"], detach=True)
        args = call.call_args.args[0]
        self.assertIn(self.name, self.docker.containers)
        for value in ("linux/amd64", "2", "2g", "ALL", "no-new-privileges:true", p.LABEL + "=unit"):
            self.assertIn(value, args)

    def test_refuse_unowned_launch(self):
        with self.assertRaises(p.Failed):
            self.docker.run("unrelated-user-container", [], [])

    def test_timeout_does_not_leak_output(self):
        with patch.object(p.subprocess, "run", side_effect=subprocess.TimeoutExpired("docker", 10, output="private-token")):
            with self.assertRaises(p.Blocked) as error:
                self.docker.call(["run"], timeout=10)
        self.assertNotIn("private-token", str(error.exception))

    def test_docker_error_redacts_known_credentials(self):
        self.docker.secrets.append("synthetic-secret")
        with patch.object(p.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "Bearer synthetic-secret")):
            with self.assertRaises(p.Blocked) as error:
                self.docker.call(["run"])
        self.assertNotIn("synthetic-secret", str(error.exception))

    def test_cleanup_checks_label_and_never_prunes(self):
        self.docker.containers.add(self.name)
        calls = []
        def invoke(args, **kwargs):
            calls.append(args)
            if "inspect" in args:
                return subprocess.CompletedProcess([], 0, json.dumps([{"Config": {"Labels": {p.LABEL: "unit"}}}]), "")
            return subprocess.CompletedProcess([], 0, "", "")
        with patch.object(self.docker, "call", side_effect=invoke):
            self.assertEqual(self.docker.cleanup(), [])
        self.assertEqual(calls[-1], ["rm", "--force", self.name])
        self.assertFalse(any("prune" in args for args in calls))

    def test_cleanup_refuses_mismatched_label(self):
        self.docker.containers.add(self.name)
        with patch.object(self.docker, "call", return_value=subprocess.CompletedProcess([], 0, '[{"Config":{"Labels":{}}}]', "")) as call:
            failures = self.docker.cleanup()
        self.assertEqual(call.call_count, 1)
        self.assertIn("ownership label mismatch", failures[0])

    def test_cleanup_timeout_reported(self):
        self.docker.containers.add(self.name)
        with patch.object(self.docker, "call", side_effect=p.Blocked("cleanup timed out")):
            self.assertTrue(self.docker.cleanup())

    def test_image_platform_rejection(self):
        with patch.object(self.docker, "call", return_value=subprocess.CompletedProcess([], 0, '[{"Architecture":"arm64","Os":"linux"}]', "")):
            with self.assertRaises(p.Failed):
                p.inspect_image(self.docker, {"artifacts": {}, "checks": {}})


class BuildPolicyTests(unittest.TestCase):
    def test_locked_bounded_parallel_build_and_context(self):
        dockerfile = Path(__file__).with_name("Dockerfile").read_text()
        self.assertIn("cargo build --release --locked -j 2", dockerfile)
        self.assertIn("CARGO_BUILD_JOBS=2 NUM_JOBS=2", dockerfile)
        self.assertIn("USER 10001:10001", dockerfile)
        self.assertIn("--network=none", dockerfile)
        self.assertNotIn("allow_unsigned_extensions", dockerfile)
        policy = Path(__file__).with_name("Dockerfile.dockerignore").read_text().splitlines()
        includes = [line for line in policy if line.startswith("!")]
        self.assertEqual(includes, ["!Cargo.toml", "!Cargo.lock", "!src/", "!src/**", "!migrations/", "!migrations/*.sql"])
        self.assertEqual(next(line for line in policy if line and not line.startswith("#")), "**")

    def test_runtime_libraries_preserve_merged_usr_symlinks(self):
        dockerfile = Path(__file__).with_name("Dockerfile").read_text()
        self.assertIn('resolved="$(readlink -f "$lib")"', dockerfile)
        self.assertIn('cp -L "$resolved" "/runtime$resolved"', dockerfile)
        self.assertNotIn('cp -L "$lib" "/runtime$lib"', dockerfile)

    def test_existing_build_lock_blocks_without_docker(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            target = root / "target/cloud-poc"
            target.mkdir(parents=True)
            (target / "native-container-build.lock").write_text("another-build")
            with self.assertRaises(p.Blocked):
                p.build_image(p.Docker("unit"), root, target, {"measurements": {}})
            self.assertEqual((target / "native-container-build.lock").read_text(), "another-build")

    def test_interrupted_build_terminates_owned_client_and_unlocks(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            target = root / "target/cloud-poc"
            target.mkdir(parents=True)
            process = Mock()
            process.poll.return_value = None
            disk = Mock(free=20 * 1024 ** 3)
            with patch.object(p.shutil, "disk_usage", return_value=disk), patch.object(p.subprocess, "Popen", return_value=process), patch.object(p.time, "sleep", side_effect=KeyboardInterrupt), patch.object(p.time, "monotonic", return_value=0):
                with self.assertRaises(KeyboardInterrupt):
                    p.build_image(p.Docker("unit"), root, target, {"measurements": {}})
            process.terminate.assert_called_once()
            process.wait.assert_called_once_with(timeout=15)
            self.assertFalse((target / "native-container-build.lock").exists())

    def test_exact_artifact_digest_mismatch_blocks_before_use(self):
        response = Mock()
        response.read.side_effect = [b"not-the-exact-archive", b""]
        opened = Mock()
        opened.__enter__ = Mock(return_value=response)
        opened.__exit__ = Mock(return_value=False)
        with patch.object(p.urllib.request, "urlopen", return_value=opened):
            with self.assertRaises(p.Blocked) as error:
                p.download_verified("onnx", Path("/unused"))
        self.assertIn("SHA256 mismatch", str(error.exception))


if __name__ == "__main__":
    unittest.main()
