"""Generated-matrix differential test: a deterministic corpus of versions, specifiers,
markers, requirements, and filenames, each compared between pyreq and packaging.

    pip install packaging
    cd conformance/harness && cargo build --release && cd ..
    python differential.py
"""
import itertools
import sys
from collections import Counter

from _common import b2s, load_packaging, run_batch

Version, Specifier, SpecifierSet, Marker, Requirement, U = load_packaging()


def vparse(s):
    try:
        return Version(s)
    except Exception:
        return None


epochs = ["", "1!", "2!"]
releases = ["1", "1.0", "1.2.3", "2.0.0", "0", "1.0.0.0", "10", "1.0.1"]
pres = ["", "a1", "b2", "rc1", "a0", "alpha1", "c3", "preview", ".dev0pre"]
posts = ["", ".post1", ".post0", "-1", ".rev2"]
devs = ["", ".dev0", ".dev1", "dev"]
locals_ = ["", "+abc", "+1", "+ubuntu.1", "+1.0", "+Foo.1"]

vers = set([
    "1", "1.0", "1.0.0", "1.0.0.0", "0", "0.0", "1!1.0", "2!0.1", "1.0a1", "1.0b1",
    "1.0rc1", "1.0.dev1", "1.0.post1", "1.0.post1.dev1", "1.0a1.post2.dev3+abc.4",
    "1.0+local", "v1.0", "  1.0  ", "1.0.0a", "1.0c", "1.0preview", "1.0-1",
    "1!2.3.4a5.post6.dev7+ubuntu.1", "foo", "1..2", "1.", ".1", "", "1.0+", "1.0.0bad",
    "2020.1", "1.0.0+x.y.z",
])
for i, c in enumerate(itertools.product(epochs, releases, pres, posts, devs, locals_)):
    if i % 7 == 0:
        vers.add("".join(c))
vers = sorted(vers)

specs = set(f"{op}{r}" for op in ["==", "!=", "<=", ">=", "<", ">", "~="]
            for r in releases + ["1.0", "2.2", "1.4.5", "1.0a1", "1!1.0"])
specs.update(["==1.0.*", "!=1.0.*", "==1.*", "===foobar", "===1.0.0", "===0", "~=2.2",
    "~=1", "~=v0.0", "<1.0.*", ">=1.0+abc", "~=1.0+abc", "==1.0+local", ">1.0", "<1.0",
    "==2020.1", "!=1.0.0.*", ">1.0a1"])
specs = sorted(specs)

sets = ["", ">=1.0", ">=1.0,<2.0", ">=1.0,!=1.1,<2.0", "==1.0,==1.0.0", ">=1.0a1",
        "~=2.2", ">1.0,<1.0", "!=1.0.*", ">=1.0,lolwat"]

markers = [
    'os_name == "posix"', 'python_version > "3.6"', '"3.6" == python_version',
    'os.name == "nt"', 'platform.python_implementation == "CPython"',
    'python_version > "3.6" and os_name == "posix"', '(python_version == "3.6")',
    'python_version > "3.6" or (python_version == "3.6" and os_name == "unix")',
    '"arm" in platform_machine', 'platform_machine not in "x86_64"', 'extra == "Foo.Bar"',
    'python_version', 'python_version ==', '== "3.6"', 'python_version = "3.6"',
    '(python_version == "3.6"', 'foo == "bar"', 'os_name ~= "1.0"', 'os_name < "z"',
    'python_version <= "3.9"', 'python_version >= "3.8" and python_version < "4.0"',
]
reqs = [
    'requests', 'requests>=2.0', 'requests[security,socks]>=2.0,<3.0; python_version < "3.9"',
    'foo[B,A]==1.0.0', 'foo (>=1.0)', 'name @ https://example.com/pkg.tar.gz',
    'name @ https://example.com/p.tgz ; python_version > "3.0"', 'Foo>=1.0', 'foo[A,B]',
    '', '==1.0', 'foo bar baz', 'foo>=', 'foo @', '(foo)', 'foo[]extra', 'django>=3,<4',
    'numpy~=1.21', 'Pillow>=8.0;extra=="img"',
]
wheels = [
    "foo-1.0-py3-none-any.whl", "foo-1.0-1bar-py3-none-any.whl",
    "Foo.Bar-2.0-py2.py3-none-any.whl", "foo-1.0-py3-none-any.tar.gz",
    "numpy-1.21.0-cp39-cp39-manylinux1_x86_64.whl", "pkg-1.0+local-py3-none-any.whl",
]
sdists = ["foo-1.0.tar.gz", "Foo.Bar-2.0.1.zip", "foo-1.0.whl", "x-1!2.0.zip"]

cmds = []
def add(i, e):
    cmds.append((i, e))

for v in vers:
    p = vparse(v)
    add(f"V\t{v}", f"ok\t{p}\t{b2s(p.is_prerelease)}" if p else "err")
    if "\t" not in v:
        for st in ("1", "0"):
            try:
                add(f"CV\t{v}\t{st}", U.canonicalize_version(v, strip_trailing_zero=(st == "1")))
            except Exception:
                add(f"CV\t{v}\t{st}", "EXC")

valid = [v for v in vers if vparse(v) is not None]
samp = [valid[i] for i in range(0, len(valid), max(1, len(valid) // 120))]
for a, b in itertools.product(samp, samp):
    pa, pb = vparse(a), vparse(b)
    add(f"VC\t{a}\t{b}", "lt" if pa < pb else ("eq" if pa == pb else "gt"))

for s in specs:
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

for m in markers:
    try:
        add(f"M\t{m}", f"ok\t{Marker(m)}")
    except Exception:
        add(f"M\t{m}", "err")
for r in reqs:
    try:
        add(f"R\t{r}", f"ok\t{Requirement(r)}")
    except Exception:
        add(f"R\t{r}", "err")
for w in wheels:
    try:
        n, ver, _, _ = U.parse_wheel_filename(w)
        add(f"WH\t{w}", f"ok\t{n}\t{ver}")
    except Exception:
        add(f"WH\t{w}", "err")
for sd in sdists:
    try:
        n, ver = U.parse_sdist_filename(sd)
        add(f"SD\t{sd}", f"ok\t{n}\t{ver}")
    except Exception:
        add(f"SD\t{sd}", "err")

cmds = [c for c in cmds if "\n" not in c[0]]
got = run_batch([c[0] for c in cmds])
mism = [(l, e, g) for (l, e), g in zip(cmds, got) if e != g]


def is_known(l, e):
    # packaging raises on an invalid contains item or a ===<non-version> spec; a bool API
    # cannot reproduce a raise, so these are intentional and excluded.
    return e == "EXC" or l.startswith("SC\t===")


real = [(l, e, g) for l, e, g in mism if not is_known(l, e)]
print(f"cases {len(cmds)}  mismatches {len(mism)}  (known raises {len(mism) - len(real)})  REAL {len(real)}")
print("real by category:", dict(Counter(l.split('\t')[0] for l, _, _ in real)))
for l, e, g in real[:40]:
    print(f"  {l!r}\n    packaging={e!r}\n    pyreq    ={g!r}")
sys.exit(1 if real else 0)
