"""Exercise actual cache-helper shell logic with tiny synthetic pinned assets."""
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

# Portable GNU stat/checksum interfaces for fixture tests on macOS, not deployment.
UTIL = r'''#!__PYTHON__
import hashlib,os,sys
name=os.path.basename(sys.argv[0]); args=sys.argv[1:]
if name=='stat':
    print(os.stat(args[-1]).st_size); sys.exit(0)
algorithm='sha256' if name=='sha256sum' else 'sha1'
if '-c' in args:
    data=sys.stdin.read() if args[-1]=='-' else open(args[-1]).read()
    ok=True
    for line in data.splitlines():
        try:
            expected,path=line.split('  ',1)
            actual=hashlib.new(algorithm,open(path,'rb').read()).hexdigest()
            ok=ok and actual==expected
        except (ValueError,OSError): ok=False
    sys.exit(0 if ok else 1)
paths=[a for a in args if not a.startswith('-')]
if not paths: print(hashlib.new(algorithm,sys.stdin.buffer.read()).hexdigest()+'  -')
for path in paths: print(hashlib.new(algorithm,open(path,'rb').read()).hexdigest()+'  '+path)
'''


class CacheHelperTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()
        self.cache = self.home / 'models'
        self.cache.mkdir()
        self.repo = 'models--gpahal--bge-m3-onnx-int8'
        self.revision = 'a' * 40
        self.contract = 'b' * 64
        self.snapshot = self.cache / self.repo / 'snapshots' / self.revision
        self.snapshot.mkdir(parents=True)
        refs = self.cache / self.repo / 'refs'
        refs.mkdir()
        (refs / 'main').write_text(self.revision)
        self.env = dict(os.environ, MODEL_CONTRACT=self.contract, MODEL_REVISION=self.revision, MODEL_CACHE_REPOSITORY=self.repo)
        self.prefixes = {'model_quantized.onnx': 'ONNX', 'tokenizer.json': 'TOKENIZER', 'config.json': 'CONFIG',
                         'special_tokens_map.json': 'SPECIAL', 'tokenizer_config.json': 'TOKENIZER_CONFIG'}
        for file, prefix in self.prefixes.items():
            data = ('synthetic ' + file).encode()
            (self.snapshot / file).write_bytes(data)
            self.env['MODEL_' + prefix + '_SIZE'] = str(len(data))
            if prefix in ('ONNX', 'TOKENIZER'):
                digest = hashlib.sha256(data).hexdigest()
                self.env['MODEL_' + prefix + '_SHA256'] = digest
            else:
                digest = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
                self.env['MODEL_' + prefix + '_SHA1'] = digest
        self.bin = self.home / 'bin'
        self.bin.mkdir()
        for name in ('stat', 'sha256sum', 'sha1sum'):
            path = self.bin / name
            path.write_text(UTIL.replace('__PYTHON__', sys.executable))
            path.chmod(0o755)
        self.env['PATH'] = str(self.bin) + os.pathsep + self.env['PATH']
        self.helper = self.home / 'helper.sh'
        self.helper.write_text((ROOT / 'deploy/k3s/model-cache.sh').read_text().replace('CACHE=/models', 'CACHE=' + shlex.quote(str(self.cache)), 1))
        self.marker = self.cache / '.agentrecall-model.complete'
        self.manifest = self.cache / '.agentrecall-model.sha256'
        self.finish()

    def finish(self):
        paths = [self.cache / self.repo / 'refs/main'] + [self.snapshot / file for file in self.prefixes]
        self.manifest.write_text(''.join(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + str(path.relative_to(self.cache)) + '\n' for path in paths))
        self.marker.write_text(self.contract)

    def run_helper(self, mode='--verify-only'):
        return subprocess.run(['bash', str(self.helper), mode], env=self.env, capture_output=True, text=True, timeout=15)

    def test_matching_complete_cache_verifies(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_matching_cache_provision_is_noop(self):
        before = {p: p.read_bytes() for p in self.cache.rglob('*') if p.is_file()}
        result = self.run_helper('--provision')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(before, {p: p.read_bytes() for p in self.cache.rglob('*') if p.is_file()})

    def test_partial_cache_refused_without_deletion(self):
        self.marker.unlink()
        result = self.run_helper('--provision')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('partial cache retained', result.stderr)
        self.assertTrue((self.snapshot / 'model_quantized.onnx').exists())

    def test_missing_completion_refused(self):
        self.marker.unlink()
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_wrong_contract_refused(self):
        self.marker.write_text('c' * 64)
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_wrong_revision_refused(self):
        (self.cache / self.repo / 'refs/main').write_text('c' * 40)
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_corrupt_weights_refused_even_if_manifest_recomputed(self):
        file = self.snapshot / 'model_quantized.onnx'
        file.write_bytes(b'x' * len(file.read_bytes()))
        self.finish()
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_corrupt_git_blob_refused(self):
        file = self.snapshot / 'config.json'
        file.write_bytes(b'x' * len(file.read_bytes()))
        self.finish()
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_snapshot_symlink_inside_cache_works(self):
        file = self.snapshot / 'model_quantized.onnx'
        blob = self.cache / 'blob'
        file.rename(blob)
        file.symlink_to(blob)
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_snapshot_symlink_escape_refused(self):
        file = self.snapshot / 'model_quantized.onnx'
        outside = self.home / 'outside'
        file.rename(outside)
        file.symlink_to(outside)
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_missing_asset_refused(self):
        (self.snapshot / 'tokenizer.json').unlink()
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_corrupt_manifest_refused(self):
        self.manifest.write_text('invalid checksum\n')
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_symlink_marker_refused(self):
        outside = self.home / 'marker'
        self.marker.rename(outside)
        self.marker.symlink_to(outside)
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_unknown_mode_refused(self):
        self.assertNotEqual(self.run_helper('--unknown').returncode, 0)


if __name__ == '__main__':
    unittest.main()
