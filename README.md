# packaging

A Rust implementation of [PEP 440](https://peps.python.org/pep-0440/) version identifiers:
parsing, comparison, and normalization. It mirrors the behaviour of Python's
[`packaging`](https://github.com/pypa/packaging) library — the same strings parse the same
way, normalize to the same canonical form, and sort in the same order.

## Features

- Parse version strings into structured form, with full PEP 440 normalization (spelling
  aliases such as `alpha`→`a` and `c`→`rc`, implicit pre/post numbers, the `1.0-1` post
  form, and local-label separators).
- Total ordering that follows PEP 440: `1.0.dev1 < 1.0a1 < 1.0rc1 < 1.0 < 1.0.post1`,
  including epochs and correctly ordered local labels.
- Equality and hashing that ignore insignificant differences (`1.0 == 1.0.0`).
- Canonical string output via `Display`.
- Read every component (epoch, release, pre, post, dev, local) plus helpers like `major`,
  `minor`, `micro`, `is_prerelease`, and `base_version`.
- Build versions directly from their parts, without going through a string.

## Installation

```sh
cargo add packaging
```

Or add it to `Cargo.toml`:

```toml
[dependencies]
packaging = "0.1"
```

Requires a Rust toolchain with 2024-edition support (Rust 1.85 or newer).

## Usage

### Parse and inspect

```rust
use packaging::Version;

let v: Version = "1!2.3.4rc1.post2".parse().unwrap();

assert_eq!(v.epoch(), 1);
assert_eq!(v.release(), &[2, 3, 4]);
assert_eq!(v.major(), 2);
assert_eq!(v.base_version(), "1!2.3.4");
assert!(v.is_prerelease());
```

### Compare and sort

```rust
use packaging::Version;

let mut versions: Vec<Version> = ["1.0", "1.0.post1", "1.0rc1", "1.0.dev1"]
    .iter()
    .map(|s| s.parse().unwrap())
    .collect();

versions.sort();

let sorted: Vec<String> = versions.iter().map(|v| v.to_string()).collect();
assert_eq!(sorted, ["1.0.dev1", "1.0rc1", "1.0", "1.0.post1"]);
```

### Equality ignores insignificant differences

```rust
use packaging::Version;

let a: Version = "1.0".parse().unwrap();
let b: Version = "1.0.0".parse().unwrap();
assert_eq!(a, b);
```

### Normalization

```rust
use packaging::Version;

// Leading `v`, surrounding whitespace, and alternate spellings all normalize.
let v: Version = "  v1.0ALPHA1  ".parse().unwrap();
assert_eq!(v.to_string(), "1.0a1");
```

### Build from parts

```rust
use packaging::{PreLetter, Version, VersionParts};

let v = Version::from_parts(VersionParts {
    release: vec![1, 2, 3],
    pre: Some((PreLetter::Rc, 1)),
    ..Default::default()
})
.unwrap();

assert_eq!(v.to_string(), "1.2.3rc1");
```

### Invalid input

```rust
use packaging::Version;

assert!("not.a.version".parse::<Version>().is_err());
```

## Compatibility

Behaviour is differentially tested against the reference Python `packaging` implementation
across a large generated corpus of version strings: validity, normalized output, and total
sort order all match.

Version components are stored as 64-bit integers, so version numbers beyond `2^64 − 1` are
not supported (these do not occur in practice).

## Scope

This crate currently covers PEP 440 **version identifiers**. Version specifiers
(`>=1.0,<2.0`) and requirement/marker parsing are not yet implemented.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[BSD 2-Clause License](LICENSE-BSD) at your option, matching the upstream `packaging` project.
