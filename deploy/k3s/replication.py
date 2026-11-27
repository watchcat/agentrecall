"""Receiver-only cloud pairing. Never rotates credentials or opens databases."""
import base64
import hashlib
import hmac
import re

EXCHANGE_PATH = '/_internal/replication/exchange'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def configure(values, cfg):
    for key, value in values.items():
        require(not value or not key.startswith('AGENTRECALL_PEER_'), 'Cloud outbound peer settings are unsupported')
        require(not value or not key.startswith('AGENTRECALL_REPLICATION_') or key == 'AGENTRECALL_REPLICATION_TOKEN',
                'Unknown cloud replication setting')
    token = values.get('AGENTRECALL_REPLICATION_TOKEN', '')
    trusted = values.get('AGENTRECALL_TRUSTED_REPLICA_ID', '')
    require(isinstance(token, str) and isinstance(trusted, str), 'Invalid pairing settings')
    require(bool(token) == bool(trusted), 'Replication token and trusted replica must be configured together')
    if token:
        require(len(token) >= 32 and all(33 <= ord(c) <= 126 for c in token)
                and not token.lower().startswith(('replace', 'changeme', '<'))
                and token not in (cfg['MCP_TOKEN'], cfg['CURSOR_HMAC_SECRET']), 'Invalid or reused replication credential')
        cue = cfg.get('CUE_CREDENTIAL')
        require(not cue or hashlib.sha256(token.encode()).hexdigest() != cue['token_hash'], 'Replication and Cue credentials must differ')
        require(re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.@-]{0,127}', trusted) is not None
                and not trusted.lower().startswith('replace') and trusted != cfg['REPLICA_ID'], 'Invalid trusted replica identity')
    cfg.update(REPLICATION_TOKEN=token, TRUSTED_REPLICA_ID=trusted, REPLICATION_ENABLED=bool(token))


def secret_manifest(cfg):
    if not cfg['REPLICATION_ENABLED']:
        return None
    return {'apiVersion': 'v1', 'kind': 'Secret', 'type': 'Opaque',
            'metadata': {'name': 'agentrecall-replication', 'namespace': 'agentrecall',
                         'labels': {'app': 'agentrecall'}},
            'data': {'replication-token': base64.b64encode(cfg['REPLICATION_TOKEN'].encode()).decode()}}


def receiver_env(cfg):
    if not cfg['REPLICATION_ENABLED']:
        return []
    return [{'name': 'AGENTRECALL_REPLICATION_TOKEN', 'valueFrom': {'secretKeyRef': {
        'name': 'agentrecall-replication', 'key': 'replication-token'}}},
        {'name': 'AGENTRECALL_TRUSTED_REPLICA_ID', 'value': cfg['TRUSTED_REPLICA_ID']}]


def validate_secret(secret, cfg):
    if secret is None:
        return
    require(cfg['REPLICATION_ENABLED'], 'Refusing implicit removal of existing pairing')
    metadata = secret.get('metadata', {})
    require(metadata.get('name') == 'agentrecall-replication'
            and metadata.get('namespace') == 'agentrecall'
            and metadata.get('labels', {}).get('app') == 'agentrecall'
            and secret.get('type') == 'Opaque', 'Refusing unrelated pairing Secret')
    data = secret.get('data', {})
    require(isinstance(data, dict) and set(data) == {'replication-token'}, 'Unexpected pairing Secret layout')
    try:
        token = base64.b64decode(data['replication-token'], validate=True)
    except (TypeError, ValueError):
        raise ValueError('Invalid pairing Secret encoding') from None
    require(hmac.compare_digest(token, cfg['REPLICATION_TOKEN'].encode()),
            'Existing pairing secret differs; refusing implicit rotation')
