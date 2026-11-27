"""Explicit semantic deployment support; standard library only, no live secrets."""
import copy
import hashlib
import json
import secrets

NS = 'agentrecall'
APP = 'agentrecall-model-provision'
OFFLINE = 'agentrecall-model-offline'
PVC = 'agentrecall-models'


def require(condition, message):
    if not condition:
        raise ValueError(message)


# MiniMax-M3/thinking draft, reviewed by Manus. Never selects the application.
def model_network_policy():
    return {
        'apiVersion': 'networking.k8s.io/v1', 'kind': 'NetworkPolicy',
        'metadata': {'name': APP, 'namespace': NS, 'labels': {'app': APP}},
        'spec': {'podSelector': {'matchLabels': {'app': APP}},
                 'policyTypes': ['Ingress', 'Egress'], 'ingress': [],
                 'egress': [
                     {'to': [{'namespaceSelector': {'matchLabels': {'kubernetes.io/metadata.name': 'kube-system'}},
                              'podSelector': {'matchLabels': {'k8s-app': 'kube-dns'}}}],
                      'ports': [{'protocol': 'UDP', 'port': 53}, {'protocol': 'TCP', 'port': 53}]},
                     {'to': [{'ipBlock': {'cidr': '0.0.0.0/0', 'except': [
                         '0.0.0.0/8', '10.0.0.0/8', '100.64.0.0/10', '127.0.0.0/8',
                         '169.254.0.0/16', '172.16.0.0/12', '192.168.0.0/16', '224.0.0.0/4', '240.0.0.0/4']}}],
                      'ports': [{'protocol': 'TCP', 'port': 443}]}]}}


def prepare(root, cfg):
    lock = json.loads((root / 'deploy/k3s/model-lock.json').read_text())
    helper = (root / 'deploy/k3s/model-cache.sh').read_text()
    require(lock['schema'] == 1 and lock['provider'] == 'bge-m3' and lock['dimension'] == 1024
            and lock['repository'] == 'gpahal/bge-m3-onnx-int8'
            and lock['cache_repository'] == 'models--gpahal--bge-m3-onnx-int8', 'Unsupported model lock contract')
    require(len(lock['revision']) == 40 and all(c in '0123456789abcdef' for c in lock['revision']), 'Invalid pinned revision')
    files = lock['files']
    require(set(files) == {'model_quantized.onnx', 'tokenizer.json', 'config.json', 'special_tokens_map.json', 'tokenizer_config.json'}, 'Unexpected model asset list')
    for name, data in files.items():
        digest = data.get('sha256', data.get('git_blob_sha1', ''))
        require(len(digest) == (64 if 'sha256' in data else 40) and all(c in '0123456789abcdef' for c in digest)
                and isinstance(data['size'], int) and 0 < data['size'] <= 600000000, 'Invalid model asset pin')
    material = json.dumps(lock, sort_keys=True).encode() + helper.encode() + cfg['IMAGE'].encode() + cfg['IMAGE_CONFIG_DIGEST'].encode()
    contract = hashlib.sha256(material).hexdigest()
    env = {'MODEL_CONTRACT': contract, 'MODEL_REVISION': lock['revision'], 'MODEL_CACHE_REPOSITORY': lock['cache_repository']}
    for file, prefix in [('model_quantized.onnx', 'ONNX'), ('tokenizer.json', 'TOKENIZER'), ('config.json', 'CONFIG'),
                         ('special_tokens_map.json', 'SPECIAL'), ('tokenizer_config.json', 'TOKENIZER_CONFIG')]:
        pin = files[file]
        env['MODEL_' + prefix + '_SIZE'] = str(pin['size'])
        env['MODEL_' + prefix + ('_SHA256' if 'sha256' in pin else '_SHA1')] = pin.get('sha256', pin.get('git_blob_sha1'))
    cfg.update({'MODEL_CONTRACT': contract, 'MODEL_ENV': env, 'MODEL_HELPER': helper,
                'MODEL_JOB': 'agentrecall-model-' + contract[:16],
                'OFFLINE_JOB': 'agentrecall-offline-' + contract[:16],
                'MODEL_SCRIPT': 'agentrecall-model-script-' + contract[:16]})


def security():
    return {'allowPrivilegeEscalation': False, 'readOnlyRootFilesystem': True,
            'runAsNonRoot': True, 'capabilities': {'drop': ['ALL']}}


def mounts(readonly):
    return [{'name': 'models', 'mountPath': '/models', **({'readOnly': True} if readonly else {})},
            {'name': 'model-script', 'mountPath': '/scripts', 'readOnly': True},
            {'name': 'tmp', 'mountPath': '/tmp'}]


def volumes(cfg, readonly):
    return [{'name': 'models', 'persistentVolumeClaim': {'claimName': PVC, **({'readOnly': True} if readonly else {})}},
            {'name': 'model-script', 'configMap': {'name': cfg['MODEL_SCRIPT'], 'defaultMode': 365}},
            {'name': 'tmp', 'emptyDir': {'sizeLimit': '64Mi'}}]


def container(cfg, mode, readonly=False):
    return {'name': 'model', 'image': cfg['IMAGE'], 'imagePullPolicy': 'Never',
            'command': ['/bin/bash', '/scripts/model-cache.sh', mode],
            'env': [{'name': k, 'value': v} for k, v in cfg['MODEL_ENV'].items()] + [{'name': 'HOME', 'value': '/home/agentrecall'}],
            'securityContext': security(), 'volumeMounts': mounts(readonly),
            'resources': {'requests': {'cpu': '100m', 'memory': '128Mi'},
                          'limits': {'cpu': '1', 'memory': '256Mi' if mode == '--verify-only' else '2Gi'}}}


def job(cfg, name, mode):
    readonly = mode != '--provision'
    app = OFFLINE if readonly else APP
    return {'apiVersion': 'batch/v1', 'kind': 'Job',
            'metadata': {'name': name, 'namespace': NS, 'labels': {'app': app},
                         'annotations': {'agentrecall/model-contract': cfg['MODEL_CONTRACT']}},
            'spec': {'backoffLimit': 0, 'activeDeadlineSeconds': 90 if mode == '--verify-only' else 1200,
                     'template': {'metadata': {'labels': {'app': app}}, 'spec': {
                         'restartPolicy': 'Never', 'automountServiceAccountToken': False, 'enableServiceLinks': False,
                         'terminationGracePeriodSeconds': 45, 'nodeSelector': {'kubernetes.io/arch': 'amd64'},
                         'securityContext': {'runAsNonRoot': True, 'runAsUser': 10001, 'runAsGroup': 10001,
                                             'fsGroup': 10001, 'fsGroupChangePolicy': 'OnRootMismatch',
                                             'seccompProfile': {'type': 'RuntimeDefault'}},
                         'containers': [container(cfg, mode, readonly)], 'volumes': volumes(cfg, readonly)}}}}


def resources(cfg):
    policy = model_network_policy()
    offline_policy = copy.deepcopy(policy)
    offline_policy['metadata']['name'] = OFFLINE
    offline_policy['metadata']['labels']['app'] = OFFLINE
    offline_policy['spec']['podSelector']['matchLabels']['app'] = OFFLINE
    offline_policy['spec']['egress'] = []
    return [
        {'apiVersion': 'v1', 'kind': 'PersistentVolumeClaim',
         'metadata': {'name': PVC, 'namespace': NS, 'labels': {'app': APP}},
         'spec': {'accessModes': ['ReadWriteOnce'], 'storageClassName': 'local-path',
                  'resources': {'requests': {'storage': '5Gi'}}}},
        policy, offline_policy,
        {'apiVersion': 'v1', 'kind': 'ConfigMap', 'immutable': True,
         'metadata': {'name': cfg['MODEL_SCRIPT'], 'namespace': NS, 'labels': {'app': APP}},
         'data': {'model-cache.sh': cfg['MODEL_HELPER']}}]


def render_workload(workload, cfg):
    init = container(cfg, '--verify-only', True)
    init['name'] = 'verify-model-cache'
    init['volumeMounts'] = [m for m in init['volumeMounts'] if m['name'] != 'tmp']
    workload = workload.replace('      containers:\n', '      initContainers: ' + json.dumps([init]) + '\n      containers:\n', 1)
    workload = workload.replace('            - name: HOME\n', '            - name: AGENTRECALL_EMBEDDING_PROVIDER\n              value: bge-m3\n            - name: AGENTRECALL_EMBEDDING_CACHE_DIR\n              value: /models\n            - name: HOME\n', 1)
    workload = workload.replace('memory: 256Mi', 'memory: 1Gi').replace('memory: 1Gi\n          startupProbe:', 'memory: 2Gi\n          startupProbe:')
    workload = workload.replace('failureThreshold: 60\n', 'failureThreshold: 300\n', 1)
    workload = workload.replace('          volumeMounts:\n', '          volumeMounts:\n            - name: models\n              mountPath: /models\n              readOnly: true\n', 1)
    extra = volumes(cfg, True)[:2]
    workload = workload.replace('      volumes:\n', '      volumes:\n' + ''.join('        - ' + json.dumps(v) + '\n' for v in extra), 1)
    return workload


def complete(obj):
    return bool(obj and any(c.get('type') == 'Complete' and c.get('status') == 'True' for c in obj.get('status', {}).get('conditions', [])))


def subset(expected, actual):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(subset(v, actual.get(k, [] if v == [] else None)) for k, v in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(expected) == len(actual) and all(subset(e, a) for e, a in zip(expected, actual))
    return expected == actual


def validate_resource(expected, actual):
    if actual is None:
        return
    require(actual.get('metadata', {}).get('labels', {}).get('app') == expected['metadata']['labels']['app'], 'Refusing foreign model resource')
    if expected['kind'] == 'ConfigMap':
        require(actual.get('immutable') is True and actual.get('data') == expected['data'], 'Model helper ConfigMap differs')
    elif expected['kind'] == 'Job':
        require(actual.get('metadata', {}).get('annotations', {}).get('agentrecall/model-contract') == expected['metadata']['annotations']['agentrecall/model-contract']
                and subset(expected['spec'], actual.get('spec', {})), 'Existing model Job does not match the pinned isolated contract')
    else:
        require(subset(expected['spec'], actual.get('spec', {})), 'Existing model storage/network configuration differs')


def state(remote, cfg):
    result = {}
    for expected in resources(cfg) + [job(cfg, cfg['MODEL_JOB'], '--provision'), job(cfg, cfg['OFFLINE_JOB'], '--probe-offline')]:
        kind, name = expected['kind'].lower(), expected['metadata']['name']
        actual = remote.get(kind, name)
        validate_resource(expected, actual)
        result[name] = actual
    return result


def require_provisioned(remote, cfg):
    current = state(remote, cfg)
    require(complete(current[cfg['MODEL_JOB']]) and complete(current[cfg['OFFLINE_JOB']]),
            'Semantic profile requires completed provisioning AND denied-egress offline inference; run --profile semantic --provision-model first')
    require(current[PVC] and current[PVC].get('status', {}).get('phase') == 'Bound', 'Model PVC must be Bound')
    return current


def headroom(remote):
    program = "import json,os\nm=dict(line.split(':',1) for line in open('/proc/meminfo'))\nv=os.statvfs('/var/lib/rancher/k3s/storage')\nprint(json.dumps({'available_memory':int(m['MemAvailable'].split()[0])*1024,'available_disk':v.f_bavail*v.f_frsize}))"
    values = json.loads(remote.run("python3 - <<'HEADROOM'\n" + program + '\nHEADROOM\n'))
    require(values['available_memory'] >= 3 * 1024**3 and values['available_disk'] >= 10 * 1024**3,
            'Insufficient measured headroom: require 3Gi available RAM and 10Gi free storage for the bounded 2Gi experiment')
    return values


def wait_job(remote, name, seconds):
    # A finite Job retains failed output/PVC. No retries or deletion on failure.
    program = "import json,subprocess,sys,time\nname=" + repr(name) + "\ndeadline=time.monotonic()+" + str(seconds) + "\nwhile time.monotonic()<deadline:\n obj=json.loads(subprocess.check_output(['kubectl','-n','agentrecall','get','job',name,'-o','json','--request-timeout=15s']))\n conditions=obj.get('status',{}).get('conditions',[])\n if any(c.get('type')=='Failed' and c.get('status')=='True' for c in conditions): sys.exit('Model Job failed; retained for inspection, no automatic retry')\n if any(c.get('type')=='Complete' and c.get('status')=='True' for c in conditions): break\n time.sleep(2)\nelse: sys.exit('Model Job deadline exceeded')\n"
    remote.run("python3 - <<'WAITMODEL'\n" + program + '\nWAITMODEL\n', timeout=seconds + 30)
    require(complete(remote.get('job', name)), 'Model Job did not complete')


def create_job(remote, cfg, name, mode):
    manifest = json.dumps(job(cfg, name, mode))
    remote.run('kubectl -n agentrecall create --dry-run=server -f -', manifest)
    remote.run('kubectl -n agentrecall create -f -', manifest)
    wait_job(remote, name, 110 if mode == '--verify-only' else 1230)


def provision(remote, cfg):
    headroom(remote)
    current = state(remote, cfg)
    all_jobs = remote.get('jobs') or {'items': []}
    for existing in all_jobs.get('items', []):
        if existing.get('metadata', {}).get('labels', {}).get('app') in (APP, OFFLINE):
            require(not existing.get('status', {}).get('active'), 'Another model Job is active; no concurrent cache writers/probes')
    for name in (cfg['MODEL_JOB'], cfg['OFFLINE_JOB']):
        require(current[name] is None or complete(current[name]), 'Existing model Job is incomplete/failed; inspect retained cache/Job before an explicit recovery')
    for resource in resources(cfg):
        if current[resource['metadata']['name']] is None:
            payload = json.dumps(resource)
            remote.run('kubectl -n agentrecall create --dry-run=server -f -', payload)
            remote.run('kubectl -n agentrecall create -f -', payload)
    if current[cfg['MODEL_JOB']] is None:
        create_job(remote, cfg, cfg['MODEL_JOB'], '--provision')
    if current[cfg['OFFLINE_JOB']] is None:
        create_job(remote, cfg, cfg['OFFLINE_JOB'], '--probe-offline')
    verify_cache_before_rollout(remote, cfg)
    require_provisioned(remote, cfg)
    print('Pinned model provisioned; denied-egress AMD64 inference/backfill/CRUD/restart passed. Live deployment/credentials/database were not changed.')
    print('Evidence Jobs: ' + cfg['MODEL_JOB'] + ', ' + cfg['OFFLINE_JOB'])


def verify_cache_before_rollout(remote, cfg):
    require_provisioned(remote, cfg)
    # A new read-only checksum Job checks content before touching the live writer.
    name = 'agentrecall-cache-check-' + cfg['MODEL_CONTRACT'][:12] + '-' + secrets.token_hex(4)
    create_job(remote, cfg, name, '--verify-only')
    print('Read-only model cache checksum preflight passed.')
