// Conformance harness: read tab-separated commands on stdin, emit one result line each.
// Driven by the Python scripts in ../, which compare each line against packaging's answer.
use pyreq::{
    Marker, Requirement, Specifier, SpecifierSet, Version, canonicalize_name, canonicalize_version,
    is_normalized_name, parse_sdist_filename, parse_wheel_filename,
};
use std::io::{self, BufRead, Write};

fn ver_line(v: &str) -> String {
    match Version::parse(v) {
        Ok(p) => format!("ok\t{}\t{}", p, p.is_prerelease()),
        Err(_) => "err".to_string(),
    }
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        let f: Vec<&str> = line.split('\t').collect();
        let res = match f[0] {
            "V" => ver_line(f[1]),
            "VC" => match (Version::parse(f[1]), Version::parse(f[2])) {
                (Ok(a), Ok(b)) => match a.cmp(&b) {
                    std::cmp::Ordering::Less => "lt".into(),
                    std::cmp::Ordering::Equal => "eq".into(),
                    std::cmp::Ordering::Greater => "gt".into(),
                },
                _ => "err".into(),
            },
            "CN" => canonicalize_name(f[1]),
            "IN" => is_normalized_name(f[1]).to_string(),
            "CV" => canonicalize_version(f[1], f[2] == "1"),
            "S" => match Specifier::parse(f[1]) {
                Ok(_) => "ok".into(),
                Err(_) => "err".into(),
            },
            "SC" => match Specifier::parse(f[1]) {
                Ok(s) => s.contains(f[2], None).to_string(),
                Err(_) => "err".into(),
            },
            "SS" => match SpecifierSet::parse(f[1]) {
                Ok(_) => "ok".into(),
                Err(_) => "err".into(),
            },
            "SSC" => match SpecifierSet::parse(f[1]) {
                Ok(s) => s.contains(f[2], None).to_string(),
                Err(_) => "err".into(),
            },
            "M" => match Marker::parse(f[1]) {
                Ok(m) => format!("ok\t{m}"),
                Err(_) => "err".into(),
            },
            "R" => match Requirement::parse(f[1]) {
                Ok(r) => format!("ok\t{r}"),
                Err(_) => "err".into(),
            },
            "WH" => match parse_wheel_filename(f[1], false) {
                Ok((n, v, _, _)) => format!("ok\t{n}\t{v}"),
                Err(_) => "err".into(),
            },
            "SD" => match parse_sdist_filename(f[1]) {
                Ok((n, v)) => format!("ok\t{n}\t{v}"),
                Err(_) => "err".into(),
            },
            other => format!("UNKNOWN:{other}"),
        };
        writeln!(out, "{res}").unwrap();
        out.flush().unwrap();
    }
}
