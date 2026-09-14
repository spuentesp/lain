#!/usr/bin/env python3
"""Export a Sigstore bundle and its signed in-toto envelope beside an artifact.

Checks subject identity before publishing; signature verification remains the
responsibility of `gh attestation verify`, using the complete Sigstore bundle.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil


def export(bundle_path: Path, artifact: Path) -> None:
    bundle = json.loads(bundle_path.read_text())
    envelope = bundle['dsseEnvelope']
    if envelope['payloadType'] != 'application/vnd.in-toto+json':
        raise ValueError('Expected an in-toto DSSE payload')
    if not envelope.get('signatures') or not all(s.get('sig') for s in envelope['signatures']):
        raise ValueError('Missing DSSE signature')
    statement = json.loads(base64.b64decode(envelope['payload'], validate=True))
    if statement.get('predicateType') != 'https://slsa.dev/provenance/v1':
        raise ValueError('Expected SLSA v1 build provenance')
    with artifact.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    if not any(subject.get('name') == artifact.name and
               subject.get('digest', {}).get('sha256') == digest
               for subject in statement.get('subject', [])):
        raise ValueError('Provenance does not match artifact name and SHA256')
    # Preserve the signed payload and signatures exactly; don't re-sign or
    # invent provenance for releases that lack build-time evidence.
    shutil.copyfile(bundle_path, str(artifact) + '.sigstore.json')
    Path(str(artifact) + '.intoto.jsonl').write_text(json.dumps(envelope) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=Path)
    parser.add_argument('artifact', type=Path)
    args = parser.parse_args()
    export(args.bundle, args.artifact)
