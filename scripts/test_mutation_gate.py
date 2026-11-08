"""Exercise mutation-gate failure handling without compiling the workspace."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


GATE = Path(__file__).with_name('mutation-gate.sh')
FAKE_CARGO = '''#!/usr/bin/env python3
import os
from pathlib import Path
import sys

args = sys.argv[1:]
if '--help' in args:
    raise SystemExit(0)
out = Path(args[args.index('--output') + 1]) / 'mutants.out'
out.mkdir(parents=True)
for name in ['caught', 'missed', 'timeout', 'unviable']:
    (out / (name + '.txt')).write_text('')
scenario = os.environ['GATE_SCENARIO']
phase = out.parent.name
checks_baseline = args[args.index('--baseline') + 1] == 'run'
broken = scenario == 'broken_census' or (scenario == 'broken_confirm' and phase == 'phase2')
if broken and checks_baseline:
    print('ERROR cargo build failed in an unmutated tree')
    raise SystemExit(4)
if scenario == 'broken_confirm':
    verdict = 'missed' if phase == 'phase1' else 'caught'
else:
    verdict = 'unviable'
(out / (verdict + '.txt')).write_text('src/lib.rs:1:1: fixture mutant\\n')
raise SystemExit(2 if verdict == 'missed' else 0)
'''


class MutationGateTests(unittest.TestCase):
    def run_gate(self, scenario):
        with tempfile.TemporaryDirectory(prefix='lain-gate-test-') as directory:
            root = Path(directory)
            (root / 'scripts').mkdir()
            (root / 'src').mkdir()
            (root / 'bin').mkdir()
            shutil.copy2(GATE, root / 'scripts/mutation-gate.sh')
            cargo = root / 'bin/cargo'
            cargo.write_text(FAKE_CARGO)
            cargo.chmod(0o755)
            source = root / 'src/lib.rs'
            source.write_text('pub fn value() -> bool { true }\n')
            for args in [
                ['init', '-q', '-b', 'dev'],
                ['config', 'user.name', 'Mutation gate test'],
                ['config', 'user.email', 'mutation-gate@example.invalid'],
                ['config', 'commit.gpgsign', 'false'],
                ['add', '.'], ['commit', '-qm', 'fixture baseline'],
            ]:
                subprocess.run(['git', *args], cwd=root, check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            base = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
            source.write_text('pub fn value() -> bool { false }\n')
            env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                       GATE_SCENARIO=scenario)
            return subprocess.run(['bash', 'scripts/mutation-gate.sh', base], cwd=root,
                                  env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                  text=True, timeout=20)

    def test_broken_census_baseline_cannot_pass_as_all_unviable(self):
        result = self.run_gate('broken_census')
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn('unmutated tree', result.stdout)

    def test_broken_confirm_baseline_cannot_pass_as_caught(self):
        result = self.run_gate('broken_confirm')
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn('unmutated tree', result.stdout)

    def test_valid_baseline_allows_genuinely_unviable_mutants(self):
        result = self.run_gate('valid_unviable')
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn('1 unviable', result.stdout)


if __name__ == '__main__':
    unittest.main()
