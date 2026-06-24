# pyreq

A Rust port of Python [`packaging`](https://github.com/pypa/packaging)'s
[PEP 440](https://peps.python.org/pep-0440/) versions and
[PEP 508](https://peps.python.org/pep-0508/) dependency specifiers. Version parsing and
ordering, version specifiers, environment markers, and requirements all mirror the reference
implementation, ported directly from its source.

## Features

- **Versions** (`Version`): PEP 440 parsing with full normalization, total ordering
  (`1.0.dev1 < 1.0a1 < 1.0rc1 < 1.0 < 1.0.post1`), equality/hashing that ignore insignificant
  differences (`1.0 == 1.0.0`), canonical `Display`, component accessors, and `from_parts`.
- **Specifiers** (`Specifier`, `SpecifierSet`): `~= == != <= >= < > ===`, wildcards
  (`==1.0.*`), comma-joined sets, `contains`, `filter`, and the PEP 440 pre-release rules.
- **Markers** (`Marker`): PEP 508 environment markers: parse, canonical `Display`, and
  evaluation against an environment.
- **Requirements** (`Requirement`): full dependency specifiers such as
  `requests[security]>=2.0; python_version<"3.9"`.
- `utils`: name normalization (`canonicalize_name`, `is_normalized_name`),
  version canonicalization (`canonicalize_version`), and filename parsing
  (`parse_wheel_filename`, `parse_sdist_filename`) returning name, `Version`, build tag, and
  `Tag` set.

## Installation

```sh
cargo add pyreq
```

Or add it to `Cargo.toml`:

```toml
[dependencies]
pyreq = "0.1"
```

Requires a Rust toolchain with 2024-edition support (Rust 1.85 or newer).

## Usage

### Versions

```rust
use pyreq::Version;

let v: Version = "1!2.3.4rc1.post2".parse().unwrap();
assert_eq!(v.release(), &[2, 3, 4]);
assert!(v.is_prerelease());

// Ordering and equality follow PEP 440.
assert!("1.0rc1".parse::<Version>().unwrap() < "1.0".parse().unwrap());
assert_eq!("1.0".parse::<Version>().unwrap(), "1.0.0".parse().unwrap());
```

### Specifiers

```rust
use pyreq::SpecifierSet;

let specs: SpecifierSet = ">=1.0,<2.0".parse().unwrap();
assert!(specs.contains("1.5", None));
assert!(!specs.contains("2.0", None));
```

### Requirements

```rust
use pyreq::Requirement;

let req = Requirement::parse("requests[security]>=2.0; python_version<\"3.9\"").unwrap();
assert_eq!(req.name, "requests");
assert!(req.marker.is_some());
```

### Markers

```rust
use std::collections::HashMap;
use pyreq::Marker;

let marker = Marker::parse("python_version >= \"3.8\"").unwrap();
let env = HashMap::from([("python_version".to_string(), "3.11".to_string())]);
assert_eq!(marker.evaluate(&env), Ok(true));
```

### Utilities

```rust
use pyreq::{canonicalize_name, parse_wheel_filename};

assert_eq!(canonicalize_name("Foo.Bar"), "foo-bar");

let (name, version, build, tags) =
    parse_wheel_filename("requests-2.31.0-py3-none-any.whl", false).unwrap();
assert_eq!(name, "requests");
assert_eq!(version.to_string(), "2.31.0");
assert!(build.is_none());
assert_eq!(tags.len(), 1);
```

## Compatibility

The version, specifier, marker, and requirement layers are ported from `packaging`'s source
and verified against it. Two test layers ship in this repository:

- `cargo test`: 87 unit tests across versions, specifiers, markers, requirements,
  utilities, and tag parsing.
- `conformance/`: differential conformance suite that compares `pyreq` against `packaging`
  25.0 input by input. Runs three ways, all reproducible:
  - a deterministic generated matrix (~348k version/specifier/marker/requirement/filename cases),
  - `packaging`'s own test vectors, harvested from its sdist (~70k cases),
  - a property-based fuzz where `hypothesis` generates PEP 440 / PEP 508 inputs.

  The latest run was clean: zero value divergences across roughly 800k generated cases plus
  360k hypothesis-generated cases. The one intentional difference is that
  `Specifier("===<non-PEP-440-string>").contains(...)` raises in `packaging` but returns
  `false` here, because `contains` returns `bool` and cannot raise. See
  [`conformance/README.md`](conformance/README.md) to build the harness and reproduce or
  compare against your own `packaging` version.

Notes: version components are stored as 64-bit integers, so values beyond `2^64 − 1` are
unsupported (they do not occur in practice). `Marker::evaluate` takes an explicit environment
rather than synthesizing a platform default: a standalone Rust crate is not running inside the
Python interpreter it describes, so `python_version`, `implementation_name`, and the like have
no source to read. It does apply the data-only transforms `packaging` does to a supplied
environment: `extra` is canonicalized (PEP 685) and `python_full_version` is repaired for
non-tagged builds. `Marker::evaluate` takes string-only values; `Marker::evaluate_with_context`
takes an environment whose values may be sets (`EnvValue::Str` or `EnvValue::Set`) and an
`EvaluateContext` (`Metadata`, `LockFile`, `Requirement`) that injects the empty `extra` /
`extras` / `dependency_groups` defaults, so set-valued markers like `"cpu" in extras` evaluate.

## Scope

Covers PEP 440 versions and PEP 508 dependency specifiers (versions, specifiers, markers, and
requirements), plus the host-independent utilities: name/version normalization, wheel and sdist
filename parsing, and `Tag`/`parse_tag` for tag strings. Out of scope are the parts of
`packaging` that introspect the running interpreter or OS and so cannot exist in a standalone
crate: tag generation (`sys_tags`, `cpython_tags`, manylinux/musllinux/mac platform
enumeration) and the metadata module.

## License

Licensed under the [BSD 2-Clause License](LICENSE-BSD), one of the licenses the upstream
`packaging` project offers.
