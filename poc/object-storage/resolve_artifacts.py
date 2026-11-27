#!/usr/bin/env python3
"""Resolve only the approved exact CLI and community artifacts; no substitutes."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import tarfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / 'target/cloud-poc/object-storage-artifact-manifest.json'
STAGE = ROOT / 'target/cloud-poc/object-storage-artifact-stage'
CLI_URL = 'https://github.com/duckdb/duckdb/releases/download/v2.0.1/duckdb-cli-linux-amd64.tar.gz'
CLI_SHA = 'b676268be4d39e4e41a904442c9796c9914e645cc085c7505f048f2c992b5eca'
BASE = 'debian:bookworm-slim@sha256:311f7a192fd9a92b165a86b32fa3aae9005b11849bfb1db2d4fd784559ee464f'


def download(url, maximum=80000000):
    start = time.monotonic()
    with urllib.request.urlopen(urllib.request.Request(url, headers={'User-Agent': 'agentrecall-poc-object-storage'}), timeout=30) as response:
        chunks, size = [], 0
        while True:
            if time.monotonic() - start > 90:
                raise TimeoutError('artifact download exceeded 90 seconds')
            chunk = response.read(65536)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum:
                raise ValueError('artifact exceeds approved bounded download')
            chunks.append(chunk)
        data = b''.join(chunks)
        return data, {'url': url, 'http_status': response.status, 'size_bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


def main():
    STAGE.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(STAGE, 0o700)
    manifest = {'module_id': 'poc-object-storage', 'status': 'blocked', 'expected_version': 'v2.0.1', 'platform': 'linux/amd64', 'base_image': BASE, 'artifacts': {}, 'runtime_load': 'not_run', 'emulator': {'status': 'blocked', 'release': 'RELEASE.2025-09-07T16-13-09Z', 'source': 'https://github.com/minio/minio/releases/tag/RELEASE.2025-09-07T16-13-09Z', 'image': 'quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z', 'digest': None, 'error': 'Authoritative registry HEAD returned 401 Unauthorized; no replacement image used.'}}
    try:
        data, identity = download(CLI_URL)
        identity['expected_sha256'] = CLI_SHA
        manifest['artifacts']['cli'] = identity
        if identity['sha256'] != CLI_SHA:
            raise ValueError('exact CLI archive SHA256 does not match official release API')
        archive = STAGE / 'cli.tar.gz'
        archive.write_bytes(data)
        with tarfile.open(str(archive), 'r:gz') as tar:
            members = [m for m in tar.getmembers() if m.isfile() and Path(m.name).name == 'duckdb']
            if len(members) != 1:
                raise ValueError('archive must contain one regular duckdb executable')
            with tar.extractfile(members[0]) as source, (STAGE / 'duckdb').open('wb') as dest:
                shutil.copyfileobj(source, dest)
        os.chmod(STAGE / 'duckdb', 0o755)
        identity['binary_sha256'] = hashlib.sha256((STAGE / 'duckdb').read_bytes()).hexdigest()
        archive.unlink()
        for name in ('duckdb_object_storage', 'cache_httpfs'):
            url = 'https://community-extensions.duckdb.org/v2.0.1/linux_amd64/' + name + '.duckdb_extension.gz'
            data, identity = download(url)
            unpacked = gzip.decompress(data)
            if len(unpacked) > 200000000:
                raise ValueError('uncompressed extension exceeds bound')
            identity['extension_sha256'] = hashlib.sha256(unpacked).hexdigest()
            identity['uncompressed_size_bytes'] = len(unpacked)
            (STAGE / (name + '.duckdb_extension')).write_bytes(unpacked)
            manifest['artifacts'][name] = identity
        manifest['status'] = 'not_run'
        manifest['limitation'] = 'Artifact bytes fetched; signature validation and exact AMD64 runtime execution remain required.'
    except Exception as error:
        manifest['error_type'] = type(error).__name__
        manifest['error'] = str(error)
    finally:
        OUTPUT.write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps(manifest, indent=2))
    return 0 if manifest['status'] == 'not_run' else 2


if __name__ == '__main__':
    raise SystemExit(main())
