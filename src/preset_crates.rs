//! The crate lists behind the `--exclude-{atcoder,codingame}-crates` aliases.
//!
//! The lists themselves live in `preset_crates/*.txt`, not in this file. Updating
//! them when a judge changes its environment is then a data edit rather than a
//! source edit, and the diff shows only the crates that actually moved.
//!
//! Each line is a [`PkgSpec`]. Blank lines and `#` comments are ignored. Order is
//! preserved as written, because it shows up verbatim in `--help`.
//!
//! Entries are `name` or `name:version` and must **not** carry the registry URL.
//! `PkgSpec::matches` pulls the URL out of `PackageId::repr` by splitting on spaces,
//! which no longer works: cargo now writes ids as
//! `registry+https://github.com/rust-lang/crates.io-index#name@version`, with no
//! spaces at all. A spec with a URL therefore matches nothing, silently. See
//! `no_urls_in_presets` below.

use anyhow::Context as _;
use krates::PkgSpec;

pub(crate) const ATCODER: Preset =
    Preset::new("atcoder", include_str!("preset_crates/atcoder.txt"));

pub(crate) const ATCODER_202301: Preset = Preset::new(
    "atcoder-202301",
    include_str!("preset_crates/atcoder-202301.txt"),
);

pub(crate) const ATCODER_202510: Preset = Preset::new(
    "atcoder-202510",
    include_str!("preset_crates/atcoder-202510.txt"),
);

pub(crate) const CODINGAME: Preset =
    Preset::new("codingame", include_str!("preset_crates/codingame.txt"));

pub(crate) const YUKICODER: Preset =
    Preset::new("yukicoder", include_str!("preset_crates/yukicoder.txt"));

pub(crate) struct Preset {
    name: &'static str,
    raw: &'static str,
}

impl Preset {
    const fn new(name: &'static str, raw: &'static str) -> Self {
        Self { name, raw }
    }

    /// The specs as written, in file order.
    pub(crate) fn specs(&self) -> impl Iterator<Item = &'static str> {
        self.raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
    }

    pub(crate) fn parse(&self) -> anyhow::Result<Vec<PkgSpec>> {
        self.specs()
            .map(|spec| {
                spec.parse::<PkgSpec>().with_context(|| {
                    format!("could not parse `{}` in the `{}` list", spec, self.name)
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Preset, ATCODER, ATCODER_202301, ATCODER_202510, CODINGAME, YUKICODER};

    const ALL: &[Preset] = &[
        ATCODER,
        ATCODER_202301,
        ATCODER_202510,
        CODINGAME,
        YUKICODER,
    ];

    /// The lists used to be parsed with `.unwrap()` at runtime, so a typo in one of
    /// them was a panic in the middle of a run. Catch it here instead.
    #[test]
    fn presets_parse() -> anyhow::Result<()> {
        for preset in ALL {
            assert!(!preset.parse()?.is_empty(), "`{}` is empty", preset.name);
        }
        Ok(())
    }

    /// A spec carrying a registry URL parses fine but matches nothing, so the whole
    /// list becomes a silent no-op. The lists used to be written that way and the
    /// `--exclude-*` aliases had quietly stopped excluding anything.
    #[test]
    fn no_urls_in_presets() {
        for preset in ALL {
            for spec in preset.specs() {
                assert!(
                    !spec.contains("://"),
                    "`{}` in `{}` carries a URL, which never matches any package",
                    spec,
                    preset.name,
                );
            }
        }
    }
}
