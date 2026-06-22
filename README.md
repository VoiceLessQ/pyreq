# pyreq

A Rust port of Python [`packaging`](https://github.com/pypa/packaging)'s
[PEP 440](https://peps.python.org/pep-0440/) versions and
[PEP 508](https://peps.python.org/pep-0508/) dependency specifiers. Version parsing and
ordering, version specifiers, environment markers, and requirements all mirror the reference
implementation — verified by differential testing against Python across millions of cases.

## Features

- **Versions** (`Version`) — PEP 440 parsing with full normalization, total ordering
  (`1.0.dev1 < 1.0a1 < 1.0rc1 < 1.0 < 1.0.post1`), equality/hashing that ignore insignificant
  differences (`1.0 == 1.0.0`), canonical `Display`, component accessors, and `from_parts`.
- **Specifiers** (`Specifier`, `SpecifierSet`) — `~= == != <= >= < > ===`, wildcards
  (`==1.0.*`), comma-joined sets, `contains`, `filter`, and the PEP 440 pre-release rules.
- **Markers** (`Marker`) — PEP 508 environment markers: parse, canonical `Display`, and
  evaluation against an environment.
- **Requirements** (`Requirement`) — full dependency specifiers such as
  `requests[security]>=2.0; python_version<"3.9"`.

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

## Compatibility

Every layer is differentially tested against the reference Python `packaging` implementation:
60k+ version strings (validity, normalized output, total ordering), thousands of specifier and
requirement checks, and marker evaluations — all matching.

Notes: version components are stored as 64-bit integers, so values beyond `2^64 − 1` are
unsupported (they do not occur in practice). `Marker::evaluate` takes an explicit environment
rather than synthesizing a platform default.

## Scope

Covers PEP 440 versions and PEP 508 dependency specifiers — versions, specifiers, markers, and
requirements. Platform/format helpers from `packaging` (wheel tags, metadata, manylinux, etc.)
are out of scope.

## License

Licensed under the [BSD 2-Clause License](LICENSE-BSD), one of the licenses the upstream
`packaging` project offers.
