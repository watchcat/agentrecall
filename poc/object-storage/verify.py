#!/usr/bin/env python3
"""Verify sanitized reports and module tests; remove only owned download/cache staging."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time

SOURCE = Path(__file__).resolve().parent
ROOT = SOURCE.parents[1]
OUTPUT = ROOT / 'target/cloud-poc'
SPEC = importlib.util.spec_from_file_location('object_storage_proof', SOURCE / 'prove.py')
proof = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proof)


def command(args, environment, timeout=30):
    return subprocess.run(args, env=environment, cwd=str(ROOT), capture_output=True, text=True, timeout=timeout)


def main():
    environment = dict(os.environ, PYTHONPYCACHEPREFIX=str(OUTPUT / 'object-storage-pycache'), DOCKER_CLI_HINTS='false')
    report = {'module_id': 'poc-object-storage', 'workspace': str(ROOT), 'prepared_worktree': False, 'copy_needed': False, 'status': 'failed', 'tests': {}, 'report_invariants': {}, 'source_scope': {}, 'cleanup': {}}
    try:
        tests = {
            'syntax': ['python3', '-m', 'py_compile', str(SOURCE / 'prove.py'), str(SOURCE / 'resolve_artifacts.py'), str(SOURCE / 'test_prove.py'), str(SOURCE / 'verify.py')],
            'unit_tests': ['python3', '-m', 'unittest', 'discover', '-s', str(SOURCE), '-p', 'test_*.py', '-v'],
            'help': ['python3', str(SOURCE / 'prove.py'), '--help'],
            'git_diff_check': ['git', 'diff', '--check']
        }
        for name, args in tests.items():
            result = command(args, environment)
            report['tests'][name] = {'status': 'passed' if result.returncode == 0 else 'failed', 'returncode': result.returncode, 'stdout': proof.redact(result.stdout), 'stderr': proof.redact(result.stderr)}
            if result.returncode:
                raise RuntimeError(name + ' failed')
        files = [file for file in SOURCE.iterdir() if file.is_file()]
        for file in files:
            result = command(['git', 'diff', '--no-index', '--check', '/dev/null', str(file)], environment)
            # --no-index returns 1 for a new-file difference, even with no errors.
            if result.returncode not in (0, 1) or result.stdout or result.stderr:
                raise RuntimeError('Whitespace check failed: ' + str(file) + ' ' + result.stdout + result.stderr)
        report['tests']['untracked_source_whitespace'] = {'status': 'passed', 'files': [str(file) for file in files], 'accepted_no_index_new_file_difference_exit': 1}
        local = json.loads((OUTPUT / 'object-storage-local.json').read_text())
        if local['status'] != 'passed' or not all(check['status'] == 'passed' for check in local['checks']):
            raise RuntimeError('Local acceptance checks did not all pass')
        samples = local['measurements']['latency_ms_raw']
        if len(samples) != 100 or statistics.median(samples) != local['measurements']['latency_ms_p50'] or proof.percentile(samples, .95) != local['measurements']['latency_ms_p95']:
            raise RuntimeError('Invalid latency samples/percentiles')
        trials = local['measurements']['recovery_trials']
        if len(trials) != 10 or not all(trial['status'] == 'passed' and trial['signal'] == 'SIGKILL' and trial['exit_code'] == 137 and not trial['requested_checkpoint'] and not trial['graceful_close'] for trial in trials):
            raise RuntimeError('Invalid independent crash trial evidence')
        if local['cleanup']['status'] != 'passed':
            raise RuntimeError('Local cleanup did not pass')
        for backend in ('s3-local', 'r2'):
            data = json.loads((OUTPUT / ('object-storage-' + backend + '.json')).read_text())
            if data['status'] != 'blocked' or not all(check['status'] == 'not_run' for check in data['checks'][1:]):
                raise RuntimeError('Blocked backend was misclassified')
            if data['measurements']['latency_ms_raw'] or data['measurements']['rss_samples'] or data['measurements']['recovery_trials']:
                raise RuntimeError('Unexecuted backend has fabricated samples')
        report['report_invariants'] = {'status': 'passed', 'local': 'passed', 's3-local': 'blocked', 'r2': 'blocked / actual_execution not_run', 'raw_latency_count': len(samples), 'independent_recovery_count': len(trials), 'rss_sample_count': len(local['measurements']['rss_samples']), 'unexecuted_backend_samples': 0}
        changed = command(['git', 'diff', '--name-only'], environment)
        changed.check_returncode()
        tracked = changed.stdout.splitlines()
        head = command(['git', 'rev-parse', 'HEAD'], environment)
        head.check_returncode()
        report['source_scope'] = {'tracked_changed_files': tracked, 'own_added_files': [str(file) for file in files], 'head': head.stdout.strip(), 'no_commit_or_push': True, 'existing_source_migrations_cargo_config_ci_unchanged': not tracked}
        for key, args in (
            ('containers', ['docker', 'ps', '-a', '--filter', 'name=agentrecall-poc-obj-', '--format', '{{.Names}}']),
            ('volumes', ['docker', 'volume', 'ls', '--filter', 'label=agentrecall-poc-obj-owner', '--format', '{{.Name}}'])
        ):
            result = command(args, environment, 20)
            result.check_returncode()
            inventory = result.stdout.splitlines()
            report['cleanup'][key] = inventory
            if inventory:
                raise RuntimeError('Owned Docker inventory not empty: ' + key)
        report['status'] = 'passed'
    except Exception as error:
        report['error_type'] = type(error).__name__
        report['error'] = proof.redact(str(error))
    finally:
        removed, errors = [], []
        for name in ('object-storage-artifact-stage', 'object-storage-pycache'):
            path = OUTPUT / name
            try:
                if path.exists():
                    if path.is_symlink():
                        raise RuntimeError('Refuse symlink staging cleanup')
                    shutil.rmtree(path)
                    removed.append(str(path))
            except Exception as error:
                errors.append(proof.redact(str(error)))
        report['cleanup'].update(status='failed' if errors else 'passed', errors=errors, removed_private_staging=removed, private_credentials_generated=False, public_cli_image_retained=True, no_unattended_jobs=True)
        if errors:
            report['status'] = 'failed'
        proof.write_report(OUTPUT / 'object-storage-verification.json', report)
    print(json.dumps({'status': report['status'], 'tests': {key: value['status'] for key, value in report['tests'].items()}, 'cleanup': report['cleanup'], 'workspace': str(ROOT), 'copy_needed': False}, indent=2))
    return 0 if report['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
