"""Offline semantic deployment guards. No real dotenv, model download, or cluster."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / 'deploy-k8s.sh'


class SemanticTests(unittest.TestCase):
    def setUp(self):
        self.deploy = types.ModuleType('deploy_semantic_test')
        code = SCRIPT.read_text().split("<<'PY'\n", 1)[1].rsplit('\nPY', 1)[0]
        exec(compile(code, str(SCRIPT), 'exec'), self.deploy.__dict__)
        self.raw = {'AGENTRECALL_SSH': 'root@192.0.2.10', 'AGENTRECALL_DOMAIN': 'offline.invalid',
                    'AGENTRECALL_IMAGE': 'agentrecall:test-unique', 'AGENTRECALL_IMAGE_CONFIG_DIGEST': 'sha256:' + 'a' * 64,
                    'AGENTRECALL_REPLICA_ID': 'test-replica', 'AGENTRECALL_MCP_TOKEN': 't' * 64,
                    'AGENTRECALL_CURSOR_HMAC_SECRET': 'c' * 64, 'AGENTRECALL_OWNER_ID': 'test-owner',
                    'AGENTRECALL_CLIENT_ID': 'test-client'}
        self.cfg = self.deploy.configuration(self.raw, 'semantic')
        self.sem = self.deploy.semantic_module(ROOT)
        self.sem.prepare(ROOT, self.cfg)
        self.resources = self.sem.resources(self.cfg)

    def test_lexical_default(self):
        self.assertEqual(self.deploy.configuration(self.raw)['PROFILE'], 'lexical')

    def test_dotenv_semantic_is_explicit(self):
        self.assertEqual(self.deploy.configuration({**self.raw, 'AGENTRECALL_PROFILE': 'semantic'})['PROFILE'], 'semantic')

    def test_cli_profile_overrides_dotenv(self):
        self.assertEqual(self.deploy.configuration({**self.raw, 'AGENTRECALL_PROFILE': 'semantic'}, 'lexical')['PROFILE'], 'lexical')

    def test_unknown_profile_refused(self):
        with self.assertRaises(self.deploy.DeployError):
            self.deploy.configuration({**self.raw, 'AGENTRECALL_PROFILE': 'semantic;touch bad'})

    def test_provider_cannot_be_silently_set(self):
        with self.assertRaises(self.deploy.DeployError):
            self.deploy.configuration({**self.raw, 'AGENTRECALL_EMBEDDING_PROVIDER': 'other'}, 'semantic')

    def test_semantic_render_keeps_egress_denied_and_readonly_cache(self):
        workload, ingress = self.deploy.render_manifests(ROOT, self.cfg)
        self.assertIn('value: bge-m3', workload)
        self.assertIn('value: /models', workload)
        self.assertIn('memory: 2Gi', workload)
        self.assertIn('failureThreshold: 300', workload)
        self.assertIn('initContainers:', workload)
        self.assertIn('--verify-only', workload)
        self.assertIn('egress: []', workload)
        self.assertIn('claimName": "agentrecall-models", "readOnly": true', workload)
        self.assertNotIn('"port": 443', workload)
        self.assertIn('pathType: Exact', ingress)
        self.assertNotIn(self.cfg['MCP_TOKEN'], workload + ingress)
        self.assertNotIn(self.cfg['CURSOR_HMAC_SECRET'], workload + ingress)

    def test_lexical_render_has_no_model_mount_or_init(self):
        workload, _ = self.deploy.render_manifests(ROOT, self.deploy.configuration(self.raw))
        self.assertNotIn('bge-m3', workload)
        self.assertNotIn('agentrecall-models', workload)
        self.assertNotIn('initContainers:', workload)
        self.assertIn('memory: 256Mi', workload)
        self.assertIn('memory: 1Gi', workload)

    def test_provision_job_has_no_live_database_or_secret(self):
        job = self.sem.job(self.cfg, self.cfg['MODEL_JOB'], '--provision')
        pod = job['spec']['template']['spec']
        text = json.dumps(job)
        self.assertNotIn('agentrecall-data', text)
        self.assertNotIn('agentrecall-runtime', text)
        self.assertNotIn('secretKeyRef', text)
        self.assertNotIn(self.cfg['MCP_TOKEN'], text)
        self.assertNotIn(self.cfg['CURSOR_HMAC_SECRET'], text)
        self.assertFalse(pod['automountServiceAccountToken'])
        self.assertEqual(pod['securityContext']['runAsUser'], 10001)
        self.assertEqual(job['spec']['backoffLimit'], 0)
        self.assertEqual(job['spec']['activeDeadlineSeconds'], 1200)
        self.assertEqual(pod['containers'][0]['resources']['limits']['memory'], '2Gi')
        self.assertTrue(pod['containers'][0]['securityContext']['readOnlyRootFilesystem'])

    def test_offline_job_is_readonly_and_network_fenced(self):
        job = self.sem.job(self.cfg, self.cfg['OFFLINE_JOB'], '--probe-offline')
        pod = job['spec']['template']['spec']
        self.assertEqual(pod['containers'][0]['command'][-1], '--probe-offline')
        self.assertTrue(pod['volumes'][0]['persistentVolumeClaim']['readOnly'])
        policy = next(r for r in self.resources if r['metadata']['name'] == self.sem.OFFLINE)
        self.assertEqual(policy['spec']['egress'], [])
        self.assertEqual(policy['spec']['ingress'], [])
        self.assertEqual(pod['containers'][0]['resources']['limits']['memory'], '2Gi')

    def test_download_policy_is_only_public_443_and_dns(self):
        policy = self.sem.model_network_policy()
        self.assertEqual(policy['spec']['podSelector']['matchLabels']['app'], self.sem.APP)
        self.assertNotEqual(self.sem.APP, 'agentrecall')
        dns, web = policy['spec']['egress']
        self.assertEqual(dns['ports'], [{'protocol': 'UDP', 'port': 53}, {'protocol': 'TCP', 'port': 53}])
        self.assertEqual(web['ports'], [{'protocol': 'TCP', 'port': 443}])
        self.assertIn('169.254.0.0/16', web['to'][0]['ipBlock']['except'])
        self.assertIn('10.0.0.0/8', web['to'][0]['ipBlock']['except'])

    def test_helper_configmap_immutable(self):
        helper = next(r for r in self.resources if r['kind'] == 'ConfigMap')
        self.assertTrue(helper['immutable'])
        self.assertEqual(helper['data']['model-cache.sh'], (ROOT / 'deploy/k3s/model-cache.sh').read_text())

    def test_contract_changes_with_image(self):
        other = dict(self.cfg, IMAGE='agentrecall:different-unique')
        self.sem.prepare(ROOT, other)
        self.assertNotEqual(self.cfg['MODEL_CONTRACT'], other['MODEL_CONTRACT'])

    def test_foreign_model_pvc_refused(self):
        desired = self.resources[0]
        existing = copy.deepcopy(desired)
        existing['metadata']['labels']['app'] = 'scout'
        with self.assertRaises(ValueError):
            self.sem.validate_resource(desired, existing)

    def test_wrong_model_storage_refused(self):
        desired = self.resources[0]
        existing = copy.deepcopy(desired)
        existing['spec']['accessModes'] = ['ReadWriteMany']
        with self.assertRaises(ValueError):
            self.sem.validate_resource(desired, existing)

    def test_altered_job_secret_mount_refused(self):
        desired = self.sem.job(self.cfg, self.cfg['MODEL_JOB'], '--provision')
        existing = copy.deepcopy(desired)
        existing['spec']['template']['spec']['volumes'].append({'name': 'secret', 'secret': {'secretName': 'agentrecall-runtime'}})
        with self.assertRaises(ValueError):
            self.sem.validate_resource(desired, existing)

    def test_altered_job_privileges_refused(self):
        desired = self.sem.job(self.cfg, self.cfg['MODEL_JOB'], '--provision')
        existing = copy.deepcopy(desired)
        existing['spec']['template']['spec']['containers'][0]['securityContext']['allowPrivilegeEscalation'] = True
        with self.assertRaises(ValueError):
            self.sem.validate_resource(desired, existing)

    def test_empty_policy_lists_may_be_omitted_by_api(self):
        desired = self.resources[2]
        existing = copy.deepcopy(desired)
        del existing['spec']['ingress']
        del existing['spec']['egress']
        self.sem.validate_resource(desired, existing)

    def test_missing_security_false_is_not_accepted(self):
        desired = self.sem.job(self.cfg, self.cfg['MODEL_JOB'], '--provision')
        existing = copy.deepcopy(desired)
        del existing['spec']['template']['spec']['containers'][0]['securityContext']['allowPrivilegeEscalation']
        with self.assertRaises(ValueError):
            self.sem.validate_resource(desired, existing)

    def model_state(self, both=True):
        state = {r['metadata']['name']: copy.deepcopy(r) for r in self.resources}
        state[self.sem.PVC]['status'] = {'phase': 'Bound'}
        for name, mode in [(self.cfg['MODEL_JOB'], '--provision'), (self.cfg['OFFLINE_JOB'], '--probe-offline')]:
            state[name] = self.sem.job(self.cfg, name, mode)
            state[name]['status'] = {'conditions': [{'type': 'Complete', 'status': 'True'}]}
        if not both:
            state[self.cfg['OFFLINE_JOB']] = None
        return state

    def test_no_rollout_without_completed_offline_inference(self):
        remote = mock.Mock()
        with mock.patch.object(self.sem, 'state', return_value=self.model_state(False)):
            with self.assertRaises(ValueError):
                self.sem.verify_cache_before_rollout(remote, self.cfg)
        remote.run.assert_not_called()

    def test_provisioning_repeat_skips_download_and_offline_job(self):
        remote = mock.Mock()
        remote.get.return_value = {'items': []}
        with mock.patch.object(self.sem, 'headroom'), mock.patch.object(self.sem, 'state', return_value=self.model_state()), \
                mock.patch.object(self.sem, 'create_job') as create, mock.patch.object(self.sem, 'verify_cache_before_rollout') as verify, \
                contextlib.redirect_stdout(io.StringIO()):
            self.sem.provision(remote, self.cfg)
        create.assert_not_called()
        verify.assert_called_once()
        remote.run.assert_not_called()

    def test_failed_model_job_not_retried_or_deleted(self):
        state = self.model_state()
        state[self.cfg['MODEL_JOB']]['status'] = {'conditions': [{'type': 'Failed', 'status': 'True'}]}
        remote = mock.Mock()
        remote.get.return_value = {'items': []}
        with mock.patch.object(self.sem, 'headroom'), mock.patch.object(self.sem, 'state', return_value=state):
            with self.assertRaises(ValueError):
                self.sem.provision(remote, self.cfg)
        remote.run.assert_not_called()

    def test_concurrent_model_job_refused(self):
        remote = mock.Mock()
        remote.get.return_value = {'items': [{'metadata': {'labels': {'app': self.sem.APP}}, 'status': {'active': 1}}]}
        with mock.patch.object(self.sem, 'headroom'), mock.patch.object(self.sem, 'state', return_value=self.model_state()):
            with self.assertRaises(ValueError):
                self.sem.provision(remote, self.cfg)
        remote.run.assert_not_called()

    def test_insufficient_memory_refused(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'available_memory': 1024**3, 'available_disk': 30 * 1024**3})
        with self.assertRaises(ValueError):
            self.sem.headroom(remote)

    def test_insufficient_disk_refused(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'available_memory': 5 * 1024**3, 'available_disk': 2 * 1024**3})
        with self.assertRaises(ValueError):
            self.sem.headroom(remote)

    def test_semantic_readiness_rejects_lexical_fallback(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'status': 'ready', 'replica_id': self.cfg['REPLICA_ID'], 'replication_configured': False, 'search_mode': 'lexical', 'search_engine': 'duckdb_fts_bm25'})
        with self.assertRaises(self.deploy.DeployError):
            self.deploy.private_readiness(remote, {'service': {'spec': {'clusterIP': '10.43.0.10'}}}, self.cfg)

    def test_semantic_exact_fallback_is_supported(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'status': 'ready', 'replica_id': self.cfg['REPLICA_ID'], 'replication_configured': False, 'search_mode': 'hybrid', 'search_engine': 'exact_scoped_vector_scan'})
        ready = self.deploy.private_readiness(remote, {'service': {'spec': {'clusterIP': '10.43.0.10'}}}, self.cfg)
        self.assertEqual(ready['search_mode'], 'hybrid')

    def test_semantic_unknown_engine_refused(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'status': 'ready', 'replica_id': self.cfg['REPLICA_ID'], 'replication_configured': False, 'search_mode': 'hybrid', 'search_engine': 'something-else'})
        with self.assertRaises(self.deploy.DeployError):
            self.deploy.private_readiness(remote, {'service': {'spec': {'clusterIP': '10.43.0.10'}}}, self.cfg)

    def test_readiness_identity_mismatch_refused(self):
        remote = mock.Mock()
        remote.run.return_value = json.dumps({'status': 'ready', 'replica_id': 'wrong', 'replication_configured': False, 'search_mode': 'hybrid', 'search_engine': 'duckdb_vss_hnsw'})
        with self.assertRaises(self.deploy.DeployError):
            self.deploy.private_readiness(remote, {'service': {'spec': {'clusterIP': '10.43.0.10'}}}, self.cfg)


if __name__ == '__main__':
    unittest.main()
