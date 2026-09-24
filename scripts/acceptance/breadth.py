#!/usr/bin/env python3
"""Who-calls accuracy across many symbols per language, against a textual oracle.

`run.py`'s language check proves one hand-verified function per language.
This measures more of each repo: it picks functions whose name is defined
exactly once (so the question "who calls it" is unambiguous), finds the
files that textually call them (`name(`, outside comments and the
definition line), and compares that file set with the files Lain reports
as callers.

The oracle is independent of Lain but approximate — a name in a string, a
same-named method on another type, or a call through a macro all fool it
— so disagreements are listed for review rather than trusted blindly.

Usage: breadth.py --lain PATH --work DIR [--per-lang 15] [--only LANG]
"""
import argparse, json, os, random, re, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run  # noqa: E402  (Mcp, checkout, SVELTE-free helpers)

HERE = os.path.dirname(os.path.abspath(__file__))

# (extensions, definition regex capturing the name, line-comment prefixes)
LANGS = {
    "Rust": ((".rs",), r"\bfn\s+([A-Za-z_]\w*)", ("//",)),
    # `>>>` lines are doctest examples inside docstrings, not calls.
    "Python": ((".py",), r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)", ("#", ">>>")),
    "TypeScript": ((".ts",), r"\bfunction\s+([A-Za-z_$][\w$]*)|\b(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?\(", ("//",)),
    "JavaScript": ((".js",), r"\bfunction\s+([A-Za-z_$][\w$]*)|\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?(?:function|\()", ("//",)),
    "Go": ((".go",), r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)", ("//",)),
    # Modifiers optional: package-private methods (`boolean isPrimitive()`)
    # count as definitions too, or a twice-defined name looks unique.
    "Java": ((".java",), r"^[ \t]*(?!return\b|new\b|throw\b|else\b|case\b)(?:@\w+(?:\([^)\n]*\))?[ \t]+)*(?:(?:public|private|protected|static|final|abstract|synchronized|default)[ \t]+)*(?:<[^>\n]+>[ \t]+)?[\w<>\[\],.?]+[ \t]+([a-z]\w*)[ \t]*\([^;\n]*$", ("//",)),
    "C": ((".c", ".h"), r"^(?:static\s+)?[A-Za-z_][\w\s\*]*?\b([A-Za-z_]\w*)\s*\([^;]*\)\s*\{?\s*$", ("//",)),
    "C++": ((".hpp", ".cpp", ".h"), r"^\s*(?:inline\s+|static\s+|virtual\s+)*[A-Za-z_][\w:<>,\s\*&]*?\b([a-z_]\w*)\s*\([^;]*\)\s*(?:const\s*)?\{?\s*$", ("//",)),
    "C#": ((".cs",), r"^[ \t]*(?!return\b|new\b|throw\b|else\b|case\b|await\b)(?:(?:public|private|protected|internal|static|virtual|override|async|sealed|abstract)[ \t]+)*[\w<>\[\],.?]+[ \t]+([A-Z]\w*)(?:<[^>\n]+>)?[ \t]*\([^;\n]*$", ("//",)),
    "Ruby": ((".rb",), r"^\s*def\s+(?:self\.)?([a-z_]\w*[!?]?)", ("#",)),
    "Swift": ((".swift",), r"\bfunc\s+([A-Za-z_]\w*)", ("//",)),
    "Kotlin": ((".kt",), r"\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)\s*\(", ("//",)),
    "Scala": ((".scala",), r"\bdef\s+([A-Za-z_]\w*)", ("//",)),
    "PHP": ((".php",), r"\bfunction\s+([A-Za-z_]\w*)", ("//", "#")),
}

# Names too generic to be unambiguous even when defined once here: the
# oracle would count calls to the standard library.
GENERIC = set("""main init new get set run add len map filter parse format write read
close open next equals hashCode toString describe apply update delete create build
test setup value values keys items append push pop call invoke execute handle process
start stop reset clear copy clone remove contains size length""".split())


def files_of(repo, exts):
    out = []
    for root, dirs, files in os.walk(repo):
        dirs[:] = [d for d in dirs if not d.startswith(".") and d not in ("node_modules", "vendor", "target", "build", "dist")]
        for f in files:
            if f.endswith(exts):
                out.append(os.path.join(root, f))
    return out


STRINGS = re.compile(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'')


def strip_comment(line, prefixes):
    s = line.lstrip()
    if any(s.startswith(p) for p in prefixes) or s.startswith("*") or s.startswith("/*"):
        return ""
    return line


def build_oracle(repo, lang):
    exts, def_re, comments = LANGS[lang]
    rx = re.compile(def_re, re.M)
    defs = {}  # name -> [(relpath, line_no)]
    texts = {}
    for path in files_of(repo, exts):
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            continue
        rel = os.path.relpath(path, repo)
        texts[rel] = text
        for m in rx.finditer(text):
            gi = next(i for i, g in enumerate(m.groups(), 1) if g)
            name = m.group(gi)
            # The line of the name itself: the Java/C# patterns can start
            # on an earlier line (modifiers, annotations).
            line = text.count("\n", 0, m.start(gi)) + 1
            defs.setdefault(name, []).append((rel, line))
    return defs, texts


def calling_files(name, texts, def_site, comments, lang=""):
    # `name(` with no space: `name (…)` is DSL or declaration syntax far
    # more often than a call (just's tests hold S-expressions in macros).
    call = re.compile(r"(?<![\w$.])" + re.escape(name) + r"\(|\.\s*" + re.escape(name) + r"\(")
    decl_src = r"\b(?:fn|def|func|fun|function)\s+" + re.escape(name) + r"\b"
    if lang in ("C", "C++"):
        # Prototype: `type name(params);`. Only for C/C++ — in Java a call
        # on a continuation line (`a, b, name(x));`) has the same shape.
        decl_src += (r"|^\s*(?!return\b)[A-Za-z_][\w\s\*&:<>]*[\s\*&]" + re.escape(name)
                     + r"\s*\([^=]*\)\s*(?:const|override|noexcept|final|\s)*;\s*$")
    decl = re.compile(decl_src)
    found = set()
    for rel, text in texts.items():
        for i, line in enumerate(text.split("\n"), 1):
            if (rel, i) == def_site:
                continue
            # Quoted text is not code: test fixtures often hold a DSL or
            # source snippets in strings.
            line = STRINGS.sub('""', strip_comment(line, comments))
            if call.search(line) and not decl.search(line):
                found.add(rel)
                break
    return found


REVIEWED = json.load(open(os.path.join(HERE, "breadth_reviewed.json")))["reviewed"]


def reviewed(lang, symbol, file, side):
    """A disagreement checked by hand and found to be the oracle's error."""
    return any(e["lang"] == lang and e["symbol"] == symbol and e["verdict"] == "oracle"
               and e["file"] in ("*", file) and e["side"] in ("*", side) for e in REVIEWED)


def lain_calling_files(mcp, name):
    out = mcp.call("get_call_sites", {"symbol": name})
    # "- **caller** (path) calls it at ..." — for module-scope callers the
    # caller *is* the file.
    files = set(re.findall(r"^- \*\*[^*]+\*\* \(([^)]+)\)", out, re.M))
    # A caller whose exact line could not be located is listed as
    # "- **name** — enclosing function at path:lo-hi (…)".
    files |= set(re.findall(r"^- \*\*[^*]+\*\* — enclosing function at (\S+?):\d", out, re.M))
    return files, out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--lain", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--per-lang", type=int, default=15)
    ap.add_argument("--only", default="")
    ap.add_argument("--json", default="")
    args = ap.parse_args()
    args.lain = os.path.abspath(args.lain)
    spec = json.load(open(os.path.join(HERE, "languages.json")))
    env = {**os.environ, "LAIN_TOOL_PROFILE": "full"}
    report = {}
    for case in spec["cases"]:
        lang = case["lang"]
        if lang not in LANGS or (args.only and args.only.lower() != lang.lower()):
            continue
        repo = run.checkout(args.work, case["repo"], case["sha"])
        exts, _, comments = LANGS[lang]
        defs, texts = build_oracle(repo, lang)
        unique = sorted(n for n, sites in defs.items()
                        if len(sites) == 1 and len(n) >= 6 and n not in GENERIC
                        and not n.startswith("__"))
        rng = random.Random(f"{lang}:{case['sha']}")
        with_callers, without = [], []
        for n in rng.sample(unique, len(unique)):
            if len(with_callers) >= args.per_lang - 3 and len(without) >= 3:
                break
            (with_callers if calling_files(n, texts, defs[n][0], comments, lang) else without).append(n)
        picked = with_callers[: args.per_lang - 3] + without[:3]
        mcp = run.Mcp(args.lain, repo, env)
        try:
            mcp.wait_ready()
            rows = []
            for n in picked:
                want = calling_files(n, texts, defs[n][0], comments, lang)
                got, raw = lain_calling_files(mcp, n)
                rows.append({"symbol": n, "defined": f"{defs[n][0][0]}:{defs[n][0][1]}",
                             "oracle": sorted(want), "lain": sorted(got),
                             "missing": sorted(want - got), "extra": sorted(got - want)})
        finally:
            mcp.close()
        tp = sum(len(set(r["oracle"]) & set(r["lain"])) for r in rows)
        fn = sum(len(r["missing"]) for r in rows)
        fp = sum(len(r["extra"]) for r in rows)
        exact = sum(1 for r in rows if not r["missing"] and not r["extra"])
        # Reviewed: drop disagreements checked by hand and found to be the
        # oracle's error; whatever is left counts against Lain.
        for row in rows:
            row["unexplained_missing"] = [f for f in row["missing"] if not reviewed(lang, row["symbol"], f, "missing")]
            row["unexplained_extra"] = [f for f in row["extra"] if not reviewed(lang, row["symbol"], f, "extra")]
        agree = sum(1 for r in rows if not r["unexplained_missing"] and not r["unexplained_extra"])
        report[lang] = {"repo": case["repo"], "symbols": len(rows), "exact": exact, "agree_after_review": agree,
                        "file_recall": tp / (tp + fn) if tp + fn else 1.0,
                        "file_precision": tp / (tp + fp) if tp + fp else 1.0, "rows": rows}
        r = report[lang]
        print(f"{lang:11} raw {r['exact']:2}/{r['symbols']:2} exact (recall {r['file_recall']:.2f}, "
              f"precision {r['file_precision']:.2f});  after review {agree:2}/{r['symbols']:2}  ({case['repo']})",
              flush=True)
        for row in rows:
            if row["unexplained_missing"] or row["unexplained_extra"]:
                print(f"    UNEXPLAINED {row['symbol']} @ {row['defined']}: "
                      f"missing {row['unexplained_missing']} extra {row['unexplained_extra']}")
    if args.json:
        json.dump(report, open(args.json, "w"), indent=1)


if __name__ == "__main__":
    main()
