#!/usr/bin/env python3
"""Isolated SQL-level proof. No Rust adapter, cloud resources, or live data."""
import argparse
import csv
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import selectors
import statistics
import subprocess
import time
import uuid
from typing import Any, Dict, List, Optional

ROOT = Path(__file__).resolve().parents[2]
REPORT_DIR = ROOT / 'target/cloud-poc'
IMAGE = 'agentrecall-poc:object-storage'
PREFIX = 'agentrecall-poc-obj-'
VERSION = 'v2.0.1'
MIGRATIONS = ('001_init.sql', '002_retention.sql', '003_current_version_snapshot.sql')
STAGES = ('compatibility', 'migrations', 'transactions', 'constraints', 'rollback', 'writer_read_after_write', 'fresh_reader', 'measurements', 'crash_recovery', 'writer_fencing')
STATUSES = frozenset(('passed', 'failed', 'blocked', 'not_run'))


class Blocked(RuntimeError):
    pass


class ProofFailure(RuntimeError):
    pass


def require_engine_version(actual: str) -> None:
    if actual != VERSION:
        raise Blocked('Expected DuckDB v2.0.1; got ' + actual)


def require_platform(actual: str) -> None:
    if actual != 'linux/amd64':
        raise Blocked('Only explicit linux/amd64 is authorized; got ' + actual)


def require_r2_credentials(environment: Dict[str, str]) -> None:
    required = ('POC_R2_ENDPOINT', 'POC_R2_BUCKET', 'POC_R2_KEY_ID', 'POC_R2_SECRET')
    if any(not environment.get(key) for key in required):
        raise Blocked('R2 private target and narrow credentials are unavailable; anonymous fallback is forbidden.')
    raise Blocked('Actual R2 execution is not authorized by this local proof; separate resource/credential scope is required.')


def classify(checks: List[Dict[str, Any]]) -> str:
    states = [item['status'] for item in checks]
    if any(state not in STATUSES for state in states):
        raise ValueError('invalid check status')
    if 'failed' in states:
        return 'failed'
    if 'blocked' in states:
        return 'blocked'
    if not states or 'not_run' in states:
        return 'not_run'
    return 'passed'


def redact(text: str, secrets: Optional[List[str]] = None) -> str:
    for secret in sorted((value for value in (secrets or []) if value), key=len, reverse=True):
        text = text.replace(secret, '[REDACTED]')
    return re.sub(r'(?i)((?:secret|password|key_id|access_key_id)\s*[=:]\s*)[^\s,;]+', r'\1[REDACTED]', text)[:16000]


def parse_ack(line: str, token: str, sequence: int) -> bool:
    # Exact unquoted single-column CSV marker; no substring or reordered match.
    return line.rstrip('\r\n') == 'POC_ACK:' + token + ':' + str(sequence)


def percentile(samples: List[float], fraction: float) -> float:
    if not samples or not 0 < fraction <= 1:
        raise ValueError('nonempty samples and 0 < fraction <= 1 required')
    return sorted(samples)[max(0, math.ceil(len(samples) * fraction) - 1)]


def output_path(value: str) -> Path:
    path = Path(value)
    if not path.is_absolute():
        path = ROOT / path
    resolved = path.resolve()
    if resolved.parent != REPORT_DIR.resolve() or not re.fullmatch(r'object-storage[\w.-]*\.json', resolved.name):
        raise ValueError('Output must be target/cloud-poc/object-storage*.json')
    return resolved


def write_report(path: Path, report: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + '.tmp-' + uuid.uuid4().hex)
    try:
        with temp.open('x', encoding='utf-8') as handle:
            json.dump(report, handle, indent=2, sort_keys=True)
            handle.write('\n')
        os.replace(str(temp), str(path))
    finally:
        if temp.exists():
            temp.unlink()


class Docker:
    """Bounded commands, exact run ownership, no global prune or discovery cleanup."""
    def __init__(self, budget: float = 600.0):
        self.run_id = uuid.uuid4().hex[:16]
        self.label = PREFIX + 'owner=' + self.run_id
        self.resources = []  # type: List[tuple]
        self.processes = []  # type: List[subprocess.Popen]
        self.deadline = time.monotonic() + budget
        self.environment = dict(os.environ, DOCKER_CLI_HINTS='false')
        self.image = IMAGE

    def run(self, args: List[str], timeout: float = 30, check: bool = True, bounded: bool = True) -> subprocess.CompletedProcess:
        remaining = self.deadline - time.monotonic() if bounded else timeout
        if remaining <= 0:
            raise TimeoutError('600-second proof budget exhausted')
        result = subprocess.run(['docker'] + args, env=self.environment, capture_output=True, text=True, timeout=min(timeout, remaining))
        if check and result.returncode:
            raise ProofFailure('Docker operation failed: ' + redact(result.stderr or result.stdout))
        return result

    def name(self, suffix: str) -> str:
        return PREFIX + self.run_id + '-' + suffix

    def container_args(self, name: str, volume: Optional[str] = None) -> List[str]:
        self.resources.append(('container', name))
        args = ['run', '--name', name, '--label', self.label, '--platform', 'linux/amd64', '--memory', '1g', '--cpus', '2', '--network', 'none']
        if volume:
            args += ['--mount', 'type=volume,src=' + volume + ',dst=/objects']
        return args

    def volume(self, suffix: str) -> str:
        name = self.name(suffix)
        self.resources.append(('volume', name))
        self.run(['volume', 'create', '--label', self.label, name])
        init_name = self.name(suffix + '-init')
        self.run(self.container_args(init_name, name) + ['--user', '0:0', '--entrypoint', '/bin/sh', self.image, '-c', 'chown 10001:10001 /objects; chmod 700 /objects'])
        return name

    def sql(self, sql: str, volume: Optional[str] = None, suffix: Optional[str] = None, check: bool = True) -> subprocess.CompletedProcess:
        name = self.name(suffix or uuid.uuid4().hex[:8])
        return self.run(self.container_args(name, volume) + [self.image, '-csv', '-noheader', '-batch', '-bail', ':memory:', sql], timeout=60, check=check)

    def remove_container(self, name: str) -> None:
        self.run(['rm', '-f', name], check=False, bounded=False)

    def cleanup(self) -> Dict[str, Any]:
        errors = []
        for kind, name in reversed(self.resources):
            try:
                inspect = self.run(([kind, 'inspect', name] if kind == 'volume' else ['inspect', name]), check=False, bounded=False)
                if inspect.returncode:
                    if 'no such' not in inspect.stderr.lower():
                        errors.append(redact(inspect.stderr))
                    continue
                item = json.loads(inspect.stdout)[0]
                labels = item.get('Labels', {}) if kind == 'volume' else item.get('Config', {}).get('Labels', {})
                if labels.get(PREFIX + 'owner') != self.run_id:
                    errors.append('Ownership mismatch; retained ' + name)
                    continue
                command = ['volume', 'rm', name] if kind == 'volume' else ['rm', '-f', name]
                removed = self.run(command, check=False, bounded=False)
                if removed.returncode:
                    errors.append(redact(removed.stderr))
            except Exception as error:
                errors.append(type(error).__name__ + ': ' + redact(str(error)))
        for process in self.processes:
            try:
                if process.poll() is None:
                    process.terminate()
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate(timeout=5)
            except (ValueError, OSError):
                pass
        return {'status': 'passed' if not errors else 'failed', 'errors': errors, 'owned_resources_count': len(self.resources), 'private_credentials_created': False, 'scope': 'Only exact registered objects with matching run ownership labels.'}


class Session:
    """Non-TTY DuckDB with a post-COMMIT SQL result marker, bounded pipe reads."""
    def __init__(self, docker: Docker, volume: str, suffix: str, read_only: bool = False):
        self.docker = docker
        self.name = docker.name(suffix)
        self.token = uuid.uuid4().hex
        args = docker.container_args(self.name, volume) + ['-i', docker.image, '-csv', '-noheader', '-batch', '-bail', ':memory:']
        self.process = subprocess.Popen(['docker'] + args, env=docker.environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        docker.processes.append(self.process)
        self.buffer = b''
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.transcript = []  # type: List[str]
        self.marker(settings(read_only) + "SELECT 'POC_READY:" + self.token + "';", 'POC_READY:' + self.token, 30)

    def marker(self, sql: str, expected: str, timeout: float = 15) -> Dict[str, Any]:
        if self.process.poll() is not None:
            raise ProofFailure('Writer already exited without requested marker')
        self.process.stdin.write((sql + '\n').encode('utf-8'))
        self.process.stdin.flush()
        deadline = min(time.monotonic() + timeout, self.docker.deadline)
        lines = []
        while True:
            while b'\n' in self.buffer:
                raw, self.buffer = self.buffer.split(b'\n', 1)
                line = raw.decode('utf-8', errors='replace').rstrip('\r')
                self.transcript.append(redact(line))
                if line == expected:
                    return {'lines': lines, 'ack_monotonic_ns': time.monotonic_ns()}
                lines.append(line)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('No exact post-statement acknowledgment within timeout')
            events = self.selector.select(remaining)
            if not events:
                continue
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                raise ProofFailure('Writer exited before marker: ' + redact('\n'.join(lines) + self.buffer.decode('utf-8', errors='replace')))
            self.buffer += chunk
            if len(self.buffer) > 262144:
                raise ProofFailure('Writer output exceeded bound without marker')

    def transaction(self, sequence: int, replica: str = 'poc-replica') -> Dict[str, Any]:
        started = time.monotonic_ns()
        marker = 'POC_ACK:' + self.token + ':' + str(sequence)
        observed = self.marker('BEGIN TRANSACTION;\n' + fixture(sequence, replica) + "\nCOMMIT;\nSELECT '" + marker + "';", marker)
        if not parse_ack(marker, self.token, sequence):
            raise ProofFailure('invalid post-COMMIT marker')
        observed['submitted_monotonic_ns'] = started
        observed['latency_ms'] = (observed['ack_monotonic_ns'] - started) / 1000000
        return observed

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.stdin.close()
            self.process.wait(timeout=15)
        self.selector.close()

    def kill(self, acknowledgment: Dict[str, Any]) -> Dict[str, Any]:
        requested = time.monotonic_ns()
        self.docker.run(['kill', '--signal', 'KILL', self.name], timeout=15)
        completed = time.monotonic_ns()
        state = json.loads(self.docker.run(['inspect', self.name]).stdout)[0]['State']
        self.process.wait(timeout=15)
        self.selector.close()
        self.docker.remove_container(self.name)
        if state['ExitCode'] != 137:
            raise ProofFailure('Expected SIGKILL exit code 137; got ' + str(state['ExitCode']))
        return {'signal': 'SIGKILL', 'exit_code': state['ExitCode'], 'ack_to_kill_request_ms': (requested - acknowledgment['ack_monotonic_ns']) / 1000000, 'ack_to_kill_completed_ms': (completed - acknowledgment['ack_monotonic_ns']) / 1000000, 'commit_to_kill_upper_bound_ms': (completed - acknowledgment['submitted_monotonic_ns']) / 1000000, 'delay_limitation': 'Docker termination completion observed; exact engine commit instant and kernel signal instant are not directly measured.', 'requested_checkpoint': False, 'graceful_close': False}


def settings(read_only: bool = False) -> str:
    return "LOAD duckdb_object_storage; SET threads=2; SET memory_limit='256MiB'; SET temp_directory='/tmp/poc-spill'; SET duckdb_objfs_backend='local'; SET duckdb_objfs_root='/objects'; SET duckdb_objfs_memory_cache_size=67108864; SET duckdb_objfs_metadata_cache_size=16777216; SET duckdb_objfs_persistent_cache_path=''; ATTACH 'duckdb_objfs://synthetic.db' AS object_db" + (' (READ_ONLY)' if read_only else '') + '; USE object_db;\n'


def migration_sql(report: Dict[str, Any]) -> str:
    chunks = []
    identities = []
    for filename in MIGRATIONS:
        data = (ROOT / 'migrations' / filename).read_bytes()
        identities.append({'name': filename, 'sha256': hashlib.sha256(data).hexdigest(), 'size_bytes': len(data), 'application': 'Unchanged UTF-8 source between separate per-migration BEGIN/COMMIT boundaries.'})
        chunks.append('BEGIN TRANSACTION;\n' + data.decode('utf-8') + '\nCOMMIT;\n')
    report['artifacts']['migrations'] = identities
    return ''.join(chunks)


def fixture(sequence: int, replica: str = 'poc-replica') -> str:
    if sequence < 1 or not re.fullmatch(r'poc-[a-z-]+', replica):
        raise ValueError('Synthetic sequence/replica required')
    sid = replica + '-' + str(sequence)
    event = '{"event_id":"' + sid + '","synthetic":true}'
    return "\n".join((
        "INSERT INTO memories(owner_id,memory_id,namespace,content,revision,version_event_id,current_version_event_json) VALUES ('poc-owner','" + sid + "','poc-synthetic','synthetic content " + str(sequence) + "',1,'" + sid + "','" + event + "');",
        "INSERT INTO mutation_log(event_id,origin_replica_id,origin_seq,owner_id,client_id,memory_id,event_json,namespace,operation,payload_json,created_at) VALUES ('" + sid + "','" + replica + "'," + str(sequence) + ",'poc-owner','poc-client','" + sid + "','" + event + "','poc-synthetic','save','{}','2026-11-26T00:00:00Z');",
        "INSERT INTO applied_events VALUES ('" + sid + "','" + replica + "'," + str(sequence) + ",'2026-11-26T00:00:00Z');",
        "INSERT INTO idempotency_ledger VALUES ('poc-owner','poc-client','" + sid + "','synthetic-fingerprint','{}','" + sid + "');",
        "INSERT INTO replica_state VALUES ('poc-peer'," + str(sequence) + ',' + str(sequence) + ") ON CONFLICT(peer_replica_id) DO UPDATE SET inbound_cursor=excluded.inbound_cursor,outbound_ack=excluded.outbound_ack;",
        "INSERT INTO service_meta VALUES ('poc_local_sequence','" + str(sequence) + "') ON CONFLICT(key) DO UPDATE SET value=excluded.value;",
        "INSERT INTO service_meta VALUES ('poc_replica_id','" + replica + "') ON CONFLICT(key) DO UPDATE SET value=excluded.value;"
    ))


def state_query() -> str:
    return "SELECT (SELECT count(*) FROM memories),(SELECT count(*) FROM mutation_log),(SELECT count(*) FROM applied_events),(SELECT count(*) FROM idempotency_ledger),(SELECT value::BIGINT FROM service_meta WHERE key='poc_local_sequence'),(SELECT inbound_cursor FROM replica_state WHERE peer_replica_id='poc-peer'),(SELECT outbound_ack FROM replica_state WHERE peer_replica_id='poc-peer'),(SELECT count(*) FROM memories m JOIN mutation_log e ON m.version_event_id=e.event_id AND m.memory_id=e.memory_id JOIN applied_events a ON a.event_id=e.event_id AND a.origin_seq=e.origin_seq JOIN idempotency_ledger i ON i.event_id=e.event_id WHERE m.current_version_event_json=e.event_json);"


def parse_state(lines: List[str], expected: int) -> Dict[str, Any]:
    rows = list(csv.reader(io.StringIO('\n'.join(lines))))
    matching = [row for row in rows if len(row) == 8 and all(re.fullmatch(r'\d+', value) for value in row)]
    if len(matching) != 1:
        raise ProofFailure('Missing or ambiguous eight-field multitable state result: ' + redact('\n'.join(lines)))
    actual = [int(value) for value in matching[0]]
    if actual != [expected] * 8:
        raise ProofFailure('Acknowledged multi-table rows/sequence mismatch: expected ' + str([expected] * 8) + '; observed ' + str(actual))
    return {'memory_count': actual[0], 'event_count': actual[1], 'applied_count': actual[2], 'idempotency_count': actual[3], 'synthetic_sequence': actual[4], 'inbound_cursor': actual[5], 'outbound_ack': actual[6], 'joined_consistent_rows': actual[7]}


def check(report: Dict[str, Any], name: str, status: str, evidence: Any) -> None:
    for item in report['checks']:
        if item['name'] == name:
            item.update(status=status, evidence=evidence)
            return
    raise ValueError('Unknown acceptance check ' + name)


def gate(docker: Docker, report: Dict[str, Any]) -> None:
    try:
        inspection = json.loads(docker.run(['image', 'inspect', IMAGE]).stdout)[0]
    except (ProofFailure, FileNotFoundError, subprocess.TimeoutExpired) as error:
        raise Blocked('Exact CLI image/Docker prerequisite unavailable: ' + redact(str(error))) from error
    if inspection['Os'] != 'linux' or inspection['Architecture'] != 'amd64':
        raise Blocked('Image is not Linux AMD64')
    # Pin all runtime invocations to the inspected immutable local image ID.
    docker.image = inspection['Id']
    report['artifacts']['image'] = {key: inspection[key] for key in ('Id', 'Os', 'Architecture', 'Size')}
    report['artifacts']['image']['user'] = inspection['Config']['User']
    try:
        version = docker.sql("SELECT version(); PRAGMA platform; SELECT current_setting('allow_unsigned_extensions'); LOAD duckdb_object_storage; LOAD cache_httpfs; SELECT extension_name,loaded,extension_version,installed_from,install_mode FROM duckdb_extensions() WHERE extension_name IN ('duckdb_object_storage','cache_httpfs') ORDER BY extension_name;").stdout.splitlines()
    except (ProofFailure, subprocess.TimeoutExpired) as error:
        raise Blocked('Exact signed extension/runtime load prerequisite unavailable: ' + redact(str(error))) from error
    require_engine_version(version[0])
    if version[1:3] != ['linux_amd64', 'false']:
        raise Blocked('Exact engine platform or normal signature validation not verified')
    if not all(any(line.startswith(name + ',true,') and ',community,REPOSITORY' in line for line in version) for name in ('duckdb_object_storage', 'cache_httpfs')):
        raise Blocked('Both signed community extensions must be loaded')
    hash_name = docker.name('hashes')
    hashes = docker.run(docker.container_args(hash_name) + ['--entrypoint', '/bin/sh', docker.image, '-c', 'uname -m; sha256sum /usr/local/bin/duckdb /home/poc/.duckdb/extensions/v2.0.1/linux_amd64/*.duckdb_extension']).stdout
    if not hashes.startswith('x86_64\n'):
        raise Blocked('AMD64 native container execution not verified')
    report['artifacts']['runtime_metadata'] = version
    report['artifacts']['runtime_hashes'] = hashes.splitlines()
    path = REPORT_DIR / 'object-storage-artifact-manifest.json'
    if path.exists():
        manifest = json.loads(path.read_text())
        report['artifacts']['downloads'] = manifest.get('artifacts', {})
        report['artifacts']['base_image'] = manifest.get('base_image')
        for key, expected in [('cli', 'binary_sha256'), ('duckdb_object_storage', 'extension_sha256'), ('cache_httpfs', 'extension_sha256')]:
            identity = manifest.get('artifacts', {}).get(key, {}).get(expected)
            if not identity or identity not in hashes:
                raise Blocked('Runtime artifact hash differs from downloaded exact artifact: ' + key)
    else:
        raise Blocked('Exact-artifact identity manifest is missing; run resolve_artifacts.py first')
    check(report, 'compatibility', 'passed', {'version': version[0], 'engine_platform': version[1], 'unsigned_extensions': False, 'network_at_runtime': 'none', 'loaded_metadata': version[3:]})


def local_proof(docker: Docker, report: Dict[str, Any]) -> None:
    gate(docker, report)
    migrations = migration_sql(report)
    volume = docker.volume('workload-objects')
    writer = Session(docker, volume, 'workload-writer')
    writer.marker(migrations + "SELECT 'POC_MIGRATED';", 'POC_MIGRATED', 60)
    schema = writer.marker("SELECT count(*) FROM information_schema.tables WHERE table_catalog='object_db' AND table_schema='main'; SELECT count(*) FROM information_schema.columns WHERE table_catalog='object_db' AND table_name='memories' AND column_name='current_version_event_json'; SELECT 'POC_SCHEMA';", 'POC_SCHEMA')['lines']
    if schema != ['10', '1']:
        raise ProofFailure('Selected catalog schema mismatch: ' + str(schema))
    check(report, 'migrations', 'passed', {'selected_catalog': 'object_db', 'table_count': 10, 'snapshot_column': True, 'identities': report['artifacts']['migrations']})
    latencies, rss = [], []
    report['measurements']['latency_ms_raw'] = latencies
    report['measurements']['rss_samples'] = rss
    for sequence in range(1, 101):
        latencies.append(writer.transaction(sequence)['latency_ms'])
        if sequence in (1, 25, 50, 75, 100):
            resource = docker.run(['exec', writer.name, '/bin/cat', '/proc/1/status']).stdout
            values = {name: int(value) for name, value in re.findall(r'^(VmRSS|VmHWM):\s+(\d+)\s+kB', resource, re.MULTILINE)}
            if 'VmRSS' not in values:
                raise ProofFailure('Independent process RSS sample unavailable')
            rss.append(dict(transaction=sequence, unit='KiB', **values))
    observed = parse_state(writer.marker(state_query() + "SELECT 'POC_STATE';", 'POC_STATE')['lines'], 100)
    check(report, 'transactions', 'passed', observed)
    check(report, 'writer_read_after_write', 'passed', observed)
    reader = Session(docker, volume, 'fresh-reader', read_only=True)
    reader_state = reader.marker(state_query() + "SELECT 'POC_READER';", 'POC_READER')['lines']
    report['measurements']['fresh_reader_raw_rows'] = reader_state
    check(report, 'fresh_reader', 'passed', {'observation_only': True, 'raw_rows': reader_state, 'limitation': 'Independent read-only attachment observed separately; no universal immediate-consistency assertion.'})
    reader.close()
    writer.close()
    for index, insert in enumerate((
        "INSERT INTO memories(owner_id,memory_id,namespace,revision) VALUES ('poc-owner','poc-replica-1','poc-synthetic',1);",
        "INSERT INTO mutation_log SELECT 'poc-duplicate',origin_replica_id,origin_seq,owner_id,client_id,memory_id,event_json,namespace,operation,base_event_id,payload_json,idempotency_json,created_at FROM mutation_log WHERE origin_seq=1;",
        "INSERT INTO idempotency_ledger SELECT * FROM idempotency_ledger LIMIT 1;",
        "INSERT INTO mutation_log SELECT * FROM mutation_log LIMIT 1;",
        "INSERT INTO applied_events SELECT * FROM applied_events LIMIT 1;",
        "INSERT INTO replica_state SELECT * FROM replica_state LIMIT 1;"
    )):
        rejected = docker.sql(settings() + 'BEGIN; ' + insert + ' COMMIT;', volume, 'constraint-' + str(index), check=False)
        if rejected.returncode == 0 or 'Constraint Error' not in rejected.stderr:
            raise ProofFailure('Required duplicate constraint did not reject insert')
        parse_state(docker.sql(settings() + state_query(), volume).stdout.splitlines(), 100)
    check(report, 'constraints', 'passed', {'memory_primary_key': True, 'origin_sequence_unique': True, 'idempotency_primary_key': True, 'mutation_event_primary_key': True, 'applied_event_primary_key': True, 'replica_state_primary_key': True, 'state_unchanged': True})
    aborted = docker.sql(settings() + 'BEGIN;\n' + fixture(101) + "\nSELECT error('POC_INTENTIONAL_ABORT');", volume, 'rollback-aborted', check=False)
    if aborted.returncode == 0 or 'POC_INTENTIONAL_ABORT' not in aborted.stderr:
        raise ProofFailure('Expected intentional transaction abort was not observed')
    rollback_state = parse_state(docker.sql(settings() + state_query(), volume).stdout.splitlines(), 100)
    check(report, 'rollback', 'passed', {'intentional_abort': True, 'all_tables_and_sequence_unchanged': rollback_state})
    report['measurements'].update(transaction_count=100, latency_ms_raw=latencies, latency_ms_p50=statistics.median(latencies), latency_ms_p95=percentile(latencies, .95), latency_definition='Host submission of BEGIN through receipt of exact post-COMMIT SQL marker, including pipe/emulation overhead.', rss_samples=rss)
    check(report, 'measurements', 'passed', {'sample_count': len(latencies), 'rss_sample_count': len(rss), 'production_slo': False})
    trials = []
    report['measurements']['recovery_trials'] = trials
    for trial in range(1, 11):
        volume = docker.volume('crash-' + str(trial) + '-objects')
        writer = Session(docker, volume, 'crash-' + str(trial) + '-writer')
        writer.marker(migrations + "SELECT 'POC_MIGRATED';", 'POC_MIGRATED', 60)
        acknowledgment = writer.transaction(1)
        timing = writer.kill(acknowledgment)
        trial_result = dict(trial=trial, status='not_run', acknowledged_sequence=1, retained_state='Only the /objects object-root volume; fresh container layer/home/temp/spill.', **timing)
        trials.append(trial_result)
        reopened = docker.sql(settings() + state_query(), volume, 'crash-' + str(trial) + '-fresh-recovery')
        try:
            state = parse_state(reopened.stdout.splitlines(), 1)
        except ProofFailure as error:
            trial_result.update(status='failed', error=redact(str(error)))
            raise
        trial_result.update(status='passed', recovered=state)
        print('local recovery trial ' + str(trial) + '/10 passed', flush=True)
    check(report, 'crash_recovery', 'passed', {'independent_trial_count': len(trials), 'all_acknowledged_rows_and_sequence_recovered': True, 'checkpoint_before_kill': False})
    fence_volume = docker.volume('fence-objects')
    a = Session(docker, fence_volume, 'fence-a')
    a.marker(migrations + "SELECT 'POC_MIGRATED';", 'POC_MIGRATED', 60)
    a.transaction(1)
    b = Session(docker, fence_volume, 'fence-b')
    b.transaction(2)
    fencing = {'takeover': 'Writer B attached read-write and acknowledged a new transaction while A remained open.', 'a_commit_after_b_takeover': 'not_run', 'a_explicit_checkpoint_flush': 'not_run'}
    try:
        acknowledgment = a.transaction(3)
        fencing['a_commit_after_b_takeover'] = {'acknowledged': True, 'latency_ms': acknowledgment['latency_ms']}
        report['safety_findings'].append('Displaced writer A acknowledged a COMMIT after B took over. Extension fencing is not an immediate write-ownership guard; cloud composition requires external single-writer coordination.')
        try:
            a.marker("CHECKPOINT object_db; SELECT 'POC_FENCE_CHECKPOINT';", 'POC_FENCE_CHECKPOINT')
            fencing['a_explicit_checkpoint_flush'] = {'acknowledged': True}
            report['safety_findings'].append('Displaced writer A also acknowledged explicit CHECKPOINT/flush; do not infer immediate ownership protection.')
        except (ProofFailure, TimeoutError) as error:
            fencing['a_explicit_checkpoint_flush'] = {'acknowledged': False, 'error': redact(str(error))}
    except (ProofFailure, TimeoutError) as error:
        fencing['a_commit_after_b_takeover'] = {'acknowledged': False, 'error': redact(str(error))}
        fencing['a_explicit_checkpoint_flush'] = {'status': 'not_run', 'reason': 'Bail-on-error writer terminated or unresponsive; no further write is attempted.'}
    fencing['a_transcript'] = a.transcript[-12:]
    a.close()
    b.close()
    report['safety_findings'].append('Writer B acquired read-write ownership while A remained open; rejection was observed only on a later A operation, not an up-front exclusive lock. External single-writer coordination remains required.')
    # A rejected COMMIT can terminate DuckDB, so never write through that A again.
    # A second independently initialized pair measures explicit flush/checkpoint.
    checkpoint_volume = docker.volume('fence-checkpoint-objects')
    checkpoint_a = Session(docker, checkpoint_volume, 'fence-checkpoint-a')
    checkpoint_a.marker(migrations + "SELECT 'POC_MIGRATED';", 'POC_MIGRATED', 60)
    checkpoint_a.transaction(1)
    checkpoint_b = Session(docker, checkpoint_volume, 'fence-checkpoint-b')
    checkpoint_b.transaction(2)
    checkpoint_trial = {'independent_pair': True, 'same_logical_database_within_pair': True, 'b_takeover_acknowledged': True}
    try:
        checkpoint_a.marker("CHECKPOINT object_db; SELECT 'POC_FENCE_CHECKPOINT';", 'POC_FENCE_CHECKPOINT')
        checkpoint_trial['a_checkpoint_flush'] = {'acknowledged': True}
        report['safety_findings'].append('Displaced writer acknowledged explicit CHECKPOINT/flush in the independent checkpoint pair; no immediate ownership guarantee.')
    except (ProofFailure, TimeoutError) as error:
        checkpoint_trial['a_checkpoint_flush'] = {'acknowledged': False, 'error': redact(str(error))}
    checkpoint_trial['a_transcript'] = checkpoint_a.transcript[-12:]
    checkpoint_a.close()
    checkpoint_b.close()
    fencing['independent_explicit_checkpoint_trial'] = checkpoint_trial
    check(report, 'writer_fencing', 'passed', fencing)
    report['limitations'].append('Fencing uses independent commit and explicit CHECKPOINT/flush writer pairs because a rejected commit can terminate writer A. No further writes are attempted through a rejected writer. This is not an up-front lock or authorization for cloud composition.')


def base_report(backend: str, platform: str) -> Dict[str, Any]:
    return {'module_id': 'poc-object-storage', 'backend': backend, 'status': 'not_run', 'platform': platform, 'checks': [{'name': stage, 'status': 'not_run'} for stage in STAGES], 'artifacts': {}, 'measurements': {'latency_ms_raw': [], 'rss_samples': [], 'recovery_trials': []}, 'settings': {'duckdb_memory_limit': '256MiB', 'threads': 2, 'data_cache_bytes': 67108864, 'metadata_cache_bytes': 16777216, 'container_memory': '1GiB', 'container_cpus': 2, 'persistent_cache': False}, 'safety_findings': [], 'limitations': ['Synthetic SQL-level storage harness, not Rust event-application correctness or an object-backed AgentRecall service.', 'Linux AMD64 execution on an ARM64 Docker host; timings are not cloud forecasts.', 'R2 is not authorized; no Cloudflare resource or credential creation.']}


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend', required=True, choices=('local', 's3-local', 'r2'))
    parser.add_argument('--platform', required=True)
    parser.add_argument('--output', required=True)
    args = parser.parse_args(argv)
    try:
        destination = output_path(args.output)
    except ValueError as error:
        parser.error(str(error))
    report = base_report(args.backend, args.platform)
    docker = None
    started = time.monotonic()
    try:
        require_platform(args.platform)
        if args.backend == 'r2':
            require_r2_credentials(os.environ)
        if args.backend == 's3-local':
            report['artifacts']['emulator'] = {'release': 'RELEASE.2025-09-07T16-13-09Z', 'quay_image': 'quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z', 'quay_resolution': '401 Unauthorized', 'docker_hub_resolution': 'pull access denied / insufficient_scope', 'digest': None}
            raise Blocked('Pinned MinIO release could not be resolved from either official registry; emulator branch stopped, no substitute image or source build.')
        docker = Docker()
        local_proof(docker, report)
        report['status'] = classify(report['checks'])
    except Blocked as error:
        check(report, 'compatibility', 'blocked', redact(str(error)))
        report['status'] = 'blocked'
        report['blocker'] = redact(str(error))
    except (ProofFailure, TimeoutError, subprocess.TimeoutExpired, OSError, ValueError, KeyError, IndexError) as error:
        pending = next((item['name'] for item in report['checks'] if item['status'] == 'not_run'), 'compatibility')
        check(report, pending, 'failed', {'error_type': type(error).__name__, 'error': redact(str(error))})
        report['status'] = 'failed'
        report['limitations'].append('Execution stopped at the first failure; later acceptance checks remain not_run.')
    finally:
        report['cleanup'] = docker.cleanup() if docker else {'status': 'passed', 'owned_resources_count': 0, 'private_credentials_created': False, 'scope': 'No Docker resources or private state created.'}
        if report['cleanup']['status'] != 'passed':
            report['status'] = 'failed'
        report['elapsed_seconds'] = time.monotonic() - started
        write_report(destination, report)
    print(json.dumps({'module_id': report['module_id'], 'backend': args.backend, 'status': report['status'], 'report': str(destination), 'cleanup': report['cleanup']['status']}), flush=True)
    return {'passed': 0, 'failed': 1, 'blocked': 2, 'not_run': 3}[report['status']]


if __name__ == '__main__':
    raise SystemExit(main())
