//! Which packages `--exclude` (and the `--exclude-*-crates` aliases) refer to.
//!
//! This used to be `krates::PkgSpec`. krates 0.21 stopped re-exporting
//! `cargo_metadata` and grew its own metadata types, so its `PkgSpec::matches` no
//! longer accepts a `cargo_metadata::Package`. Since `PkgSpec` was the only thing
//! this crate used from krates, the type lives here now and krates is gone.
//!
//! The accepted syntax is deliberately narrower than `cargo pkgid`:
//!
//! ```text
//! name
//! name@version
//! name:version    (cargo's older spelling, accepted for convenience)
//! ```
//!
//! A registry URL (`https://…/crates.io-index#name@version`) is **rejected**. The
//! preset lists used to be written that way and matched nothing at all, because
//! krates 0.8 looked the URL up in `PackageId::repr` by splitting on spaces and
//! cargo had since changed ids to a space-free form. Excluding by name and version
//! is what this tool actually wants, so the URL is refused outright rather than
//! silently ignored.

use cargo_metadata as cm;
use semver::Version;
use std::{fmt, str::FromStr};

/// A package name with an optional exact version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgSpec {
    name: String,
    version: Option<Version>,
}

impl PkgSpec {
    /// Whether `package` is the one this spec names. With no version, every version
    /// of the package matches.
    pub fn matches(&self, package: &cm::Package) -> bool {
        package.name == self.name
            && match &self.version {
                Some(version) => *version == package.version,
                None => true,
            }
    }
}

impl fmt::Display for PkgSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(version) => write!(f, "{}@{}", self.name, version),
            None => f.write_str(&self.name),
        }
    }
}

impl FromStr for PkgSpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if s.contains("://") {
            return Err(format!(
                "`{}`: a registry URL is not supported here. Write `name` or \
                 `name@version`",
                s,
            ));
        }

        let (name, version) = match s.split_once(['@', ':']) {
            Some((name, version)) => {
                let version = version.parse::<Version>().map_err(|e| {
                    format!("`{}`: could not parse `{}` as a version: {}", s, version, e)
                })?;
                (name, Some(version))
            }
            None => (s, None),
        };

        if name.is_empty()
            || !name
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_ascii_alphanumeric())
        {
            return Err(format!("`{}`: `{}` is not a package name", s, name));
        }

        Ok(Self {
            name: name.to_owned(),
            version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::PkgSpec;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses() {
        let spec = |s: &str| s.parse::<PkgSpec>().map(|s| s.to_string());

        assert_eq!(Ok("proconio".to_owned()), spec("proconio"));
        assert_eq!(Ok("maplit@1.0.2".to_owned()), spec("maplit@1.0.2"));
        // cargo's older `name:version` spelling parses to the same thing.
        assert_eq!(Ok("maplit@1.0.2".to_owned()), spec("maplit:1.0.2"));
        assert_eq!(Ok("rustc-hash@1.1.0".to_owned()), spec("rustc-hash@1.1.0"));
        assert_eq!(
            Ok("lazy_static@1.5.0".to_owned()),
            spec("lazy_static@1.5.0")
        );

        // A URL is refused rather than silently matching nothing.
        assert!(spec("https://github.com/rust-lang/crates.io-index#maplit:1.0.2").is_err());
        // Versions must be complete; `0.5` is a range, not a version.
        assert!(spec("proconio@0.5").is_err());
        assert!(spec("").is_err());
        assert!(spec("not a name").is_err());
    }
}
