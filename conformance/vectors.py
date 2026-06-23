"""Packaging's own test vectors: download the matching packaging sdist, harvest every
string literal from its test suite, and compare pyreq against packaging on that corpus.

    pip install packaging
    cd conformance/harness && cargo build --release && cd ..
    python vectors.py
"""
import ast
import io
import itertools
import os
import re
import subprocess
import sys
import tarfile
import tempfile
from collections import Counter

import packaging

from _common import b2s, load_packaging, run_batch

Version, Specifier, SpecifierSet, Marker, Requirement, U = load_packaging()
VER = packaging.__version__


def fetch_test_sources():
    """Download the packaging sdist matching the installed version, return {file: source}."""
    tmp = tempfile.mkdtemp(prefix="pyreq-vectors-")
    print(f"downloading packaging=={VER} sdist ...", file=sys.stderr)
    r = subprocess.run(
        [sys.executable, "-m", "pip", "download", f"packaging=={VER}",
         "--no-binary", ":all:", "--no-deps", "-d", tmp],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit("pip download failed:\n" + r.stderr)
    tgz = next((os.path.join(tmp, f) for f in os.listdir(tmp) if f.endswith(".tar.gz")), None)
    if not tgz:
        sys.exit("no sdist downloaded")
    out = {}
    with tarfile.open(tgz) as tf:
        for m in tf.getmembers():
            name = os.path.basename(m.name)
            if "/tests/" in m.name.replace("\\", "/") and name.startswith("test_") and name.endswith(".py"):
                key = name[len("test_"):-len(".py")]
                out[key] = tf.extractfile(m).read().decode("utf-8", "replace")
    return out


def strings(src):
    return [n.value for n in ast.walk(ast.parse(src)) if isinstance(n, ast.Constant) and isinstance(n.value, str)]


def vparse(s):
    try:
        return Version(s)
    except Exception:
        return None


sources = fetch_test_sources()
OPRE = re.compile(r"^(===|==|!=|~=|<=|>=|<|>)")
versions, specs, markers, reqs, names, wheels, sdists = set(), set(), set(), set(), set(), set(), set()

for s in strings(sources.get("version", "")):
    versions.add(s)
for s in strings(sources.get("specifiers", "")):
    (specs if OPRE.match(s) else versions if vparse(s) else set()).add(s)
for s in strings(sources.get("utils", "")):
    if s.endswith(".whl"):
        wheels.add(s)
    elif s.endswith(".tar.gz") or s.endswith(".zip"):
        sdists.add(s)
    elif vparse(s):
        versions.add(s)
    else:
        names.add(s)
for s in strings(sources.get("markers", "")):
    if ('"' in s or "'" in s) and re.search(r"(python_version|os[._]name|sys[._]platform|platform_|implementation_|extra| in |==|!=|<=|>=)", s):
        markers.add(s)
for s in strings(sources.get("requirements", "")):
    if re.match(r"^[A-Za-z0-9]", s) and not OPRE.match(s) and len(s) > 1:
        reqs.add(s)
    if vparse(s):
        versions.add(s)

valid = sorted(v for v in versions if vparse(v) is not None and "\t" not in v and "\n" not in v)
print(f"harvested from packaging {VER}: {len(versions)} versions, {len(specs)} specs, "
      f"{len(markers)} markers, {len(reqs)} reqs, {len(names)} names, {len(wheels)} wheels, {len(sdists)} sdists",
      file=sys.stderr)

cmds = []
def add(i, e):
    cmds.append((i, e))


def tabsafe(s):
    return "\t" not in s and "\n" not in s


for v in sorted(versions):
    if not tabsafe(v):
        continue
    p = vparse(v)
    add(f"V\t{v}", f"ok\t{p}\t{b2s(p.is_prerelease)}" if p else "err")
    for st in ("1", "0"):
        try:
            add(f"CV\t{v}\t{st}", U.canonicalize_version(v, strip_trailing_zero=(st == "1")))
        except Exception:
            add(f"CV\t{v}\t{st}", "EXC")

samp = [valid[i] for i in range(0, len(valid), max(1, len(valid) // 120))]
for a, b in itertools.product(samp, samp):
    pa, pb = vparse(a), vparse(b)
    add(f"VC\t{a}\t{b}", "lt" if pa < pb else ("eq" if pa == pb else "gt"))

for s in sorted(specs):
    if not tabsafe(s):
        continue
    try:
        sp = Specifier(s)
        add(f"S\t{s}", "ok")
    except Exception:
        sp = None
        add(f"S\t{s}", "err")
    for v in valid:
        if sp is None:
            add(f"SC\t{s}\t{v}", "err")
        else:
            try:
                add(f"SC\t{s}\t{v}", b2s(sp.contains(v)))
            except Exception:
                add(f"SC\t{s}\t{v}", "EXC")

for m in sorted(markers):
    if tabsafe(m):
        try:
            add(f"M\t{m}", f"ok\t{Marker(m)}")
        except Exception:
            add(f"M\t{m}", "err")
for r in sorted(reqs):
    if tabsafe(r):
        try:
            add(f"R\t{r}", f"ok\t{Requirement(r)}")
        except Exception:
            add(f"R\t{r}", "err")
for n in sorted(names):
    if tabsafe(n):
        add(f"CN\t{n}", U.canonicalize_name(n))
        add(f"IN\t{n}", b2s(U.is_normalized_name(n)))
for w in sorted(wheels):
    if tabsafe(w):
        try:
            nm, ver, _, _ = U.parse_wheel_filename(w)
            add(f"WH\t{w}", f"ok\t{nm}\t{ver}")
        except Exception:
            add(f"WH\t{w}", "err")
for sd in sorted(sdists):
    if tabsafe(sd):
        try:
            nm, ver = U.parse_sdist_filename(sd)
            add(f"SD\t{sd}", f"ok\t{nm}\t{ver}")
        except Exception:
            add(f"SD\t{sd}", "err")

got = run_batch([c[0] for c in cmds])
mism = [(l, e, g) for (l, e), g in zip(cmds, got) if e != g]
real = [(l, e, g) for l, e, g in mism if not (e == "EXC" or l.startswith("SC\t==="))]
print(f"cases {len(cmds)}  mismatches {len(mism)}  (known raises {len(mism) - len(real)})  REAL {len(real)}")
print("real by category:", dict(Counter(l.split('\t')[0] for l, _, _ in real)))
for l, e, g in real[:40]:
    print(f"  {l!r}\n    packaging={e!r}\n    pyreq    ={g!r}")
sys.exit(1 if real else 0)
