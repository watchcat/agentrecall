"""Offline tests for the environment-backed deploy-k8s.sh entrypoint.

Uses only synthetic credentials and subprocess stubs; never accesses the live
cluster or the real repository .env.
"""
import base64
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "deploy-k8s.sh"
TOKEN = "t" * 64
CURSOR = "c" * 64


class DeployCliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.env_file = self.home / "private.env"
        self.marker = self.home / "subprocess-called"
        self.bin = self.home / "bin"
        self.bin.mkdir()
        for name in ("ssh", "docker", "gzip"):
            tool = self.bin / name
            tool.write_text('#!/bin/sh\nprintf called > "' + str(self.marker) + '"\nexit 99\n')
            tool.chmod(0o755)
        self.config = {
            "AGENTRECALL_SSH": "root@192.0.2.10",
            "AGENTRECALL_DOMAIN": "offline-only.invalid",
            "AGENTRECALL_IMAGE": "agentrecall:test-unique",
            "AGENTRECALL_IMAGE_CONFIG_DIGEST": "sha256:" + "a" * 64,
            "AGENTRECALL_REPLICA_ID": "test-replica",
            "AGENTRECALL_MCP_TOKEN": TOKEN,
            "AGENTRECALL_CURSOR_HMAC_SECRET": CURSOR,
            "AGENTRECALL_OWNER_ID": "test-owner",
            "AGENTRECALL_CLIENT_ID": "test-client",
            "AGENTRECALL_NAMESPACES": "shared",
            "AGENTRECALL_SCOPES": "memory:read,memory:write,memory:delete",
            "AGENTRECALL_CLUSTER_ISSUER": "letsencrypt",
            "AGENTRECALL_CA_BUNDLE": "",
        }

    def write_env(self, config=None, extra=""):
        values = self.config if config is None else config
        self.env_file.write_text("\n".join(k + "=" + v for k, v in values.items()) + "\n" + extra)
        self.env_file.chmod(0o600)

    def cli(self, *args, xtrace=False):
        env = dict(os.environ)
        env["PATH"] = str(self.bin) + os.pathsep + env.get("PATH", "")
        result = subprocess.run(
            ["bash"] + (["-x"] if xtrace else []) + [str(SCRIPT), "--env-file", str(self.env_file), *args],
            cwd=self.home, env=env, text=True, capture_output=True, timeout=15,
        )
        output = result.stdout + result.stderr
        self.assertNotIn(TOKEN, output)
        self.assertNotIn(CURSOR, output)
        self.assertNotIn("Traceback", output)
        return result, output

    def reject(self, key, value):
        self.write_env({**self.config, key: value})
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_help_does_not_need_env(self):
        result, output = self.cli("--help")
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("--dry-run", output)
        self.assertFalse(self.marker.exists())

    def test_entrypoint_is_executable(self):
        self.assertTrue(os.access(SCRIPT, os.X_OK))

    def test_dry_run_is_offline_and_redacted(self):
        self.write_env()
        result, output = self.cli("--dry-run")
        self.assertEqual(result.returncode, 0, output)
        self.assertFalse(self.marker.exists())

    def test_shell_xtrace_does_not_leak_secrets(self):
        self.write_env()
        result, output = self.cli("--dry-run", xtrace=True)
        self.assertEqual(result.returncode, 0, output)
        self.assertFalse(self.marker.exists())

    def test_dotenv_does_not_execute_shell_substitution(self):
        injected = self.home / "dotenv-executed"
        self.write_env(extra='UNRELATED=$(touch "' + str(injected) + '")\n')
        result, output = self.cli("--dry-run")
        self.assertEqual(result.returncode, 0, output)
        self.assertFalse(injected.exists())
        self.assertFalse(self.marker.exists())

    def test_optional_quotes_and_export(self):
        self.write_env({**self.config, "AGENTRECALL_MCP_TOKEN": '"' + TOKEN + '"'})
        self.env_file.write_text(self.env_file.read_text().replace("AGENTRECALL_DOMAIN=", "export AGENTRECALL_DOMAIN="))
        result, output = self.cli("--dry-run")
        self.assertEqual(result.returncode, 0, output)

    def test_duplicate_assignment_fails_without_values(self):
        self.write_env(extra="AGENTRECALL_MCP_TOKEN=" + TOKEN + "\n")
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_malformed_line_is_redacted(self):
        self.write_env(extra="MALFORMED " + TOKEN + "\n")
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)

    def test_missing_file_fails(self):
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_public_file_mode_fails(self):
        self.write_env()
        self.env_file.chmod(0o644)
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_symlink_file_fails(self):
        other = self.home / "other.env"
        self.write_env()
        self.env_file.rename(other)
        self.env_file.symlink_to(other)
        result, _ = self.cli("--dry-run")
        self.assertNotEqual(result.returncode, 0)

    def test_unmatched_quote_fails(self):
        self.reject("AGENTRECALL_MCP_TOKEN", '"' + TOKEN)

    def test_short_token_fails(self):
        self.reject("AGENTRECALL_MCP_TOKEN", "short")

    def test_short_cursor_fails(self):
        self.reject("AGENTRECALL_CURSOR_HMAC_SECRET", "short")

    def test_placeholder_secret_fails(self):
        self.reject("AGENTRECALL_MCP_TOKEN", "replace-with-at-least-32-random-characters")

    def test_empty_identity_fails(self):
        self.reject("AGENTRECALL_REPLICA_ID", "")

    def test_domain_injection_fails(self):
        self.reject("AGENTRECALL_DOMAIN", "example.com;touch-bad")

    def test_ssh_option_injection_fails(self):
        self.reject("AGENTRECALL_SSH", "root@-bad")

    def test_mutable_latest_image_fails(self):
        self.reject("AGENTRECALL_IMAGE", "agentrecall:latest")

    def test_invalid_digest_fails(self):
        self.reject("AGENTRECALL_IMAGE_CONFIG_DIGEST", "sha256:bad")

    def test_unsupported_scope_fails(self):
        self.reject("AGENTRECALL_SCOPES", "memory:admin")

    def test_empty_csv_namespace_fails(self):
        self.reject("AGENTRECALL_NAMESPACES", "shared,,private")

    def test_replication_is_not_silently_enabled(self):
        self.reject("AGENTRECALL_REPLICATION_TOKEN", "r" * 64)

    def test_embeddings_are_not_silently_enabled(self):
        self.reject("AGENTRECALL_EMBEDDING_PROVIDER", "bge-m3")

    def test_dry_build_does_not_start_tools(self):
        self.write_env()
        self.cli("--dry-run", "--build")
        self.assertFalse(self.marker.exists())

    def test_semantic_provision_dry_run_is_offline(self):
        self.write_env()
        result, output = self.cli("--profile", "semantic", "--provision-model", "--dry-run")
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("no live rollout", output)
        self.assertFalse(self.marker.exists())

    def test_provision_requires_explicit_semantic_profile(self):
        self.write_env()
        result, _ = self.cli("--provision-model", "--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_check_cannot_provision(self):
        self.write_env()
        result, _ = self.cli("--profile", "semantic", "--provision-model", "--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_provision_cannot_build(self):
        self.write_env()
        result, _ = self.cli("--profile", "semantic", "--provision-model", "--build", "--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_provision_cannot_import(self):
        self.write_env()
        result, _ = self.cli("--profile", "semantic", "--provision-model", "--import-image", "--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_profile_override_does_not_download(self):
        self.write_env({**self.config, "AGENTRECALL_PROFILE": "semantic"})
        result, output = self.cli("--profile", "lexical", "--dry-run")
        self.assertEqual(result.returncode, 0, output)
        self.assertIn("Selected profile: lexical", output)
        self.assertFalse(self.marker.exists())

    def test_semantic_build_refused_before_any_action(self):
        self.write_env()
        result, _ = self.cli("--profile", "semantic", "--build", "--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())

    def test_semantic_import_refused_before_any_action(self):
        self.write_env()
        result, _ = self.cli("--profile", "semantic", "--import-image", "--dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.marker.exists())


class DeployGuardTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.module = types.ModuleType("deploy_k8s_under_test")
        code = SCRIPT.read_text().split("<<'PY'\n", 1)[1].rsplit("\nPY", 1)[0]
        exec(compile(code, str(SCRIPT), "exec"), self.module.__dict__)
        self.raw = {
            "AGENTRECALL_SSH": "root@192.0.2.10", "AGENTRECALL_DOMAIN": "offline-only.invalid",
            "AGENTRECALL_IMAGE": "agentrecall:test-unique", "AGENTRECALL_IMAGE_CONFIG_DIGEST": "sha256:" + "a" * 64,
            "AGENTRECALL_REPLICA_ID": "test-replica", "AGENTRECALL_MCP_TOKEN": TOKEN,
            "AGENTRECALL_CURSOR_HMAC_SECRET": CURSOR, "AGENTRECALL_OWNER_ID": "test-owner",
            "AGENTRECALL_CLIENT_ID": "test-client", "AGENTRECALL_NAMESPACES": "shared",
            "AGENTRECALL_SCOPES": "memory:read,memory:write,memory:delete",
        }
        self.cfg = self.module.configuration(self.raw)
        self.env_file = self.home / "private.env"
        self.env_file.write_text("\n".join(k + "=" + v for k, v in self.raw.items()) + "\n")
        self.env_file.chmod(0o600)
        self.state = {
            "nodes": {"items": [{"status": {"nodeInfo": {"architecture": "amd64", "operatingSystem": "linux"},
                                             "conditions": [{"type": "Ready", "status": "True"}]}}]},
            "issuer": {"status": {"conditions": [{"type": "Ready", "status": "True"}]}},
            "namespace": {"metadata": {"labels": {"app": "agentrecall"}}},
            "pvc": {"metadata": {"labels": {"app": "agentrecall"}}, "status": {"phase": "Bound"},
                    "spec": {"storageClassName": "local-path", "accessModes": ["ReadWriteOnce"],
                             "resources": {"requests": {"storage": "5Gi"}}}},
            "secret": self.module.secret_manifest(self.cfg), "certificate": None, "service": None, "ingress": None,
            "deployment": {"metadata": {"labels": {"app": "agentrecall"}, "generation": 1},
                           "status": {"replicas": 1, "updatedReplicas": 1, "readyReplicas": 1, "availableReplicas": 1, "observedGeneration": 1},
                           "spec": {"replicas": 1, "strategy": {"type": "Recreate"}, "template": {"spec": {
                               "containers": [{"name": "agentrecall", "image": self.cfg["IMAGE"], "env": [
                                   {"name": "AGENTRECALL_REPLICA_ID", "value": self.cfg["REPLICA_ID"]},
                                   {"name": "AGENTRECALL_DATABASE_PATH", "value": "/state/data/cloud.duckdb"},
                                   {"name": "AGENTRECALL_ROLE", "value": "cloud"},
                                   {"name": "AGENTRECALL_ALLOWED_HOSTS", "value": self.cfg["DOMAIN"]},
                               ]}], "volumes": [
                                   {"name": "agentrecall-data", "persistentVolumeClaim": {"claimName": "agentrecall-data"}},
                                   {"name": "runtime-credentials", "secret": {"secretName": "agentrecall-runtime"}},
                               ]}}}},
        }

    def guard(self):
        self.module.validate_existing(self.state, self.cfg)

    def invoke(self, *args, digest="present"):
        remote = mock.Mock()
        output = io.StringIO()
        actual_digest = self.cfg["IMAGE_CONFIG_DIGEST"] if digest == "present" else digest
        with contextlib.ExitStack() as stack:
            stack.enter_context(mock.patch.object(self.module, "Remote", return_value=remote))
            stack.enter_context(mock.patch.object(self.module, "collect_state", return_value=self.state))
            dns = stack.enter_context(mock.patch.object(self.module, "validate_dns"))
            image = stack.enter_context(mock.patch.object(self.module, "image_id", return_value=actual_digest))
            build = stack.enter_context(mock.patch.object(self.module, "build_image"))
            imported = stack.enter_context(mock.patch.object(self.module, "import_image"))
            verify = stack.enter_context(mock.patch.object(self.module, "verify_live"))
            stack.enter_context(contextlib.redirect_stdout(output))
            try:
                self.module.main(ROOT, ["--env-file", str(self.env_file), *args])
                error = None
            except self.module.DeployError as e:
                error = e
        self.assertNotIn(TOKEN, output.getvalue())
        self.assertNotIn(CURSOR, output.getvalue())
        return remote, dns, image, build, imported, verify, error

    def test_secret_contains_hash_not_bearer(self):
        secret = self.module.secret_manifest(self.cfg)
        self.assertNotIn(TOKEN, json.dumps(secret))
        record = json.loads(base64.b64decode(secret["data"]["tokens.json"]))[0]
        self.assertEqual(record["token_hash"], self.module.hashlib.sha256(TOKEN.encode()).hexdigest())
        self.assertEqual(base64.b64decode(secret["data"]["cursor-key"]).decode(), CURSOR)

    def test_matching_existing_secret_is_accepted(self):
        self.guard()

    def test_scope_order_does_not_rotate_secret(self):
        secret = self.state["secret"]
        records = json.loads(base64.b64decode(secret["data"]["tokens.json"]))
        records[0]["scopes"].reverse()
        secret["data"]["tokens.json"] = base64.b64encode(json.dumps(records).encode()).decode()
        self.guard()

    def test_changed_cursor_is_refused(self):
        self.state["secret"]["data"]["cursor-key"] = base64.b64encode(b"different-cursor" * 4).decode()
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_changed_token_is_refused_before_image_or_mutations(self):
        self.state["secret"]["data"]["tokens.json"] = base64.b64encode(b"[]").decode()
        remote, _, image, build, imported, _, error = self.invoke("--build")
        self.assertIsNotNone(error)
        remote.run.assert_not_called()
        image.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()

    def test_multiple_existing_clients_are_not_removed(self):
        secret = self.state["secret"]
        records = json.loads(base64.b64decode(secret["data"]["tokens.json"]))
        records.append({**records[0], "client_id": "another-client"})
        secret["data"]["tokens.json"] = base64.b64encode(json.dumps(records).encode()).decode()
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_missing_secret_on_existing_deployment_is_not_regenerated(self):
        self.state["secret"] = None
        remote, _, image, _, _, _, error = self.invoke()
        self.assertIsNotNone(error)
        remote.run.assert_not_called()
        image.assert_not_called()

    def test_changed_replica_is_refused(self):
        self.cfg["REPLICA_ID"] = "other-replica"
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_foreign_namespace_is_refused(self):
        self.state["namespace"]["metadata"]["labels"]["app"] = "scout"
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_two_writers_are_refused(self):
        self.state["deployment"]["spec"]["replicas"] = 2
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def semantic_existing(self):
        pod = self.state["deployment"]["spec"]["template"]["spec"]
        pod["containers"][0]["env"] += [
            {"name": "AGENTRECALL_EMBEDDING_PROVIDER", "value": "bge-m3"},
            {"name": "AGENTRECALL_EMBEDDING_CACHE_DIR", "value": "/models"},
        ]
        pod["containers"][0]["volumeMounts"] = [{"name": "models", "mountPath": "/models", "readOnly": True}]
        pod["volumes"].append({"name": "models", "persistentVolumeClaim": {"claimName": "agentrecall-models", "readOnly": True}})
        return pod

    def test_supported_semantic_workload_can_transition_to_lexical(self):
        self.semantic_existing()
        self.guard()

    def test_unknown_existing_semantic_provider_refused(self):
        pod = self.semantic_existing()
        next(e for e in pod["containers"][0]["env"] if e["name"] == "AGENTRECALL_EMBEDDING_PROVIDER")["value"] = "other"
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_writable_existing_model_cache_refused(self):
        pod = self.semantic_existing()
        pod["volumes"][-1]["persistentVolumeClaim"]["readOnly"] = False
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_provisioning_only_never_applies_live_manifests(self):
        semantic = self.module.semantic_module(ROOT)
        with mock.patch.object(self.module, "semantic_module", return_value=semantic), mock.patch.object(semantic, "provision") as provision:
            remote, _, _, build, imported, verify, error = self.invoke("--profile", "semantic", "--provision-model")
        self.assertIsNone(error)
        provision.assert_called_once()
        remote.run.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()
        verify.assert_not_called()

    def test_unbound_pvc_is_refused(self):
        self.state["pvc"]["status"]["phase"] = "Pending"
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_foreign_ingress_is_refused(self):
        self.state["ingress"] = {"spec": {"ingressClassName": "traefik", "rules": []}}
        with self.assertRaises(self.module.DeployError):
            self.guard()

    def test_manifests_have_only_safe_rendered_placeholders(self):
        workload, ingress = self.module.render_manifests(ROOT, self.cfg)
        self.assertNotIn("__", workload + ingress)
        self.assertNotIn(TOKEN, workload + ingress)
        self.assertNotIn(CURSOR, workload + ingress)
        self.assertIn(self.cfg["DOMAIN"], workload + ingress)
        self.assertIn('router.tls: "true"', ingress)
        self.assertIn("router.entrypoints: websecure", ingress)

    def test_default_does_not_build_import_or_update_secret(self):
        remote, _, _, build, imported, verify, error = self.invoke()
        self.assertIsNone(error)
        build.assert_not_called()
        imported.assert_not_called()
        verify.assert_called_once()
        commands = [c.args[0] for c in remote.run.call_args_list]
        self.assertFalse(any("create" in c and "certificate" not in c for c in commands))
        self.assertFalse(any("secret" in c or "restart" in c or "delete" in c for c in commands))

    def test_check_has_no_mutating_operations(self):
        remote, _, image, build, imported, verify, error = self.invoke("--check")
        self.assertIsNone(error)
        remote.run.assert_not_called()
        image.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()
        verify.assert_called_once()

    def test_missing_image_default_fails_before_mutations(self):
        remote, _, _, build, imported, _, error = self.invoke(digest=None)
        self.assertIsNotNone(error)
        remote.run.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()

    def test_import_existing_image_is_skipped(self):
        _, _, _, build, imported, _, error = self.invoke("--import-image")
        self.assertIsNone(error)
        build.assert_not_called()
        imported.assert_not_called()

    def test_build_does_not_overwrite_remote_tag(self):
        remote, _, _, build, imported, _, error = self.invoke("--build")
        self.assertIsNotNone(error)
        remote.run.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()

    def test_first_deploy_streams_only_hashed_token(self):
        for name in ("namespace", "deployment", "pvc", "secret"):
            self.state[name] = None
        remote, _, _, _, _, _, error = self.invoke()
        self.assertIsNone(error)
        create = [c for c in remote.run.call_args_list if c.args[0] == "kubectl -n agentrecall create -f -"]
        self.assertEqual(len(create), 1)
        payload = create[0].args[1]
        self.assertNotIn(TOKEN, payload)
        self.assertEqual(json.loads(payload)["kind"], "Secret")

    def test_remote_errors_and_argv_do_not_leak_secret_payload(self):
        remote = self.module.Remote(self.cfg)
        payload = json.dumps(self.module.secret_manifest(self.cfg))
        with mock.patch.object(self.module.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, TOKEN, CURSOR)) as run:
            with self.assertRaises(self.module.DeployError) as error:
                remote.run("kubectl -n agentrecall create -f -", payload)
        self.assertNotIn(TOKEN, str(error.exception))
        self.assertNotIn(CURSOR, str(error.exception))
        self.assertNotIn(payload, " ".join(run.call_args.args[0]))
        self.assertEqual(run.call_args.kwargs["input"], payload)

    def test_dry_run_does_not_even_construct_remote_or_resolve_dns(self):
        remote, dns, image, build, imported, verify, error = self.invoke("--dry-run", "--build")
        self.assertIsNone(error)
        remote.run.assert_not_called()
        dns.assert_not_called()
        image.assert_not_called()
        build.assert_not_called()
        imported.assert_not_called()
        verify.assert_not_called()

    def test_image_pin_uses_config_not_oci_index(self):
        remote = mock.Mock()
        remote.run.side_effect = [json.dumps({"images": [{"repoTags": ["docker.io/library/" + self.cfg["IMAGE"]]}]}),
                                  json.dumps({"status": {"id": self.cfg["IMAGE_CONFIG_DIGEST"]}})]
        self.assertEqual(self.module.image_id(remote, self.cfg), self.cfg["IMAGE_CONFIG_DIGEST"])
        remote.run.side_effect = [json.dumps({"images": [{"repoTags": [self.cfg["IMAGE"]]}]}),
                                  json.dumps({"status": {"id": "sha256:" + "b" * 64}})]
        with self.assertRaises(self.module.DeployError):
            self.module.image_id(remote, self.cfg)

    def test_arm_image_cannot_be_imported(self):
        with mock.patch.object(self.module, "local_run", return_value=json.dumps([{"Architecture": "arm64", "Os": "linux"}])):
            with mock.patch.object(self.module.subprocess, "Popen") as popen:
                with self.assertRaises(self.module.DeployError):
                    self.module.import_image(mock.Mock(), self.cfg)
                popen.assert_not_called()

    def build_fixture(self):
        root = self.home / "source"
        for name in ("src", "migrations", "poc/native-container"):
            (root / name).mkdir(parents=True, exist_ok=True)
        for name in ("Cargo.toml", "Cargo.lock", "src/main.rs", "migrations/001.sql", "poc/native-container/Dockerfile", "poc/native-container/Dockerfile.dockerignore"):
            (root / name).write_text("synthetic build input\n")
        (root / ".env").write_text(TOKEN)
        (root / ".git").mkdir()
        return root

    def test_build_context_excludes_env_and_git(self):
        root = self.build_fixture()
        def local(argv, timeout=60):
            if argv[1:3] == ["buildx", "build"]:
                context = Path(argv[-1])
                self.assertFalse((context / ".env").exists())
                self.assertFalse((context / ".git").exists())
                self.assertTrue((context / "src/main.rs").exists())
                self.assertTrue((context / "Dockerfile.dockerignore").exists())
                self.assertIn("linux/amd64", argv)
            return ""
        with mock.patch.object(self.module, "local_run", side_effect=local), contextlib.redirect_stdout(io.StringIO()):
            self.module.build_image(root, self.cfg)

    def test_symlink_in_build_inputs_is_refused(self):
        root = self.build_fixture()
        (root / "src/private.rs").symlink_to(root / ".env")
        with mock.patch.object(self.module, "local_run", return_value="") as run:
            with self.assertRaises(self.module.DeployError):
                self.module.build_image(root, self.cfg)
        run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
