"""Check that release provenance exports preserve signatures and reject mismatches."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('export_provenance', Path(__file__).with_name('export-release-provenance.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.artifact = self.root / 'lain-test.tar.gz'
        self.artifact.write_bytes(b'release fixture')
        self.statement = {'predicateType': 'https://slsa.dev/provenance/v1',
                          'subject': [{'name': self.artifact.name,
                                       'digest': {'sha256': hashlib.sha256(self.artifact.read_bytes()).hexdigest()}}]}
        self.bundle_path = self.root / 'bundle.json'

    def write_bundle(self, signatures=None):
        self.envelope = {'payloadType': 'application/vnd.in-toto+json',
                         'payload': base64.b64encode(json.dumps(self.statement).encode()).decode(),
                         'signatures': [{'sig': 'fixture-signature'}] if signatures is None else signatures}
        self.bundle_path.write_text(json.dumps({'dsseEnvelope': self.envelope, 'verificationMaterial': {'fixture': True}}))

    def test_preserves_bundle_and_signed_envelope(self):
        self.write_bundle()
        module.export(self.bundle_path, self.artifact)
        self.assertEqual(Path(str(self.artifact) + '.sigstore.json').read_bytes(), self.bundle_path.read_bytes())
        lines = Path(str(self.artifact) + '.intoto.jsonl').read_text().splitlines()
        self.assertEqual(len(lines), 1)
        self.assertEqual(json.loads(lines[0]), self.envelope)

    def test_rejects_tampered_artifact(self):
        self.write_bundle()
        self.artifact.write_bytes(b'tampered')
        with self.assertRaisesRegex(ValueError, 'does not match'):
            module.export(self.bundle_path, self.artifact)
        self.assertFalse(Path(str(self.artifact) + '.sigstore.json').exists())

    def test_rejects_wrong_subject_name(self):
        self.statement['subject'][0]['name'] = 'other.tar.gz'
        self.write_bundle()
        with self.assertRaisesRegex(ValueError, 'does not match'):
            module.export(self.bundle_path, self.artifact)

    def test_rejects_additional_subjects(self):
        self.statement['subject'].append({'name': 'other.tar.gz', 'digest': {'sha256': '00'}})
        self.write_bundle()
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            module.export(self.bundle_path, self.artifact)

    def test_rejects_missing_subject(self):
        self.statement['subject'] = []
        self.write_bundle()
        with self.assertRaisesRegex(ValueError, 'exactly one'):
            module.export(self.bundle_path, self.artifact)

    def test_rejects_unsigned_envelope(self):
        self.write_bundle(signatures=[])
        with self.assertRaisesRegex(ValueError, 'Missing DSSE signature'):
            module.export(self.bundle_path, self.artifact)

    def test_rejects_other_predicate(self):
        self.statement['predicateType'] = 'https://example.org/other'
        self.write_bundle()
        with self.assertRaisesRegex(ValueError, 'Expected SLSA'):
            module.export(self.bundle_path, self.artifact)


if __name__ == '__main__':
    unittest.main()
