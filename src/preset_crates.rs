//! The crate lists behind the `--exclude-{atcoder,codingame}-crates` aliases.
//!
//! The lists themselves live in `preset_crates/*.txt`, not in this file. Updating
//! them when a judge changes its environment is then a data edit rather than a
//! source edit, and the diff shows only the crates that actually moved.
//!
//! Each line is a [`PkgSpec`]. Blank lines and `#` comments are ignored. Order is
//! preserved as written, because it shows up verbatim in `--help`.

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
    use super::{ATCODER, ATCODER_202301, ATCODER_202510, CODINGAME};

    /// The lists used to be parsed with `.unwrap()` at runtime, so a typo in one of
    /// them was a panic in the middle of a run. Catch it here instead.
    #[test]
    fn presets_parse() -> anyhow::Result<()> {
        for preset in [ATCODER, ATCODER_202301, ATCODER_202510, CODINGAME] {
            assert!(!preset.parse()?.is_empty(), "`{}` is empty", preset.name);
        }
        Ok(())
    }
}
