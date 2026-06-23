"""Property-based differential test: hypothesis generates PEP 440 / PEP 508 inputs,
each is compared between pyreq (via the harness) and packaging.

    pip install hypothesis packaging
    cd conformance/harness && cargo build --release && cd ..
    python property.py            # ~12k examples per property
    EXAMPLES=60000 python property.py
"""
import os
import sys

from hypothesis import HealthCheck, assume, given, settings, strategies as st

from _common import Client, b2s, load_packaging

Version, Specifier, SpecifierSet, Marker, Requirement, U = load_packaging()
H = Client()
EXAMPLES = int(os.environ.get("EXAMPLES", "12000"))


def vparse(s):
    try:
        return Version(s)
    except Exception:
        return None


release = st.lists(st.integers(0, 6), min_size=1, max_size=4).map(lambda xs: ".".join(map(str, xs)))
epoch = st.sampled_from(["", "0!", "1!", "2!", "10!"])
pre = st.sampled_from(["", "a1", "b2", "rc1", "a0", "alpha", "alpha3", "c3", "preview", "preview2", ".pre1", "_b_2", "-rc-4"])
post = st.sampled_from(["", ".post1", ".post0", "-1", "-5", ".rev2", "_post3", "post"])
dev = st.sampled_from(["", ".dev0", ".dev1", "dev", "_dev_2", "-dev3"])
local = st.sampled_from(["", "+abc", "+1", "+ubuntu.1", "+1.0", "+Foo_1-x", "+a.b.c.0"])
lead = st.sampled_from(["", "v", " ", "  ", "v "])
tail = st.sampled_from(["", " ", "  "])


@st.composite
def version_str(draw):
    return draw(lead) + draw(epoch) + draw(release) + draw(pre) + draw(post) + draw(dev) + draw(local) + draw(tail)


junk = st.text(alphabet="0123456789.abrcdevpostlphi+!-_ ", min_size=0, max_size=12)
any_version = st.one_of(version_str(), junk)
op = st.sampled_from(["==", "!=", "<=", ">=", "<", ">", "~=", "==="])


@st.composite
def specifier_str(draw):
    o = draw(op)
    v = draw(version_str())
    if draw(st.booleans()) and o in ("==", "!="):
        v = v.rstrip() + ".*"
    return o + v


names = st.text(alphabet="abcABC0123._-", min_size=0, max_size=10)
NO_TAB = lambda s: "\t" not in s and "\n" not in s
S = settings(max_examples=EXAMPLES, deadline=None, suppress_health_check=[HealthCheck.too_slow])


@S
@given(v=any_version)
def test_version_parse(v):
    assume(NO_TAB(v))
    p = vparse(v)
    exp = f"ok\t{p}\t{b2s(p.is_prerelease)}" if p is not None else "err"
    assert H.ask(f"V\t{v}") == exp, f"V {v!r}: py={exp!r}"


@S
@given(a=version_str(), b=version_str())
def test_ordering(a, b):
    assume(NO_TAB(a) and NO_TAB(b))
    pa, pb = vparse(a), vparse(b)
    assume(pa is not None and pb is not None)
    exp = "lt" if pa < pb else ("eq" if pa == pb else "gt")
    assert H.ask(f"VC\t{a}\t{b}") == exp, f"VC {a!r} {b!r}: py={exp!r}"


@S
@given(s=specifier_str(), v=version_str())
def test_contains(s, v):
    assume(NO_TAB(s) and NO_TAB(v))
    try:
        sp = Specifier(s)
    except Exception:
        assert H.ask(f"SC\t{s}\t{v}") == "err"
        return
    if vparse(v) is None:
        return
    try:
        exp = b2s(sp.contains(v))
    except Exception:
        # packaging raises on ===<non-version> (its prereleases property parses the
        # arbitrary string and fails); a bool-returning API cannot reproduce that.
        assume(False)
        return
    assert H.ask(f"SC\t{s}\t{v}") == exp, f"SC {s!r} contains {v!r}: py={exp!r}"


@S
@given(s=specifier_str())
def test_specifier_parse(s):
    assume(NO_TAB(s))
    try:
        Specifier(s)
        exp = "ok"
    except Exception:
        exp = "err"
    assert H.ask(f"S\t{s}") == exp, f"S {s!r}: py={exp!r}"


@S
@given(n=names)
def test_canon_name(n):
    assume(NO_TAB(n))
    assert H.ask(f"CN\t{n}") == U.canonicalize_name(n), f"CN {n!r}"
    assert H.ask(f"IN\t{n}") == b2s(U.is_normalized_name(n)), f"IN {n!r}"


@S
@given(v=version_str(), strip=st.booleans())
def test_canon_version(v, strip):
    assume(NO_TAB(v))
    try:
        exp = U.canonicalize_version(v, strip_trailing_zero=strip)
    except Exception:
        assume(False)
        return
    assume(NO_TAB(exp))
    assert H.ask(f"CV\t{v}\t{'1' if strip else '0'}") == exp, f"CV {v!r} {strip}"


if __name__ == "__main__":
    tests = [test_version_parse, test_ordering, test_contains, test_specifier_parse,
             test_canon_name, test_canon_version]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"PASS  {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL  {t.__name__}\n   {e}")
    print(f"\n{EXAMPLES} examples/property. " + ("ALL PASS" if not failed else f"{failed} FAILED"))
    sys.exit(1 if failed else 0)
