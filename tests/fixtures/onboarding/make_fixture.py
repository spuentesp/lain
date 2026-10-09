#!/usr/bin/env python3
"""Build the onboarding fixture: a small controlled repo whose dependency
truth is declared in ground_truth.json BEFORE any LAIN query.

Layout (one target used from several files, one indirect consumer, one
similarly-named decoy with no relation):

    core.py       normalize_token  (EDIT TARGET; the edit adds collapse_ws)
    api.py        create_user      -> calls normalize_token   (direct)
    session.py    load_user        -> calls normalize_token   (direct)
    pipeline.py   open_session     -> calls load_user         (indirect, depth 2)
    util.py       normalize_token_len                        (decoy: similar
                   name, NO calls edge to normalize_token)
    tests/        cover the direct AND the indirect consumer against the
                  NEW behavior — red before the edit, green after. The
                  tests are the oracle; nobody may edit them.

Usage:
    make_fixture.py build <dir>        # create the repo + git init/commit
    make_fixture.py apply-edit <dir>   # scripted edit (graph mode only)
"""
import os
import subprocess
import sys

CORE_PRE = '''\
"""Token normalization shared by the user-facing modules."""


def normalize_token(raw):
    return raw.strip().lower()
'''

CORE_POST = '''\
"""Token normalization shared by the user-facing modules."""


def collapse_ws(s):
    return " ".join(s.split())


def normalize_token(raw):
    return collapse_ws(raw).lower()
'''

API = '''\
from core import normalize_token


def create_user(name, token):
    """Create a user; the token is normalized on the way in."""
    return {"name": name, "token": normalize_token(token)}
'''

SESSION = '''\
from core import normalize_token


def load_user(token):
    """Load the user a token belongs to, after normalizing it."""
    return {"token": normalize_token(token), "authenticated": True}
'''

PIPELINE = '''\
from session import load_user


def open_session(raw):
    """Open a session from a raw token.

    Indirect consumer of normalize_token: this file never names it,
    the relationship runs through session.load_user.
    """
    return load_user(raw)
'''

UTIL = '''\
def normalize_token_len(raw):
    """Length of a raw token.

    Decoy on purpose: similar name to core.normalize_token, but nothing
    here calls it and nothing in core calls this. A blast radius of
    normalize_token that lists this symbol is a false positive.
    """
    return len(raw.strip())
'''

TESTS = '''\
"""Consumer contracts for the change: internal whitespace runs collapse
to a single space. Covers the two DIRECT consumers (api.create_user,
session.load_user) and the INDIRECT one (pipeline.open_session, which
reaches normalize_token only through session.load_user).

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from api import create_user
from pipeline import open_session
from session import load_user


class DirectConsumers(unittest.TestCase):
    def test_api_create_user_collapses_whitespace(self):
        user = create_user("ada", "  A   B  ")
        self.assertEqual(user["token"], "a b")

    def test_api_create_user_plain_token(self):
        user = create_user("ada", "  Bob  ")
        self.assertEqual(user["token"], "bob")

    def test_session_load_user_collapses_whitespace(self):
        user = load_user("  A   B  ")
        self.assertEqual(user["token"], "a b")


class IndirectConsumer(unittest.TestCase):
    def test_pipeline_open_session_collapses_whitespace(self):
        session = open_session("  A   B  ")
        self.assertEqual(session["token"], "a b")


if __name__ == "__main__":
    unittest.main()
'''

FILES_PRE = {
    "core.py": CORE_PRE,
    "api.py": API,
    "session.py": SESSION,
    "pipeline.py": PIPELINE,
    "util.py": UTIL,
    "tests/test_consumers.py": TESTS,
    "README.md": (
        "# token-service\n\nSmall service. `core.normalize_token` is the "
        "shared normalization used by `api` and `session`.\n\n"
        "Tests: `python3 -m unittest discover -s tests -v`\n"
    ),
}


def _git(dir_, *args, env=None):
    merged = dict(os.environ)
    merged.setdefault("GIT_AUTHOR_NAME", "onboarding-fixture")
    merged.setdefault("GIT_AUTHOR_EMAIL", "fixture@lain.local")
    merged.setdefault("GIT_COMMITTER_NAME", "onboarding-fixture")
    merged.setdefault("GIT_COMMITTER_EMAIL", "fixture@lain.local")
    if env:
        merged.update(env)
    subprocess.run(["git", "-C", dir_, *args], check=True, env=merged,
                   capture_output=True)


def build(dest):
    os.makedirs(dest, exist_ok=True)
    for rel, body in FILES_PRE.items():
        path = os.path.join(dest, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(body)
    subprocess.run(["git", "init", "-q", "-b", "main"], check=True, cwd=dest)
    _git(dest, "config", "user.email", "fixture@lain.local")
    _git(dest, "config", "user.name", "onboarding-fixture")
    _git(dest, "add", "-A")
    # Fixed dates keep the commit reproducible across runs.
    fixed = {"GIT_AUTHOR_DATE": "2026-01-01T00:00:00 +0000",
             "GIT_COMMITTER_DATE": "2026-01-01T00:00:00 +0000"}
    _git(dest, "commit", "-q", "-m", "fixture: token service before the change",
         env=fixed)
    return dest


def apply_edit(dest):
    """The scripted change (graph mode only): normalize_token collapses
    internal whitespace runs via a new collapse_ws helper."""
    with open(os.path.join(dest, "core.py"), "w") as f:
        f.write(CORE_POST)
    return dest


def main(argv):
    if len(argv) != 3 or argv[1] not in ("build", "apply-edit"):
        print(__doc__, file=sys.stderr)
        return 2
    cmd, dest = argv[1], argv[2]
    if cmd == "build":
        if os.path.isdir(dest) and os.listdir(dest):
            print(f"refusing to build into non-empty directory: {dest}",
                  file=sys.stderr)
            return 1
        build(dest)
    else:
        apply_edit(dest)
    print(dest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
