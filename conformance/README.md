# Conformance suite

Differential tests that compare `pyreq` directly against Python
[`packaging`](https://github.com/pypa/packaging) (the reference implementation this crate
ports), input by input. The port targets `packaging` 25.0.

A small Rust binary (`harness/`) exposes `pyreq` over a line protocol; Python scripts generate
inputs, compute `packaging`'s answer for each, run the same inputs through the harness, and
diff the two. Any disagreement is printed with both answers.

## Requirements

- A Rust toolchain (to build the harness).
- Python 3.9+ with `packaging>=25` and, for the property test, `hypothesis`:

  ```sh
  pip install "packaging>=25" hypothesis
  ```

  If no `packaging>=25` is installed, the scripts fall back to the copy pip vendors. Force a
  choice with `PYREQ_PACKAGING=installed` or `PYREQ_PACKAGING=vendored`.

## Build the harness

```sh
cd conformance/harness
cargo build --release
cd ..
```

## Run

```sh
python differential.py     # deterministic generated matrix of versions/specs/markers/...
python vectors.py          # packaging's own test vectors (downloads the matching sdist)
python property.py         # hypothesis-generated PEP 440 / PEP 508 inputs
EXAMPLES=60000 python property.py   # heavier property run
```

Each script prints `... REAL 0` and exits 0 when `pyreq` matches `packaging` on every case.

## What is checked

Versions (parse, normalize, `is_prerelease`, total ordering), specifiers and sets
(`parse`, `contains`), markers and requirements (`parse` + canonical `Display`), name and
version canonicalization, `is_normalized_name`, and wheel/sdist filename parsing.

## Known intentional divergence

`Specifier("===<non-PEP-440-string>").contains(...)` raises in `packaging` (its `prereleases`
property parses the arbitrary string and fails). `pyreq`'s `contains` returns `bool` and cannot
raise, so it returns `false`. The scripts exclude these `===`/`EXC` cases from the real-mismatch
count, since a raise is not a value a `bool` API can reproduce.

## Protocol

The harness reads one tab-separated command per stdin line and writes one result line. For
example `V\t1.0a1` to parse a version, `SC\t>=1.0\t1.5` for `Specifier(">=1.0").contains("1.5")`.
See `harness/src/main.rs` for the full command set.
