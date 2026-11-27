#!/usr/bin/env bash
# Native AgentRecall -> existing single-node AMD64 k3s. See deploy/k3s/README.md.
# Secrets are read as data from .env, never sourced, printed or passed in argv.
set +x
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
command -v python3 >/dev/null 2>&1 || { echo 'python3 is required' >&2; exit 1; }
exec python3 - "$ROOT" "$@" <<'PY'
import argparse
import base64
import hashlib
import hmac
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import socket
import ssl
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

NS = 'agentrecall'
DEFAULT_DOMAIN = 'memory.all-your-base.net'
SCOPES = {'memory:read', 'memory:write', 'memory:delete'}
TOOLS = {'memory_save', 'memory_get', 'memory_search', 'memory_update', 'memory_delete'}


class DeployError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise DeployError(message)


def load_env(path):
    try:
        info = path.lstat()
        require(stat.S_ISREG(info.st_mode) and not path.is_symlink(), '.env must be a regular, non-symlink file')
        require(info.st_uid == os.geteuid(), '.env must be owned by the current user')
        require(stat.S_IMODE(info.st_mode) == 0o600, '.env must have permissions 0600 (chmod 600)')
        require(info.st_size <= 65536, '.env is unexpectedly large')
        lines = path.read_text(encoding='utf-8').splitlines()
    except (OSError, UnicodeError):
        raise DeployError('Cannot read private environment file') from None
    result = {}
    for number, line in enumerate(lines, 1):
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        if line.startswith('export '):
            line = line[7:].lstrip()
        match = re.fullmatch(r'([A-Z][A-Z0-9_]*)\s*=(.*)', line)
        require(match is not None, 'Malformed .env assignment at line ' + str(number))
        key, value = match.groups()
        require(key not in result, 'Duplicate .env key ' + key)
        value = value.strip()
        if value[:1] in ('"', "'"):
            require(len(value) >= 2 and value[-1] == value[0], 'Unmatched .env quote at line ' + str(number))
            value = value[1:-1]
        result[key] = value
    return result


def configuration(values, profile=None):
    cfg = {'PROFILE': profile if profile is not None else values.get('AGENTRECALL_PROFILE', 'lexical')}
    require(cfg['PROFILE'] in ('lexical', 'semantic'), 'AGENTRECALL_PROFILE must be lexical or semantic')
    defaults = {'NAMESPACES': 'shared', 'SCOPES': 'memory:read,memory:write,memory:delete',
                'CLUSTER_ISSUER': 'letsencrypt', 'IMAGE_CONFIG_DIGEST': '', 'CA_BUNDLE': ''}
    for name in ('SSH', 'DOMAIN', 'IMAGE', 'IMAGE_CONFIG_DIGEST', 'REPLICA_ID', 'MCP_TOKEN',
                 'CURSOR_HMAC_SECRET', 'OWNER_ID', 'CLIENT_ID', 'NAMESPACES', 'SCOPES', 'CLUSTER_ISSUER', 'CA_BUNDLE'):
        key = 'AGENTRECALL_' + name
        value = values.get(key, defaults.get(name, ''))
        if name not in ('IMAGE_CONFIG_DIGEST', 'CA_BUNDLE'):
            require(bool(value), 'Missing required setting ' + key)
        cfg[name] = value
    ident = r'[A-Za-z0-9][A-Za-z0-9_.@-]{0,127}'
    hostname = r'(?=.{1,253}$)(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)*[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?'
    require(re.fullmatch(r'(?:[A-Za-z0-9_][A-Za-z0-9_.-]*@)?' + hostname, cfg['SSH']) is not None, 'Invalid AGENTRECALL_SSH')
    require(re.fullmatch(hostname, cfg['DOMAIN']) is not None and '.' in cfg['DOMAIN'], 'Invalid AGENTRECALL_DOMAIN')
    require(re.fullmatch(r'agentrecall:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}', cfg['IMAGE']) is not None
            and cfg['IMAGE'].split(':')[1].lower() != 'latest', 'AGENTRECALL_IMAGE must be a unique agentrecall:tag, not latest')
    require(not cfg['IMAGE_CONFIG_DIGEST'] or re.fullmatch(r'sha256:[a-f0-9]{64}', cfg['IMAGE_CONFIG_DIGEST']) is not None,
            'Invalid AGENTRECALL_IMAGE_CONFIG_DIGEST (use CRI/config digest)')
    for name in ('REPLICA_ID', 'OWNER_ID', 'CLIENT_ID'):
        require(re.fullmatch(ident, cfg[name]) is not None and not cfg[name].lower().startswith('replace'), 'Invalid AGENTRECALL_' + name)
    for name in ('MCP_TOKEN', 'CURSOR_HMAC_SECRET'):
        value = cfg[name]
        require(len(value) >= 32 and all(33 <= ord(c) <= 126 for c in value)
                and not value.lower().startswith(('replace', 'changeme', '<')), 'Invalid AGENTRECALL_' + name + ': use a persistent random secret')
    for name in ('NAMESPACES', 'SCOPES'):
        parts = cfg[name].split(',')
        require(all(parts), 'Empty entry in AGENTRECALL_' + name)
        require(all(p in SCOPES if name == 'SCOPES' else re.fullmatch(ident, p) is not None for p in parts), 'Invalid AGENTRECALL_' + name)
        cfg[name] = sorted(set(parts))
    require(re.fullmatch(r'[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?', cfg['CLUSTER_ISSUER']) is not None, 'Invalid AGENTRECALL_CLUSTER_ISSUER')
    require(not cfg['CA_BUNDLE'] or Path(cfg['CA_BUNDLE']).is_file(), 'AGENTRECALL_CA_BUNDLE must be an existing trusted CA file')
    cue_token = values.get('AGENTRECALL_CUE_MCP_TOKEN', '')
    cue_owner = values.get('AGENTRECALL_CUE_OWNER_ID', '')
    require(bool(cue_token) == bool(cue_owner), 'Cue token and owner must be configured together')
    cfg['CUE_CREDENTIAL'] = None
    if cue_token:
        require(len(cue_token) >= 32 and all(33 <= ord(c) <= 126 for c in cue_token)
                and not cue_token.lower().startswith(('replace', 'changeme', '<'))
                and cue_token not in (cfg['MCP_TOKEN'], cfg['CURSOR_HMAC_SECRET']), 'Invalid or reused Cue credential')
        cue_client = values.get('AGENTRECALL_CUE_CLIENT_ID', 'cue-cloud')
        cue_namespaces = values.get('AGENTRECALL_CUE_NAMESPACES', 'shared').split(',')
        require(all(re.fullmatch(ident, p) is not None and not p.lower().startswith('replace')
                    for p in [cue_owner, cue_client] + cue_namespaces), 'Invalid Cue identity or namespace')
        require(cue_client != cfg['CLIENT_ID'], 'Cue client must be distinct from the existing client')
        cfg['CUE_CREDENTIAL'] = {'token_hash': hashlib.sha256(cue_token.encode()).hexdigest(),
                                 'owner_id': cue_owner, 'client_id': cue_client, 'scopes': sorted(SCOPES),
                                 'allowed_namespaces': sorted(set(cue_namespaces)), 'active': True}
    semantic_call(replication_module(), 'configure', values, cfg)
    for key, value in values.items():
        if value and (key.startswith('AGENTRECALL_EMBEDDING_')
                      or key in ('AGENTRECALL_CURSOR_HMAC_PREVIOUS_SECRET', 'AGENTRECALL_CURSOR_HMAC_PREVIOUS_UNTIL')):
            raise DeployError('Unsupported cloud-deployment setting ' + key + '; use a separate reviewed configuration')
    return cfg


def secret_manifest(cfg):
    record = {'token_hash': hashlib.sha256(cfg['MCP_TOKEN'].encode()).hexdigest(),
              'owner_id': cfg['OWNER_ID'], 'client_id': cfg['CLIENT_ID'], 'scopes': cfg['SCOPES'],
              'allowed_namespaces': cfg['NAMESPACES'], 'active': True}
    records = [record] + ([cfg['CUE_CREDENTIAL']] if cfg.get('CUE_CREDENTIAL') else [])
    return {'apiVersion': 'v1', 'kind': 'Secret', 'type': 'Opaque',
            'metadata': {'name': 'agentrecall-runtime', 'namespace': NS, 'labels': {'app': NS}},
            'data': {'tokens.json': base64.b64encode(json.dumps(records, separators=(',', ':')).encode()).decode(),
                     'cursor-key': base64.b64encode(cfg['CURSOR_HMAC_SECRET'].encode()).decode()}}


def normalized_credentials(secret):
    try:
        records = json.loads(base64.b64decode(secret['data']['tokens.json'], validate=True))
        require(isinstance(records, list), 'Existing credential data is invalid')
        for record in records:
            record['scopes'] = sorted(set(record.get('scopes', [])))
            record['allowed_namespaces'] = sorted(set(record.get('allowed_namespaces', [])))
            record.setdefault('active', True)
        return json.dumps(records, sort_keys=True, separators=(',', ':')), base64.b64decode(secret['data']['cursor-key'], validate=True)
    except (KeyError, TypeError, ValueError, AttributeError):
        raise DeployError('Existing Kubernetes credential data is invalid') from None


def replication_module():
    root = Path(sys.argv[1]) if __name__ == '__main__' else Path(configuration.__code__.co_filename).resolve().parent
    spec = importlib.util.spec_from_file_location('agentrecall_replication_config', root / 'deploy/k3s/replication.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def semantic_module(root):
    spec = importlib.util.spec_from_file_location('agentrecall_semantic', root / 'deploy/k3s/semantic.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def semantic_call(module, operation, *args):
    try:
        return getattr(module, operation)(*args)
    except ValueError as error:
        raise DeployError(str(error)) from None


def render_manifests(root, cfg):
    workload = (root / 'deploy/k3s/workload.yaml').read_text()
    ingress = (root / 'deploy/k3s/ingress.yaml').read_text()
    workload = workload.replace('__IMAGE__', cfg['IMAGE']).replace('__REPLICA_ID__', cfg['REPLICA_ID']).replace(DEFAULT_DOMAIN, cfg['DOMAIN'])
    ingress = ingress.replace('__HOSTNAME__', cfg['DOMAIN']).replace('cert-manager.io/cluster-issuer: letsencrypt', 'cert-manager.io/cluster-issuer: ' + cfg['CLUSTER_ISSUER'])
    if cfg['REPLICATION_ENABLED']:
        anchor = '            - name: AGENTRECALL_ROLE\n              value: cloud\n'
        require(workload.count(anchor) == 1, 'Unexpected workload environment layout')
        entries = semantic_call(replication_module(), 'receiver_env', cfg)
        receiver = ('            - name: AGENTRECALL_REPLICATION_TOKEN\n'
                    '              valueFrom:\n                secretKeyRef:\n'
                    '                  name: agentrecall-replication\n                  key: replication-token\n'
                    '            - name: AGENTRECALL_TRUSTED_REPLICA_ID\n'
                    '              value: ' + json.dumps(entries[1]['value']) + '\n')
        workload = workload.replace(anchor, anchor + receiver)
        path = '          - path: /mcp\n            pathType: Exact\n            backend:\n              service:\n                name: agentrecall\n                port:\n                  number: 8789\n'
        require(ingress.count(path) == 1, 'Unexpected ingress path layout')
        ingress = ingress.replace(path, path + path.replace('/mcp', '/_internal/replication/exchange'))
    if cfg['PROFILE'] == 'semantic':
        module = semantic_module(root)
        semantic_call(module, 'prepare', root, cfg)
        cfg['MODEL_ROOT'] = str(root)
        workload = semantic_call(module, 'render_workload', workload, cfg)
    require('__' not in workload + ingress, 'Unresolved manifest placeholder')
    require('router.entrypoints: websecure' in ingress and 'router.tls: "true"' in ingress
            and 'path: /mcp\n' in ingress and 'pathType: Exact' in ingress, 'Ingress must be exact-path HTTPS-only')
    return workload, ingress


class Remote:
    def __init__(self, cfg):
        self.argv = ['ssh', '-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
                     '-o', 'UpdateHostKeys=no', '-o', 'ConnectTimeout=12', '-o', 'ServerAliveInterval=10',
                     '-o', 'ServerAliveCountMax=2', cfg['SSH']]

    def command(self, command):
        return self.argv + ['bash', '-c', shlex.quote('set -euo pipefail\n' + command)]

    def run(self, command, payload=None, timeout=60):
        try:
            result = subprocess.run(self.command(command), input=payload, capture_output=True, text=True, timeout=timeout)
        except (OSError, subprocess.TimeoutExpired):
            raise DeployError('SSH operation unavailable or timed out; no secret diagnostics displayed') from None
        require(result.returncode == 0, 'Remote operation failed; inspect the target manually (secret diagnostics suppressed)')
        return result.stdout

    def get(self, kind, name=None, namespaced=True):
        scope = '-n agentrecall ' if namespaced else ''
        command = 'kubectl ' + scope + 'get ' + kind + (' ' + shlex.quote(name) if name else '') + ' --ignore-not-found -o json'
        output = self.run(command)
        try:
            return json.loads(output) if output.strip() else None
        except ValueError:
            raise DeployError('Remote API returned invalid JSON') from None


def collect_state(remote, cfg):
    namespace = remote.get('namespace', NS, False)
    state = {'namespace': namespace, 'nodes': remote.get('nodes', namespaced=False),
             'issuer': remote.get('clusterissuer', cfg['CLUSTER_ISSUER'], False)}
    for kind, name in (('deployment', NS), ('pvc', 'agentrecall-data'), ('secret', 'agentrecall-runtime'),
                       ('certificate', 'agentrecall-tls'), ('service', NS), ('ingress', NS)):
        state[kind] = remote.get(kind, name) if namespace else None
    state['replication_secret'] = remote.get('secret', 'agentrecall-replication') if namespace else None
    return state


def is_ready(resource):
    return resource is not None and any(c.get('type') == 'Ready' and c.get('status') == 'True' for c in resource.get('status', {}).get('conditions', []))


def validate_existing(state, cfg):
    nodes = (state['nodes'] or {}).get('items', [])
    require(len(nodes) == 1 and is_ready(nodes[0]) and nodes[0]['status']['nodeInfo']['architecture'] == 'amd64'
            and nodes[0]['status']['nodeInfo']['operatingSystem'] == 'linux', 'Require one Ready Linux AMD64 k3s node')
    require(is_ready(state['issuer']), 'The configured existing ClusterIssuer must be Ready')
    namespace, deployment, pvc, secret = (state[k] for k in ('namespace', 'deployment', 'pvc', 'secret'))
    for obj in (namespace, deployment, pvc, secret, state.get('service')):
        if obj:
            require(obj.get('metadata', {}).get('labels', {}).get('app') == NS, 'Refusing unrelated or unlabelled existing resource')
    service, ingress = state.get('service'), state.get('ingress')
    if service:
        require(service['spec'].get('type') == 'ClusterIP' and service['spec'].get('selector') == {'app': NS}, 'Unexpected existing Service identity')
    if ingress:
        spec = ingress.get('spec', {})
        paths = [p for r in spec.get('rules', []) for p in r.get('http', {}).get('paths', [])]
        permitted_paths = {'/mcp', '/_internal/replication/exchange'} if cfg['REPLICATION_ENABLED'] else {'/mcp'}
        actual_paths = {p.get('path') for p in paths}
        annotations = ingress.get('metadata', {}).get('annotations', {})
        require(spec.get('ingressClassName') == 'traefik' and len(paths) == len(actual_paths)
                and '/mcp' in actual_paths and actual_paths <= permitted_paths
                and all(p.get('pathType') == 'Exact' and p.get('backend', {}).get('service', {}) == {'name': NS, 'port': {'number': 8789}} for p in paths)
                and annotations.get('traefik.ingress.kubernetes.io/router.entrypoints') == 'websecure'
                and annotations.get('traefik.ingress.kubernetes.io/router.tls') == 'true'
                and len(spec.get('rules', [])) == 1 and spec['rules'][0].get('host') == cfg['DOMAIN']
                and len(spec.get('tls', [])) == 1 and spec['tls'][0].get('secretName') == 'agentrecall-tls', 'Refusing unrelated existing Ingress')
    if pvc:
        spec = pvc.get('spec', {})
        require(spec.get('storageClassName') == 'local-path' and spec.get('accessModes') == ['ReadWriteOnce']
                and spec.get('resources', {}).get('requests', {}).get('storage') == '5Gi'
                and pvc.get('status', {}).get('phase') == 'Bound', 'Existing PVC must be the Bound local-path RWO 5Gi volume')
    if deployment:
        spec = deployment['spec']
        require(spec.get('replicas') == 1 and spec.get('strategy', {}).get('type') == 'Recreate', 'Require one writer and Recreate updates')
        pod = spec['template']['spec']
        containers = pod['containers']
        require(len(containers) == 1 and containers[0]['name'] == NS, 'Unexpected deployment container layout')
        env = {e['name']: e.get('value') for e in containers[0].get('env', [])}
        require(env.get('AGENTRECALL_REPLICA_ID') == cfg['REPLICA_ID'] and env.get('AGENTRECALL_DATABASE_PATH') == '/state/data/cloud.duckdb', 'Refusing database/replica identity change')
        receiver = [e for e in containers[0].get('env', []) if e['name'].startswith(('AGENTRECALL_REPLICATION_', 'AGENTRECALL_PEER_')) or e['name'] == 'AGENTRECALL_TRUSTED_REPLICA_ID']
        expected_receiver = semantic_call(replication_module(), 'receiver_env', cfg)
        require(env.get('AGENTRECALL_ROLE') == 'cloud' and (not receiver or receiver == expected_receiver), 'Existing workload is not a supported cloud configuration')
        require(not receiver or state.get('replication_secret') is not None, 'Existing receiver is missing its pairing Secret; refusing regeneration')
        embedding = {k: v for k, v in env.items() if k.startswith('AGENTRECALL_EMBEDDING_')}
        require(not embedding or embedding == {'AGENTRECALL_EMBEDDING_PROVIDER': 'bge-m3', 'AGENTRECALL_EMBEDDING_CACHE_DIR': '/models'}, 'Existing workload has unsupported embedding settings')
        require(pvc is not None and secret is not None, 'Existing deployment is missing its PVC or runtime Secret; refusing regeneration')
        volumes = {v['name']: v for v in pod.get('volumes', [])}
        require(volumes.get('agentrecall-data', {}).get('persistentVolumeClaim', {}).get('claimName') == 'agentrecall-data'
                and volumes.get('runtime-credentials', {}).get('secret', {}).get('secretName') == 'agentrecall-runtime', 'Unexpected storage/credential volume identity')
        if embedding:
            model_volume = volumes.get('models', {}).get('persistentVolumeClaim', {})
            model_mount = next((m for m in containers[0].get('volumeMounts', []) if m.get('name') == 'models'), {})
            require(model_volume.get('claimName') == 'agentrecall-models' and model_volume.get('readOnly') is True
                    and model_mount.get('mountPath') == '/models' and model_mount.get('readOnly') is True, 'Semantic model cache must be the dedicated read-only PVC')
    if secret:
        actual, actual_key = normalized_credentials(secret)
        expected, expected_key = normalized_credentials(secret_manifest(cfg))
        require(hmac.compare_digest(actual.encode(), expected.encode()) and hmac.compare_digest(actual_key, expected_key),
                'Existing credentials differ from .env; refusing implicit rotation or client removal')
    semantic_call(replication_module(), 'validate_secret', state.get('replication_secret'), cfg)


def validate_dns(cfg):
    def addresses(host):
        return {x[4][0] for x in socket.getaddrinfo(host, None, socket.AF_INET, socket.SOCK_STREAM)}
    require(addresses(cfg['DOMAIN']) == addresses(cfg['SSH'].rsplit('@', 1)[-1]), 'Domain DNS must point exclusively to the configured SSH node')


def image_id(remote, cfg):
    inventory = json.loads(remote.run('k3s crictl images -o json'))
    tags = {cfg['IMAGE'], 'docker.io/library/' + cfg['IMAGE']}
    if not any(tags.intersection(i.get('repoTags') or []) for i in inventory.get('images', [])):
        return None
    info = json.loads(remote.run('k3s crictl inspecti ' + shlex.quote(cfg['IMAGE'])))
    digest = info['status']['id']
    require(re.fullmatch(r'sha256:[a-f0-9]{64}', digest) is not None, 'Invalid remote CRI/config image ID')
    if cfg['IMAGE_CONFIG_DIGEST']:
        require(digest == cfg['IMAGE_CONFIG_DIGEST'], 'Imported image differs from expected CRI/config digest')
    return digest


def local_run(argv, timeout=60):
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        raise DeployError('Local image operation unavailable or timed out') from None
    require(result.returncode == 0, 'Local image operation failed; inspect Docker manually')
    return result.stdout


def build_image(root, cfg):
    require(not local_run(['docker', 'image', 'ls', '--quiet', cfg['IMAGE']]).strip(), 'Build requires a new local image tag too; existing tags are preserved')
    inputs = ('Cargo.toml', 'Cargo.lock', 'src', 'migrations', 'poc/native-container/Dockerfile', 'poc/native-container/Dockerfile.dockerignore')
    for name in inputs:
        source = root / name
        require(source.exists() and not source.is_symlink(), 'Build input missing or symlinked: ' + name)
        require(not source.is_dir() or not any(p.is_symlink() for p in source.rglob('*')), 'Build source contains symlinks: ' + name)
    with tempfile.TemporaryDirectory(prefix='agentrecall-build-') as directory:
        context = Path(directory)
        for name in inputs:
            source = root / name
            destination = context / (source.name if name.startswith('poc/') else name)
            if source.is_dir():
                shutil.copytree(source, destination)
            else:
                shutil.copy2(source, destination)
        print('Explicit AMD64 build from isolated source allowlist; this may be slow on ARM64.')
        local_run(['docker', 'buildx', 'build', '--platform', 'linux/amd64', '--load', '-t', cfg['IMAGE'], '-f', str(context / 'Dockerfile'), str(context)], timeout=7200)


def import_image(remote, cfg):
    info = json.loads(local_run(['docker', 'image', 'inspect', cfg['IMAGE']]))
    require(len(info) == 1 and info[0].get('Architecture') == 'amd64' and info[0].get('Os') == 'linux', 'Import requires a local Linux AMD64 image')
    processes = []
    try:
        save = subprocess.Popen(['docker', 'save', cfg['IMAGE']], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        processes.append(save)
        zipped = subprocess.Popen(['gzip', '-1'], stdin=save.stdout, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        processes.append(zipped)
        save.stdout.close()
        transferred = subprocess.run(remote.command('gzip -dc | k3s ctr images import -'), stdin=zipped.stdout,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=600)
        zipped.stdout.close()
        zip_code, save_code = zipped.wait(timeout=30), save.wait(timeout=30)
        require(transferred.returncode == zip_code == save_code == 0, 'Image transfer/import failed; workload not applied')
    except (OSError, subprocess.TimeoutExpired):
        raise DeployError('Image transfer/import unavailable or timed out') from None
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
            process.wait()
    require(image_id(remote, cfg) is not None, 'Imported image is not visible to k3s CRI')


def private_readiness(remote, state, cfg):
    address = str(ipaddress.IPv4Address(state['service']['spec']['clusterIP']))
    program = "import json,urllib.request\nreq=urllib.request.Request(" + repr('http://' + address + ':8789/health/ready') + ",headers={'Host':'agentrecall'})\nopener=urllib.request.build_opener(urllib.request.ProxyHandler({}))\nwith opener.open(req,timeout=20) as response: print(response.read().decode())"
    ready = json.loads(remote.run("python3 - <<'READINESS'\n" + program + '\nREADINESS\n'))
    expected = 'hybrid' if cfg['PROFILE'] == 'semantic' else 'lexical'
    require(ready.get('status') == 'ready' and ready.get('replica_id') == cfg['REPLICA_ID']
            and ready.get('replication_configured') is cfg['REPLICATION_ENABLED'] and ready.get('search_mode') == expected,
            'Private readiness does not match the selected profile/identity')
    if cfg['PROFILE'] == 'semantic':
        require(ready.get('search_engine') in ('duckdb_vss_hnsw', 'exact_scoped_vector_scan'), 'Semantic vector engine is not ready')
    return ready


def verify_live(remote, cfg):
    state = collect_state(remote, cfg)
    validate_existing(state, cfg)
    deployment = state['deployment']
    require(deployment is not None and deployment.get('status', {}).get('readyReplicas') == 1
            and deployment['status'].get('replicas') == deployment['status'].get('updatedReplicas') == deployment['status'].get('availableReplicas') == 1
            and deployment.get('status', {}).get('observedGeneration', 0) >= deployment['metadata'].get('generation', 1), 'Deployment must have one Ready current-generation replica')
    container = deployment['spec']['template']['spec']['containers'][0]
    require(container['image'] == cfg['IMAGE'] and image_id(remote, cfg) is not None, 'Deployment image does not match .env')
    allowed = next((e.get('value', '') for e in container.get('env', []) if e['name'] == 'AGENTRECALL_ALLOWED_HOSTS'), '')
    require(cfg['DOMAIN'] in allowed.split(','), 'Deployment Host allowlist does not match the public hostname')
    provider = next((e.get('value') for e in container.get('env', []) if e['name'] == 'AGENTRECALL_EMBEDDING_PROVIDER'), None)
    require(provider == ('bge-m3' if cfg['PROFILE'] == 'semantic' else None), 'Live deployment profile does not match selected profile')
    receiver = [e for e in container.get('env', []) if e['name'].startswith(('AGENTRECALL_REPLICATION_', 'AGENTRECALL_PEER_')) or e['name'] == 'AGENTRECALL_TRUSTED_REPLICA_ID']
    require(receiver == semantic_call(replication_module(), 'receiver_env', cfg), 'Live receiver configuration does not match .env')
    paths = {p['path'] for r in state['ingress']['spec']['rules'] for p in r['http']['paths']}
    require(paths == ({'/mcp', '/_internal/replication/exchange'} if cfg['REPLICATION_ENABLED'] else {'/mcp'}), 'Live ingress does not match receiver configuration')
    if cfg['PROFILE'] == 'semantic':
        semantic_call(semantic_module(Path(cfg['MODEL_ROOT'])), 'require_provisioned', remote, cfg)
    private_readiness(remote, state, cfg)
    certificate = state['certificate']
    require(is_ready(certificate) and cfg['DOMAIN'] in certificate.get('spec', {}).get('dnsNames', [])
            and any(c.get('type') == 'Ready' and c.get('status') == 'True' and c.get('observedGeneration', 0) >= certificate['metadata'].get('generation', 1)
                    for c in certificate['status'].get('conditions', [])), 'Expected current-generation public certificate must be Ready')
    context = ssl.create_default_context(cafile=cfg['CA_BUNDLE'] or None)
    require(context.check_hostname and context.verify_mode == ssl.CERT_REQUIRED, 'TLS verification must stay enabled')
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None
    opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=context), NoRedirect())
    def request(url, payload=None, token=None):
        headers = {'Accept': 'application/json, text/event-stream', 'MCP-Protocol-Version': '2025-03-26'}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        if payload is not None:
            headers['Content-Type'] = 'application/json'
        req = urllib.request.Request(url, headers=headers, data=json.dumps(payload).encode() if payload is not None else None)
        try:
            with opener.open(req, timeout=20) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as error:
            try:
                return error.code, error.read()
            finally:
                error.close()
    base = 'https://' + cfg['DOMAIN']
    require(request('http://' + cfg['DOMAIN'] + '/mcp')[0] == 404, 'Plain HTTP MCP must not be routed')
    init = {'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'deploy-k8s-check', 'version': '1'}}}
    require(request(base + '/mcp', init)[0] == 401, 'Missing bearer must be rejected')
    require(request(base + '/mcp', init, 'not-a-real-credential')[0] == 401, 'Wrong bearer must be rejected')
    status, body = request(base + '/mcp', init, cfg['MCP_TOKEN'])
    require(status == 200 and isinstance(json.loads(body).get('result', {}).get('serverInfo', {}).get('name'), str), 'Authenticated MCP initialization failed')
    status, body = request(base + '/mcp', {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list', 'params': {}}, cfg['MCP_TOKEN'])
    require(status == 200 and {t['name'] for t in json.loads(body).get('result', {}).get('tools', [])} == TOOLS, 'Expected five MCP tools were not found')
    search = {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call', 'params': {'name': 'memory_search', 'arguments': {'query': 'deployment readiness probe', 'namespace': cfg['NAMESPACES'][0], 'limit': 1}}}
    status, body = request(base + '/mcp', search, cfg['MCP_TOKEN'])
    result = json.loads(body).get('result', {})
    require(status == 200 and not result.get('isError') and result.get('structuredContent', {}).get('search_mode') == ('hybrid' if cfg['PROFILE'] == 'semantic' else 'lexical'), 'Authenticated search did not match the selected profile')
    for path in ('/health/live', '/health/ready', '/metrics'):
        require(request(base + path)[0] == 404, 'Public diagnostics must not be routed')
    exchange = '/_internal/replication/exchange'
    require(request('http://' + cfg['DOMAIN'] + exchange, {})[0] == 404, 'Plain HTTP replication must not be routed')
    if cfg['REPLICATION_ENABLED']:
        probe = {'source_replica_id': 'deliberately-untrusted-probe', 'remote_cursor': 0, 'events': []}
        for token in (None, 'not-a-real-replication-credential', cfg['MCP_TOKEN']):
            require(request(base + exchange, probe, token)[0] == 401, 'Replication must reject missing/wrong/MCP bearer')
        require(request(base + exchange, probe, cfg['REPLICATION_TOKEN'])[0] == 401, 'Replication must reject a wrong source replica')
        require(request(base + exchange + '/extra', probe)[0] == 404, 'Replication ingress must be exact-path only')
    else:
        require(request(base + exchange)[0] == 404, 'Unconfigured replication must not be routed')
    print('Verified ' + cfg['PROFILE'] + ' profile, Ready replica/PVC, matching credentials/image and authenticated HTTPS-only MCP: ' + base + '/mcp')


def main(root, argv):
    parser = argparse.ArgumentParser(description='Deploy native AgentRecall to existing AMD64 k3s; secrets come only from a private .env.')
    parser.add_argument('--env-file', type=Path, default=root / '.env')
    parser.add_argument('--dry-run', action='store_true', help='Offline validation and redacted plan; no network, build or writes')
    parser.add_argument('--check', action='store_true', help='Read-only live checks; no rollout or memory writes')
    parser.add_argument('--profile', choices=('lexical', 'semantic'), help='Explicit profile; overrides AGENTRECALL_PROFILE (default lexical)')
    parser.add_argument('--provision-model', action='store_true', help='Semantic only: isolated finite model/cache jobs; do not roll the live service')
    images = parser.add_mutually_exclusive_group()
    images.add_argument('--import-image', action='store_true', help='Explicitly import a local prebuilt AMD64 image if absent')
    images.add_argument('--build', action='store_true', help='Explicitly build/import a NEW tag using isolated source inputs')
    args = parser.parse_args(argv)
    require(not (args.check and (args.build or args.import_image or args.dry_run or args.provision_model)), '--check cannot be combined with deployment/image/provisioning flags')
    require(not (args.provision_model and (args.build or args.import_image)), '--provision-model cannot build/import an image')
    cfg = configuration(load_env(args.env_file), args.profile)
    require(not args.provision_model or cfg['PROFILE'] == 'semantic', '--provision-model requires the explicit semantic profile')
    require(not (cfg['PROFILE'] == 'semantic' and (args.build or args.import_image)), 'Semantic profile reuses the tested imported image; build/import in lexical mode before provisioning that artifact')
    workload, ingress = render_manifests(root, cfg)
    if args.dry_run:
        print('Offline plan: isolated namespace agentrecall; one writer; existing PVC/credentials preserved.')
        print('Selected profile: ' + cfg['PROFILE'] + '; application egress remains denied.')
        print('Replication: ' + ('receiver-only HTTPS pairing; dedicated Secret; whole-replica trust' if cfg['REPLICATION_ENABLED'] else 'disabled'))
        print('Image action: ' + ('explicit build + import of a new tag' if args.build else 'explicit import if absent' if args.import_image else 'reuse already imported image'))
        print('Would validate node/DNS/issuer/ownership/identity before any remote changes.')
        if args.provision_model:
            print('Would provision pinned model in dedicated model-only PVC, then test denied-egress inference; no live rollout/credentials/database changes.')
        else:
            print('Would apply reviewed workload and exact-path HTTPS-only ingress, then verify MCP.')
            if cfg['PROFILE'] == 'semantic':
                print('Requires completed provision/offline Jobs and fresh read-only cache preflight; no implicit model download.')
        print('Secrets: loaded privately; values and hashes not displayed. No network/build/writes performed.')
        return
    remote = Remote(cfg)
    validate_dns(cfg)
    state = collect_state(remote, cfg)
    validate_existing(state, cfg)
    if args.check:
        verify_live(remote, cfg)
        return
    existing_image = image_id(remote, cfg)
    if args.provision_model:
        require(state['namespace'] is not None and state['deployment'] is not None, 'Provisioning requires the existing isolated AgentRecall deployment; deploy lexical first')
        require(existing_image is not None, 'Provisioning requires the already imported native image')
        semantic_call(semantic_module(root), 'provision', remote, cfg)
        return
    if cfg['PROFILE'] == 'semantic':
        require(existing_image is not None, 'Semantic rollout uses the tested imported image; provision that artifact first')
        module = semantic_module(root)
        semantic_call(module, 'headroom', remote)
        semantic_call(module, 'verify_cache_before_rollout', remote, cfg)
    if args.build:
        require(existing_image is None, '--build requires a NEW remote image tag; existing tags are never overwritten')
        build_image(root, cfg)
        import_image(remote, cfg)
    elif args.import_image and existing_image is None:
        import_image(remote, cfg)
    else:
        require(existing_image is not None, 'Image not imported; use --import-image or explicit --build with a new tag')
    if not state['namespace']:
        namespace = {'apiVersion': 'v1', 'kind': 'Namespace', 'metadata': {'name': NS, 'labels': {'app': NS}}}
        remote.run('kubectl create -f -', json.dumps(namespace))
    if not state['secret']:
        remote.run('kubectl -n agentrecall create -f -', json.dumps(secret_manifest(cfg)))
    print('Credentials preserved; applying reviewed AgentRecall-only manifests.')
    for manifest in (workload, ingress):
        remote.run('kubectl -n agentrecall apply --dry-run=server -f -', manifest)
    if cfg['REPLICATION_ENABLED'] and not state.get('replication_secret'):
        remote.run('kubectl -n agentrecall create -f -', json.dumps(semantic_call(replication_module(), 'secret_manifest', cfg)))
    for manifest in (workload, ingress):
        remote.run('kubectl -n agentrecall apply -f -', manifest)
    rollout_seconds = 720 if cfg['PROFILE'] == 'semantic' else 180
    remote.run('kubectl -n agentrecall rollout status deployment/agentrecall --timeout=' + str(rollout_seconds) + 's', timeout=rollout_seconds + 30)
    remote.run('kubectl -n agentrecall wait --for=create certificate/agentrecall-tls --timeout=60s', timeout=90)
    remote.run('kubectl -n agentrecall wait --for=condition=Ready certificate/agentrecall-tls --timeout=240s', timeout=270)
    verify_live(remote, cfg)


if __name__ == '__main__':
    try:
        main(Path(sys.argv[1]), sys.argv[2:])
    except DeployError as error:
        print('error: ' + str(error), file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError):
        print('error: deployment/check failed; private diagnostics suppressed. No automatic database rollback.', file=sys.stderr)
        sys.exit(1)
PY
