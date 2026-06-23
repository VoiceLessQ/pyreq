"""Shared helpers: locate the Rust harness binary and the packaging oracle."""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_EXE = ".exe" if os.name == "nt" else ""
BIN = os.path.join(HERE, "harness", "target", "release", "pyreq-conformance" + _EXE)


def require_binary():
    if not os.path.exists(BIN):
        sys.exit(
            "harness binary not found.\n"
            "Build it first:\n"
            "    cd conformance/harness && cargo build --release"
        )


def _ver_at_least_25(mod):
    try:
        return int(mod.__version__.split(".")[0]) >= 25
    except Exception:
        return False


def load_packaging():
    """Return packaging modules to compare against (the port targets packaging 25.0+).

    Prefers an installed ``packaging>=25``; if that is missing or older, falls back to the
    copy pip vendors, which is current. Set PYREQ_PACKAGING=installed|vendored to force one.
    """
    pref = os.environ.get("PYREQ_PACKAGING", "")
    pkg = None
    if pref != "vendored":
        try:
            import packaging as _p
            if pref == "installed" or _ver_at_least_25(_p):
                pkg = _p
        except ImportError:
            pass
    if pkg is None and pref != "installed":
        try:
            import pip._vendor.packaging as _p  # pip bundles a current packaging
            pkg = _p
        except ImportError:
            pass
    if pkg is None:
        sys.exit("need packaging>=25:  pip install 'packaging>=25'")
    name = pkg.__name__
    mods = __import__(name + ".version", fromlist=["x"]), __import__(name + ".specifiers", fromlist=["x"]), \
        __import__(name + ".markers", fromlist=["x"]), __import__(name + ".requirements", fromlist=["x"]), \
        __import__(name + ".utils", fromlist=["x"])
    print(f"comparing against {name} {pkg.__version__}", file=sys.stderr)
    return (mods[0].Version, mods[1].Specifier, mods[1].SpecifierSet,
            mods[2].Marker, mods[3].Requirement, mods[4])


def run_batch(commands):
    """Run a list of command strings through the harness, return result lines."""
    require_binary()
    inp = "\n".join(commands) + "\n"
    out = subprocess.run([BIN], input=inp, capture_output=True, encoding="utf-8").stdout
    return out.split("\n")


class Client:
    """Persistent harness process for one-command-at-a-time (property test) use."""

    def __init__(self):
        require_binary()
        self.p = subprocess.Popen(
            [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, encoding="utf-8"
        )

    def ask(self, cmd):
        self.p.stdin.write(cmd + "\n")
        self.p.stdin.flush()
        return self.p.stdout.readline().rstrip("\n")


def b2s(x):
    return "true" if x else "false"
