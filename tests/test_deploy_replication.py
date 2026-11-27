"""Offline pairing tests: synthetic identities/secrets only; no live services."""
import base64
import copy
import hashlib
import json
from pathlib import Path
import unittest
import test_deploy_k8s as baseline

TOKEN, CURSOR, ROOT = baseline.TOKEN, baseline.CURSOR, baseline.ROOT


class ReplicationTests(unittest.TestCase):
    def setUp(self):
        fixture = baseline.DeployGuardTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.module, self.raw, self.state = fixture.module, fixture.raw, fixture.state
        self.raw.update(AGENTRECALL_REPLICATION_TOKEN='r' * 64,
                        AGENTRECALL_TRUSTED_REPLICA_ID='local-stable')
        self.cfg = self.module.configuration(self.raw)
        self.helper = self.module.replication_module()

    def reject(self, **changes):
        with self.assertRaises(self.module.DeployError):
            self.module.configuration({**self.raw, **changes})

    def paired_state(self):
        self.state['replication_secret'] = self.helper.secret_manifest(self.cfg)
        self.state['deployment']['spec']['template']['spec']['containers'][0]['env'] += self.helper.receiver_env(self.cfg)
        backend = {'service': {'name': 'agentrecall', 'port': {'number': 8789}}}
        self.state['ingress'] = {'metadata': {'annotations': {
            'traefik.ingress.kubernetes.io/router.entrypoints': 'websecure',
            'traefik.ingress.kubernetes.io/router.tls': 'true'}}, 'spec': {
            'ingressClassName': 'traefik', 'tls': [{'secretName': 'agentrecall-tls'}],
            'rules': [{'host': self.cfg['DOMAIN'], 'http': {'paths': [
                {'path': p, 'pathType': 'Exact', 'backend': backend}
                for p in ('/mcp', self.helper.EXCHANGE_PATH)]}}]}}

    def test_complete_pair_is_supported(self):
        self.assertTrue(self.cfg['REPLICATION_ENABLED'])
        self.assertEqual(self.cfg['TRUSTED_REPLICA_ID'], 'local-stable')

    def test_token_without_identity_refused(self):
        self.reject(AGENTRECALL_TRUSTED_REPLICA_ID='')

    def test_identity_without_token_refused(self):
        self.reject(AGENTRECALL_REPLICATION_TOKEN='')

    def test_reused_secrets_refused(self):
        for token in (TOKEN, CURSOR):
            with self.subTest(token_length=len(token)):
                self.reject(AGENTRECALL_REPLICATION_TOKEN=token)

    def test_short_or_placeholder_secret_refused(self):
        for token in ('short', 'replace-' + 'r' * 50, 'r' * 32 + ' ', '\n' + 'r' * 40):
            with self.subTest(length=len(token)):
                self.reject(AGENTRECALL_REPLICATION_TOKEN=token)

    def test_peer_outbound_configuration_refused(self):
        for suffix in ('URL', 'TOKEN', 'REPLICA_ID'):
            self.reject(**{'AGENTRECALL_PEER_' + suffix: 'not-permitted'})

    def test_unknown_replication_setting_refused(self):
        self.reject(AGENTRECALL_REPLICATION_EXTRA='unexpected')

    def test_own_identity_and_injected_identity_refused(self):
        for identity in (self.cfg['REPLICA_ID'], 'bad\nvalue', 'replace-local'):
            self.reject(AGENTRECALL_TRUSTED_REPLICA_ID=identity)

    def test_dedicated_secret_does_not_modify_runtime(self):
        before = copy.deepcopy(self.state['secret'])
        secret = self.helper.secret_manifest(self.cfg)
        self.assertEqual(secret['metadata']['name'], 'agentrecall-replication')
        self.assertEqual(set(secret['data']), {'replication-token'})
        self.assertEqual(base64.b64decode(secret['data']['replication-token']), b'r' * 64)
        self.assertEqual(before, self.state['secret'])

    def test_pairing_secret_is_not_rotated(self):
        self.paired_state()
        self.state['replication_secret']['data']['replication-token'] = base64.b64encode(b'x' * 64).decode()
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_foreign_pairing_secret_refused(self):
        self.paired_state()
        self.state['replication_secret']['metadata']['labels']['app'] = 'scout'
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_malformed_secret_refused(self):
        secret = self.helper.secret_manifest(self.cfg)
        secret['data']['replication-token'] = 'invalid!'
        with self.assertRaises(ValueError):
            self.helper.validate_secret(secret, self.cfg)

    def test_initial_receiver_transition_allowed(self):
        self.module.validate_existing(self.state, self.cfg)

    def test_matching_existing_receiver_allowed(self):
        self.paired_state()
        self.module.validate_existing(self.state, self.cfg)

    def test_receiver_secret_cannot_be_regenerated(self):
        self.paired_state()
        self.state['replication_secret'] = None
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_receiver_cannot_be_silently_disabled(self):
        self.paired_state()
        disabled = self.module.configuration({k: v for k, v in self.raw.items()
                                             if k not in ('AGENTRECALL_REPLICATION_TOKEN', 'AGENTRECALL_TRUSTED_REPLICA_ID')})
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, disabled)

    def test_receiver_trusted_identity_cannot_change(self):
        self.paired_state()
        cfg = self.module.configuration({**self.raw, 'AGENTRECALL_TRUSTED_REPLICA_ID': 'another-local'})
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, cfg)

    def test_render_has_only_secret_reference_and_exact_https_route(self):
        workload, ingress = self.module.render_manifests(ROOT, self.cfg)
        self.assertNotIn('r' * 64, workload + ingress)
        self.assertIn('name: agentrecall-replication', workload)
        self.assertNotIn('AGENTRECALL_PEER_', workload)
        self.assertIn('egress: []', workload)
        self.assertEqual(ingress.count('pathType: Exact'), 2)
        self.assertIn('router.entrypoints: websecure', ingress)
        self.assertNotIn('/health', ingress)

    def test_prefix_receiver_ingress_refused(self):
        self.paired_state()
        self.state['ingress']['spec']['rules'][0]['http']['paths'][1]['pathType'] = 'Prefix'
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_plaintext_ingress_refused(self):
        self.paired_state()
        self.state['ingress']['metadata']['annotations']['traefik.ingress.kubernetes.io/router.entrypoints'] = 'web'
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_extra_route_refused(self):
        self.paired_state()
        paths = self.state['ingress']['spec']['rules'][0]['http']['paths']
        paths.append({**paths[0], 'path': '/metrics'})
        with self.assertRaises(self.module.DeployError):
            self.module.validate_existing(self.state, self.cfg)

    def test_cue_credential_is_distinct_and_shared_owner(self):
        cfg = self.module.configuration({**self.raw, 'AGENTRECALL_CUE_MCP_TOKEN': 'q' * 64,
                                         'AGENTRECALL_CUE_OWNER_ID': 'jacob'})
        records = json.loads(base64.b64decode(self.module.secret_manifest(cfg)['data']['tokens.json']))
        self.assertEqual(len(records), 2)
        self.assertEqual(records[0]['client_id'], self.cfg['CLIENT_ID'])
        self.assertEqual(records[1]['owner_id'], 'jacob')
        self.assertEqual(records[1]['allowed_namespaces'], ['shared'])
        self.assertNotIn('q' * 64, json.dumps(records))

    def test_incomplete_or_reused_cue_credential_refused(self):
        self.reject(AGENTRECALL_CUE_MCP_TOKEN='q' * 64)
        self.reject(AGENTRECALL_CUE_MCP_TOKEN=TOKEN, AGENTRECALL_CUE_OWNER_ID='jacob')

    def test_semantic_pair_preserves_readonly_models(self):
        cfg = self.module.configuration(self.raw, 'semantic')
        workload, ingress = self.module.render_manifests(ROOT, cfg)
        self.assertIn('AGENTRECALL_EMBEDDING_PROVIDER', workload)
        self.assertIn('mountPath: /models\n              readOnly: true', workload)
        self.assertIn(self.helper.EXCHANGE_PATH, ingress)


if __name__ == '__main__':
    unittest.main()
