import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location('object_storage_prove', Path(__file__).with_name('prove.py'))
prove = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prove)


class Gates(unittest.TestCase):
    def test_exact_version(self):
        prove.require_engine_version('v2.0.1')
        for wrong in ('v2.0.0', '2.0.1', 'v2.0.1-dev', 'v2.0.10', ' v2.0.1'):
            with self.subTest(wrong=wrong), self.assertRaises(prove.Blocked):
                prove.require_engine_version(wrong)

    def test_explicit_amd64(self):
        prove.require_platform('linux/amd64')
        for wrong in ('linux/arm64', 'amd64', 'darwin/amd64', ''):
            with self.subTest(wrong=wrong), self.assertRaises(prove.Blocked):
                prove.require_platform(wrong)

    def test_missing_credentials_refuses_no_unsigned_fallback(self):
        with self.assertRaisesRegex(prove.Blocked, 'anonymous fallback is forbidden'):
            prove.require_r2_credentials({})

    def test_present_credentials_still_require_scope(self):
        with self.assertRaisesRegex(prove.Blocked, 'not authorized'):
            prove.require_r2_credentials({key: 'private-value' for key in ('POC_R2_ENDPOINT', 'POC_R2_BUCKET', 'POC_R2_KEY_ID', 'POC_R2_SECRET')})

    def test_settings_before_attach_selected_catalog(self):
        sql = prove.settings()
        for setting in ('threads=2', "memory_limit='256MiB'", 'duckdb_objfs_backend', 'duckdb_objfs_root', 'duckdb_objfs_memory_cache_size=67108864', 'duckdb_objfs_metadata_cache_size=16777216'):
            self.assertLess(sql.index(setting), sql.index('ATTACH'))
        self.assertLess(sql.index('ATTACH'), sql.index('USE object_db;'))
        self.assertIn('(READ_ONLY)', prove.settings(True))
        self.assertNotIn('unsigned', sql)

    def test_migrations_are_unmodified_between_boundaries(self):
        report = {'artifacts': {}}
        sql = prove.migration_sql(report)
        for filename in prove.MIGRATIONS:
            original = (prove.ROOT / 'migrations' / filename).read_bytes().decode('utf-8')
            self.assertIn('BEGIN TRANSACTION;\n' + original + '\nCOMMIT;\n', sql)
        self.assertEqual(len(report['artifacts']['migrations']), 3)

    def test_missing_exact_image_is_blocked(self):
        docker = mock.Mock()
        docker.run.side_effect = prove.ProofFailure('No such image')
        with self.assertRaisesRegex(prove.Blocked, 'prerequisite unavailable'):
            prove.gate(docker, prove.base_report('local', 'linux/amd64'))
        docker.sql.assert_not_called()

    def test_failed_signed_load_is_blocked(self):
        docker = mock.Mock()
        image = {'Os': 'linux', 'Architecture': 'amd64', 'Id': 'sha256:synthetic', 'Size': 1, 'Config': {'User': '10001:10001'}}
        docker.run.return_value = subprocess.CompletedProcess([], 0, json.dumps([image]), '')
        docker.sql.side_effect = prove.ProofFailure('signature invalid')
        with self.assertRaisesRegex(prove.Blocked, 'signed extension/runtime load'):
            prove.gate(docker, prove.base_report('local', 'linux/amd64'))


class Markers(unittest.TestCase):
    def test_exact_ack(self):
        self.assertTrue(prove.parse_ack('POC_ACK:token:7\r\n', 'token', 7))

    def test_misleading_ack_rejected(self):
        for line in ('prefix POC_ACK:token:7', 'POC_ACK:token:70', 'POC_ACK:other:7', 'POC_ACK:token:6', '"POC_ACK:token:7"', 'POC_ACK:token:7 trailing'):
            with self.subTest(line=line):
                self.assertFalse(prove.parse_ack(line, 'token', 7))

    def test_ack_is_after_commit(self):
        session = object.__new__(prove.Session)
        session.token = 'token'
        captured = []
        def marker(sql, expected):
            captured.append((sql, expected))
            return {'ack_monotonic_ns': time.monotonic_ns(), 'lines': []}
        session.marker = marker
        result = session.transaction(7)
        sql, expected = captured[0]
        self.assertLess(sql.index('BEGIN TRANSACTION;'), sql.index('INSERT INTO memories'))
        self.assertLess(sql.index('INSERT INTO service_meta'), sql.index('COMMIT;'))
        self.assertLess(sql.index('COMMIT;'), sql.index("SELECT 'POC_ACK:"))
        self.assertEqual(expected, 'POC_ACK:token:7')
        self.assertGreaterEqual(result['latency_ms'], 0)

    def test_write_failure_does_not_fabricate_ack(self):
        session = object.__new__(prove.Session)
        session.token = 'token'
        session.marker = mock.Mock(side_effect=prove.ProofFailure('COMMIT rejected'))
        with self.assertRaises(prove.ProofFailure):
            session.transaction(1)

    def test_exit_before_marker_rejected(self):
        session = object.__new__(prove.Session)
        session.process = mock.Mock()
        session.process.poll.return_value = 1
        with self.assertRaisesRegex(prove.ProofFailure, 'already exited'):
            session.marker('COMMIT;', 'ACK')

    def test_marker_timeout(self):
        session = object.__new__(prove.Session)
        session.process = mock.Mock()
        session.process.poll.return_value = None
        session.buffer = b''
        session.docker = mock.Mock(deadline=time.monotonic() - 1)
        with self.assertRaisesRegex(TimeoutError, 'No exact'):
            session.marker("SELECT 'ACK';", 'ACK', .01)


class Results(unittest.TestCase):
    def test_classification(self):
        for states, expected in (([], 'not_run'), (['passed'], 'passed'), (['passed', 'not_run'], 'not_run'), (['passed', 'blocked'], 'blocked'), (['failed', 'blocked'], 'failed')):
            self.assertEqual(prove.classify([{'status': state} for state in states]), expected)
        with self.assertRaises(ValueError):
            prove.classify([{'status': 'success'}])

    def test_state_requires_all_tables_and_sequence(self):
        self.assertEqual(prove.parse_state(['100,100,100,100,100,100,100,100'], 100)['joined_consistent_rows'], 100)
        for line in ('100,100,100,99,100,100,100,100', '100,100,100,100,99,100,100,100', '1,2', ''):
            with self.subTest(line=line), self.assertRaises(prove.ProofFailure):
                prove.parse_state([line], 100)

    def test_ambiguous_state_rejected(self):
        with self.assertRaises(prove.ProofFailure):
            prove.parse_state(['1,1,1,1,1,1,1,1'] * 2, 1)

    def test_statistics(self):
        self.assertEqual(prove.percentile(list(range(1, 101)), .95), 95)
        with self.assertRaises(ValueError):
            prove.percentile([], .95)

    def test_secret_safe_diagnostics(self):
        self.assertNotIn('fake-secret', prove.redact('failure fake-secret SECRET=private PASSWORD:other', ['fake-secret']))
        self.assertNotIn('private', prove.redact('SECRET=private'))
        self.assertEqual(len(prove.redact('x' * 20000)), 16000)

    def test_output_namespace(self):
        self.assertEqual(prove.output_path('target/cloud-poc/object-storage-local.json').name, 'object-storage-local.json')
        for wrong in ('poc/object-storage/report.json', 'target/cloud-poc/native-container.json', '/tmp/object-storage.json', 'target/cloud-poc/object-storage-local.json/../../other.json'):
            with self.subTest(wrong=wrong), self.assertRaises(ValueError):
                prove.output_path(wrong)

    def test_atomic_json_write(self):
        with tempfile.TemporaryDirectory(prefix='agentrecall-poc-obj-unit-') as temp:
            path = Path(temp) / 'report.json'
            prove.write_report(path, {'status': 'blocked'})
            self.assertEqual(json.loads(path.read_text()), {'status': 'blocked'})
            self.assertEqual([file.name for file in Path(temp).iterdir()], ['report.json'])

    def test_pending_checks_never_pass(self):
        report = prove.base_report('local', 'linux/amd64')
        self.assertEqual(prove.classify(report['checks']), 'not_run')
        self.assertEqual(report['measurements']['latency_ms_raw'], [])

    def test_blocked_emulator_starts_no_docker(self):
        with tempfile.TemporaryDirectory(prefix='agentrecall-poc-obj-unit-') as temp, mock.patch.object(prove, 'output_path', return_value=Path(temp) / 'report.json'), mock.patch.object(prove, 'Docker') as docker:
            result = prove.main(['--backend', 's3-local', '--platform', 'linux/amd64', '--output', 'unused'])
            self.assertEqual(result, 2)
            docker.assert_not_called()
            report = json.loads((Path(temp) / 'report.json').read_text())
            self.assertEqual(report['status'], 'blocked')
            self.assertTrue(all(item['status'] == 'not_run' for item in report['checks'][1:]))


class Lifecycle(unittest.TestCase):
    def test_caps_labels_and_network(self):
        docker = prove.Docker()
        args = docker.container_args(docker.name('writer'), docker.name('objects'))
        for literal in ('linux/amd64', '1g', '2', 'none', docker.label):
            self.assertIn(literal, args)
        self.assertTrue(docker.resources[0][1].startswith(prove.PREFIX))
        self.assertFalse(any('password' in value.lower() for value in args))

    def test_cleanup_owned_resources_after_failure(self):
        docker = prove.Docker()
        name = docker.name('writer')
        docker.resources = [('container', name)]
        inspected = subprocess.CompletedProcess([], 0, json.dumps([{'Config': {'Labels': {prove.PREFIX + 'owner': docker.run_id}}}]), '')
        removed = subprocess.CompletedProcess([], 0, name, '')
        with mock.patch.object(docker, 'run', side_effect=[inspected, removed]) as run:
            report = docker.cleanup()
        self.assertEqual(report['status'], 'passed')
        self.assertEqual(run.call_args_list[1].args[0], ['rm', '-f', name])

    def test_cleanup_already_removed_lowercase_is_success(self):
        docker = prove.Docker()
        docker.resources = [('container', docker.name('writer'))]
        absent = subprocess.CompletedProcess([], 1, '', 'error: no such object: writer')
        with mock.patch.object(docker, 'run', return_value=absent):
            self.assertEqual(docker.cleanup()['status'], 'passed')

    def test_main_failure_always_cleans_up(self):
        with tempfile.TemporaryDirectory(prefix='agentrecall-poc-obj-unit-') as temp, mock.patch.object(prove, 'output_path', return_value=Path(temp) / 'report.json'), mock.patch.object(prove, 'Docker') as constructor, mock.patch.object(prove, 'local_proof', side_effect=prove.ProofFailure('test injected failure')):
            constructor.return_value.cleanup.return_value = {'status': 'passed'}
            result = prove.main(['--backend', 'local', '--platform', 'linux/amd64', '--output', 'unused'])
            self.assertEqual(result, 1)
            constructor.return_value.cleanup.assert_called_once()
            report = json.loads((Path(temp) / 'report.json').read_text())
            self.assertEqual(report['status'], 'failed')
            self.assertEqual(report['cleanup']['status'], 'passed')

    def test_cleanup_never_removes_unowned(self):
        docker = prove.Docker()
        name = docker.name('writer')
        docker.resources = [('container', name)]
        inspected = subprocess.CompletedProcess([], 0, json.dumps([{'Config': {'Labels': {prove.PREFIX + 'owner': 'someone-else'}}}]), '')
        with mock.patch.object(docker, 'run', return_value=inspected) as run:
            report = docker.cleanup()
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(run.call_count, 1)

    def test_cleanup_reports_timeout(self):
        docker = prove.Docker()
        docker.resources = [('volume', docker.name('objects'))]
        with mock.patch.object(docker, 'run', side_effect=subprocess.TimeoutExpired('docker', 30)):
            self.assertEqual(docker.cleanup()['status'], 'failed')

    def test_command_timeout_explicit(self):
        docker = prove.Docker()
        with mock.patch.object(prove.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')) as run:
            docker.run(['version'], timeout=7)
        self.assertEqual(run.call_args.kwargs['timeout'], 7)
        self.assertEqual(run.call_args.kwargs['env']['DOCKER_CLI_HINTS'], 'false')

    def test_budget_stops_new_work(self):
        docker = prove.Docker(budget=-1)
        with mock.patch.object(prove.subprocess, 'run') as run, self.assertRaises(TimeoutError):
            docker.run(['version'])
        run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
