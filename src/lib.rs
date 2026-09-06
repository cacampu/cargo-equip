#![forbid(unsafe_code)]
#![warn(rust_2018_idioms)]
#![recursion_limit = "256"]

mod cargo_udeps;
mod preset_crates;
mod process;
mod ra_proc_macro;
mod rust;
mod rustfmt;
pub mod shell;
mod toolchain;
mod workspace;

use crate::{
    ra_proc_macro::ProcMacroExpander,
    rust::CodeEdit,
    shell::Shell,
    workspace::{
        Edition, MetadataExt as _, PackageExt as _, PackageIdExt as _, ResolveBehavior,
        TargetExt as _,
    },
};
use anyhow::Context as _;
use cargo_metadata as cm;
use clap::{Args, Parser, ValueEnum};
use indoc::indoc;
use itertools::{iproduct, Itertools as _};
use krates::PkgSpec;
use maplit::{btreeset, hashmap, hashset};
use petgraph::{
    graph::{Graph, NodeIndex},
    visit::Dfs,
};
use prettytable::{cell, format::FormatBuilder, row, Table};
use quote::quote;
use std::{
    cmp,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt::Debug,
    path::{Path, PathBuf},
    rc::Rc,
    str::FromStr,
};

// We need to prepend " " to `long_help`s.
// https://github.com/BurntSushi/ripgrep/blob/9eddb71b8e86a04d7048b920b9b50a2e97068d03/crates/core/app.rs#L533-L539

#[allow(clippy::large_enum_variant)]
#[derive(Parser, Debug)]
// `DeriveDisplayOrder` and `UnifiedHelpMessage` were clap 2 settings; both are the
// only behavior clap 4 has, so they are gone rather than translated.
#[command(
    author = "Ryo Yamashita <qryxip@gmail.com>",
    about = "Please run as `cargo equip`, not `cargo-equip`.",
    bin_name = "cargo",
    version
)]
pub enum Opt {
    #[command(
        version,
        // Without this the version line would read `cargo-equip-equip`, since clap
        // joins the bin name and the subcommand name. It does not affect matching.
        display_name = "cargo-equip",
        about = indoc! {r#"

            A Cargo subcommand to bundle your code into one `.rs` file for competitive programming.

            Use -h for short descriptions and --help for more detials.
        "#},
        author = "Ryo Yamashita <qryxip@gmail.com>",
        override_usage = r#"cargo equip [OPTIONS]
    cargo equip [OPTIONS] --lib
    cargo equip [OPTIONS] --bin <NAME>
    cargo equip [OPTIONS] --example <NAME>
    cargo equip [OPTIONS] --src <PATH>"#
    )]
    Equip(OptEquip),
}

#[derive(Args, Debug)]
pub struct OptEquip {
    /// Bundle the lib/bin/example target and its dependencies
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["lib", "bin", "example"],
        long_help = indoc! {r#"
            Bundle the lib/bin/example target and its dependencies.

            This option is intended to be used from editors such as VSCode. Use `--lib`, `--bin` or `--example` for normal usage.
        "#}
    )]
    src: Option<PathBuf>,

    /// Bundle the library and its dependencies
    #[arg(long, conflicts_with_all = ["bin", "example"])]
    lib: bool,

    /// Bundle the binary and its dependencies
    #[arg(long, value_name = "NAME", conflicts_with = "example")]
    bin: Option<String>,

    /// Bundle the binary example and its dependencies
    #[arg(long, value_name = "NAME")]
    example: Option<String>,

    /// Path to Cargo.toml
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,

    /// Exclude library crates from bundling
    #[arg(long, value_name = "SPEC", value_parser = validate_pkg_spec)]
    exclude: Vec<String>,

    /// Alias for `--exclude {crates available on AtCoder}`
    #[arg(
        long,
        long_help = &*Box::leak(
            format!(
                "Alias for:\n--exclude {}\n ",
                preset_crates::ATCODER.specs().format("\n          "),
            )
            .into_boxed_str(),
        )
    )]
    exclude_atcoder_crates: bool,

    /// Alias for `--exclude {crates available on AtCoder 202301}`
    #[arg(
        long,
        long_help = &*Box::leak(
            format!(
                "Alias for:\n--exclude {}\n ",
                preset_crates::ATCODER_202301.specs().format("\n          "),
            )
            .into_boxed_str(),
        )
    )]
    exclude_atcoder_202301_crates: bool,
    /// Alias for `--exclude {crates available on AtCoder 202510}`
    #[arg(
        long,
        long_help = &*Box::leak(
            format!(
                "Alias for:\n--exclude {}\n ",
                preset_crates::ATCODER_202510.specs().format("\n          "),
            )
            .into_boxed_str(),
        )
    )]
    exclude_atcoder_202510_crates: bool,

    /// Alias for `--exclude {crates available on CodinGame}`
    #[arg(
        long,
        long_help = &*Box::leak(
            format!(
                "Alias for:\n--exclude {}\n ",
                preset_crates::CODINGAME.specs().format("\n          "),
            )
            .into_boxed_str(),
        )
    )]
    exclude_codingame_crates: bool,

    /// Do not include license and copyright notices for the users
    #[arg(
        long,
        value_name = "DOMAIN_AND_USERNAME",
        long_help = concat!(
            indoc! {r#"
                Do not include license and copyright notices for the users.

                Supported formats:
                * github.com/{username}
                * gitlab.com/{username}
            "#},
            ' ',
        )
    )]
    mine: Vec<User>,

    /// [Deprecated] Alias for `--toolchain-for-udeps`
    #[arg(long, value_name = "TOOLCHAIN", conflicts_with = "toolchain_for_udeps")]
    toolchain: Option<String>,

    /// `nightly` toolchain for `cargo-udeps`
    #[arg(long, value_name = "TOOLCHAIN", default_value = "nightly")]
    toolchain_for_udeps: String,

    /// Toolchain for expanding procedural macros
    #[arg(long, value_name = "TOOLCHAIN")]
    toolchain_for_proc_macro_srv: Option<String>,

    /// Expand the libraries to the module
    #[arg(
        long,
        value_name = "MODULE_PATH",
        default_value = "crate::__cargo_equip"
    )]
    mod_path: CrateSinglePath,

    /// Remove some part [possible values: docs, comments]
    #[arg(
        long,
        value_name = "REMOVE",
        hide_possible_values = true,
        long_help = concat!(
            indoc! {r#"
                Removes
                * doc comments (`//! ..`, `/// ..`, `/** .. */`, `#[doc = ".."]`) with `--remove docs`.
                * comments (`// ..`, `/* .. */`) with `--remove comments`.

                ```
                #[allow(dead_code)]
                pub mod a {
                    //! A.

                    /// A.
                    pub struct A; // aaaaa
                }
                ```

                ↓

                ```
                #[allow(dead_code)]
                pub mod a {
                    pub struct A;
                }
                ```
            "#},
            ' ',
        )
    )]
    remove: Vec<Remove>,

    /// Minify part of the output before emitting [default: none]  [possible values: none, libs, all]
    #[arg(
        long,
        value_name = "MINIFY",
        hide_possible_values = true,
        default_value = "none",
        hide_default_value = true,
        long_help = concat!(
            indoc! {r#"
                Minifies
                - each expaned library with `--minify lib`.
                - the whole code with `--minify all`.

                Not that the minification function is incomplete. Unnecessary spaces may be inserted.
            "#},
            ' ',
        )
    )]
    minify: Minify,

    /// Do not resolve `cfg(..)`s
    #[arg(long)]
    no_resolve_cfgs: bool,

    /// Do not format the output before emitting
    #[arg(long)]
    no_rustfmt: bool,

    /// Do not check the output before emitting
    #[arg(long)]
    no_check: bool,

    /// Write to the file instead of STDOUT
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// [Deprecated] Alias for `--minify`
    #[arg(long, value_name = "MINIFY", default_value = "none")]
    oneline: Minify,

    /// [Deprecated] No-op
    #[arg(long, conflicts_with = "no_resolve_cfgs")]
    resolve_cfgs: bool,

    /// [Deprecated] No-op
    #[arg(long, conflicts_with = "no_rustfmt")]
    rustfmt: bool,

    /// [Deprecated] No-op
    #[arg(long, conflicts_with = "no_check")]
    check: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum User {
    Github(String),
    GitlabCom(String),
}

impl FromStr for User {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, &'static str> {
        return if let Some(username) = s.strip_prefix("github.com/") {
            Ok(Self::Github(username.to_owned()))
        } else if let Some(username) = s.strip_prefix("gitlab.com/") {
            Ok(Self::GitlabCom(username.to_owned()))
        } else {
            Err(MSG)
        };

        static MSG: &str = indoc! {r"
            Supported formats:
            * github.com/{username}
            * gitlab.com/{username}
        "};
    }
}

/// A `crate::$ident` path, stored as the bare `$ident`.
///
/// The name is kept as a `String` rather than a `syn::Ident` because clap requires
/// argument types to be `Clone + Send + Sync`, and `syn::Ident` is none of those.
#[derive(Clone, Debug, derive_more::Display)]
#[display("crate::{}", _0)]
pub struct CrateSinglePath(String);

impl CrateSinglePath {
    fn ident(&self) -> syn::Ident {
        syn::Ident::new(&self.0, proc_macro2::Span::call_site())
    }
}

impl FromStr for CrateSinglePath {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, &'static str> {
        (|| {
            let syn::Path {
                leading_colon,
                segments,
            } = syn::parse_str(s).map_err(|_| ())?;
            match (leading_colon, &*segments.into_iter().collect::<Vec<_>>()) {
                (None, [p1, p2]) if p1.ident == "crate" => Ok(Self(p2.ident.to_string())),
                _ => Err(()),
            }
        })()
        .map_err(|()| "expected `crate::$ident`")
    }
}

/// clap stores argument values as `String` here because `PkgSpec` is not `Clone`.
/// Parsing at this point still rejects a malformed spec with a proper CLI error.
fn validate_pkg_spec(spec: &str) -> Result<String, String> {
    spec.parse::<PkgSpec>().map_err(|e| e.to_string())?;
    Ok(spec.to_owned())
}

#[derive(Clone, Copy, PartialEq, Debug, ValueEnum)]
pub enum Remove {
    Docs,
    Comments,
}

#[derive(Clone, Copy, PartialEq, Debug, ValueEnum)]
pub enum Minify {
    None,
    Libs,
    All,
}

pub struct Context<'a> {
    pub cwd: PathBuf,
    pub cache_dir: PathBuf,
    pub shell: &'a mut Shell,
}

pub fn run(opt: Opt, ctx: Context<'_>) -> anyhow::Result<()> {
    let Opt::Equip(OptEquip {
        src,
        lib,
        bin,
        example,
        manifest_path,
        exclude,
        exclude_atcoder_crates,
        exclude_atcoder_202301_crates,
        exclude_atcoder_202510_crates,
        exclude_codingame_crates,
        mine,
        toolchain: deprecated_toolchain_opt,
        toolchain_for_udeps,
        toolchain_for_proc_macro_srv,
        mod_path,
        remove,
        minify,
        no_resolve_cfgs,
        no_rustfmt,
        no_check,
        output,
        oneline: deprecated_oneline_opt,
        resolve_cfgs: deprecated_resolve_cfgs_flag,
        rustfmt: deprecated_rustfmt_flag,
        check: deprecated_check_flag,
    }) = opt;

    let minify = match (minify, deprecated_oneline_opt) {
        (Minify::None, oneline) => oneline,
        (minify, _) => minify,
    };

    let exclude = {
        let mut exclude = exclude
            .iter()
            .map(|spec| spec.parse().expect("validated by `validate_pkg_spec`"))
            .collect::<Vec<PkgSpec>>();
        if exclude_atcoder_crates {
            exclude.extend(preset_crates::ATCODER.parse()?);
        }
        if exclude_atcoder_202301_crates {
            exclude.extend(preset_crates::ATCODER_202301.parse()?);
        }
        if exclude_atcoder_202510_crates {
            exclude.extend(preset_crates::ATCODER_202510.parse()?);
        }
        if exclude_codingame_crates {
            exclude.extend(preset_crates::CODINGAME.parse()?);
        }
        exclude
    };

    let toolchain_for_udeps = deprecated_toolchain_opt
        .as_ref()
        .unwrap_or(&toolchain_for_udeps);

    let cargo_equip_mod_name = &mod_path.ident();

    let Context {
        cwd,
        cache_dir,
        shell,
    } = ctx;

    if deprecated_toolchain_opt.is_some() {
        shell.warn("`--toolchain` was renamed to `--toolchain-for-udeps`")?;
    }
    if deprecated_resolve_cfgs_flag {
        shell.warn("`--resolve-cfgs` is deprecated. `#[cfg(..)]`s are resolved by default")?;
    }
    if deprecated_rustfmt_flag {
        shell.warn("`--rustfmt` is deprecated. the output is formatted by default")?;
    }
    if deprecated_check_flag {
        shell.warn("`--check` is deprecated. the output is checked by default")?;
    }

    let manifest_path = if let Some(manifest_path) = manifest_path {
        cwd.join(manifest_path.strip_prefix(".").unwrap_or(&manifest_path))
    } else {
        workspace::locate_project(&cwd)?
    };

    let metadata = workspace::cargo_metadata(&manifest_path, &cwd)?;

    let (root, root_package) = if lib {
        metadata.lib_target()
    } else if let Some(bin) = bin {
        metadata.bin_target_by_name(&bin)
    } else if let Some(example) = example {
        metadata.example_target_by_name(&example)
    } else if let Some(src) = src {
        metadata.target_by_src_path(&cwd.join(src))
    } else {
        metadata.exactly_one_target()
    }?;

    if root_package.edition() == Edition::Edition2015 {
        shell.warn("Rust 2015 is unsupported")?;
    }
    let resolve_behavior = workspace::resolve_behavior(root_package, &metadata.workspace_root)?;
    if resolve_behavior >= ResolveBehavior::V2 {
        shell.warn(
            "Currently cargo-equip only support Feature Resovler v1, and may go search more crates \
             than real Cargo does. Please watch https://github.com/qryxip/cargo-equip/issues/94",
        )?;
    }

    let libs_to_bundle = {
        let unused_deps = &if root.is_lib() {
            hashset!()
        } else {
            match cargo_udeps::cargo_udeps(root_package, root, toolchain_for_udeps, shell) {
                Ok(unused_deps) => unused_deps,
                Err(warning) => {
                    shell.warn(warning)?;
                    hashset!()
                }
            }
        };
        let mut libs_to_bundle =
            metadata.libs_to_bundle(&root_package.id, root.is_example(), unused_deps, &exclude)?;
        if root.is_lib() {
            libs_to_bundle.insert(&root_package.id, (root, root.crate_name()));
        }
        libs_to_bundle
    };

    let error_message = |head: &str| {
        let mut msg = head.to_owned();

        msg += "\n\n";
        msg += &libs_to_bundle
            .iter()
            .map(|(package_id, (_, pseudo_extern_crate_name))| {
                format!(
                    "- `{}` as `crate::{}::crates::{}`\n",
                    package_id, cargo_equip_mod_name, pseudo_extern_crate_name,
                )
            })
            .join("");

        let atcoder_specs = preset_crates::ATCODER.parse().unwrap_or_default();
        let crates_available_on_atcoder = iproduct!(libs_to_bundle.keys(), &atcoder_specs)
            .filter(|(id, spec)| spec.matches(&metadata[id]))
            .map(|(id, _)| format!("- `{}`\n", id))
            .join("");

        if !crates_available_on_atcoder.is_empty() {
            msg += &format!(
                "\nnote: attempted to bundle with the following crate(s), which are available on \
                 AtCoder. to exclude them from bundling, run with `--exclude-atcoder-crates`\n\n{}",
                crates_available_on_atcoder,
            );
        }

        msg
    };

    let code = bundle(
        &metadata,
        if root.is_lib() {
            RootCrate::Lib(root_package, root)
        } else {
            RootCrate::BinLike(root_package, root)
        },
        &libs_to_bundle,
        &mine,
        cargo_equip_mod_name,
        !no_resolve_cfgs,
        &remove,
        minify,
        !no_rustfmt,
        toolchain_for_proc_macro_srv.as_deref(),
        &cache_dir,
        shell,
    )
    .with_context(|| error_message("could not bundle the code"))?;

    if !no_check {
        workspace::cargo_check_using_current_lockfile_and_cache(
            &metadata,
            root_package,
            root,
            &exclude,
            &code,
        )
        .with_context(|| error_message("the bundled code was not valid"))?;
    }

    if let Some(output) = output {
        let output = cwd.join(output);
        cargo_util::paths::write(output, code)
    } else {
        write!(shell.out(), "{}", code)?;
        Ok(())
    }
}

/// One library after rewriting, split into the three module trees the output has:
/// `__cargo_equip::crates`, `::macros` and `::preludes`.
struct BundledLib<'a> {
    /// The name this library gets inside `crate::__cargo_equip::crates`.
    pseudo_extern_crate_name: &'a str,
    package: &'a cm::Package,
    crate_mod: String,
    macro_mod: String,
    prelude_mod: String,
}

/// The messages from one `cargo check --message-format json` run, shared between the
/// two things read out of them.
type CargoMessages = Rc<Vec<cm::Message>>;

#[allow(clippy::too_many_arguments)]
fn bundle(
    metadata: &cm::Metadata,
    root_crate: RootCrate<'_>,
    libs_to_bundle: &BTreeMap<&cm::PackageId, (&cm::Target, String)>,
    mine: &[User],
    cargo_equip_mod_name: &syn::Ident,
    resolve_cfgs: bool,
    remove: &[Remove],
    minify: Minify,
    rustfmt: bool,
    toolchain_for_proc_macro_srv: Option<&str>,
    cache_dir: &Path,
    shell: &mut Shell,
) -> anyhow::Result<String> {
    let active_toolchain = &*toolchain::active_toolchain(root_crate.package().manifest_dir())?;
    let toolchain_for_proc_macro_srv = toolchain_for_proc_macro_srv.unwrap_or(active_toolchain);

    let (cargo_messages_for_out_dirs, cargo_messages_for_proc_macro_dll_paths) = run_cargo_checks(
        metadata,
        root_crate,
        libs_to_bundle,
        active_toolchain,
        toolchain_for_proc_macro_srv,
        shell,
    )?;

    let out_dirs = workspace::list_out_dirs(metadata, &cargo_messages_for_out_dirs);

    // Borrows from `cargo_messages_for_proc_macro_dll_paths`, so it has to be built
    // here rather than inside `run_cargo_checks`.
    let macro_expander = cargo_messages_for_proc_macro_dll_paths
        .as_ref()
        .map(|messages| {
            let proc_macro_srv_exe = &toolchain::find_rust_analyzer_proc_macro_srv(
                root_crate.package().manifest_dir(),
                toolchain_for_proc_macro_srv,
            )?;

            let proc_macro_crate_dylibs = &ra_proc_macro::list_proc_macro_dylibs(messages, |p| {
                libs_to_bundle.contains_key(p)
            });

            ProcMacroExpander::spawn(proc_macro_srv_exe, proc_macro_crate_dylibs)
        })
        .transpose()?;

    let proc_macro_names = macro_expander
        .as_ref()
        .map(proc_macro_names_by_package)
        .unwrap_or_default();

    let resolve_nodes = metadata
        .resolve
        .as_ref()
        .map(|cm::Resolve { nodes, .. }| &nodes[..])
        .unwrap_or(&[])
        .iter()
        .map(|node| (&node.id, node))
        .collect::<HashMap<_, _>>();

    let mut code = if let Some((_, bin_target)) = root_crate.bin_like() {
        let code = cargo_util::paths::read(bin_target.src_path.as_ref())?;
        if rust::find_skip_attribute(&code)? {
            shell.status("Found", "`#![cfg_attr(cargo_equip, cargo_equip::skip)]`")?;
            return Ok(code);
        }
        code
    } else {
        "".to_owned()
    };

    shell.status("Bundling", "the code")?;

    if let Some((bin_package, bin_target)) = root_crate.bin_like() {
        code = rust::process_bin(
            cargo_equip_mod_name,
            &bin_target.src_path,
            { macro_expander }.as_mut(),
            |extern_crate_name| {
                metadata
                    .dep_lib_by_extern_crate_name(&bin_package.id, extern_crate_name)
                    .map(|_| extern_crate_name.to_owned())
            },
            |extern_crate_name| {
                matches!(
                    metadata.dep_lib_by_extern_crate_name(&bin_package.id, extern_crate_name),
                    Some(lib_package) if libs_to_bundle.contains_key(&lib_package.id)
                )
            },
            || (bin_target.crate_name(), &bin_package.id.repr),
        )?;
    }

    let libs = libs_to_bundle
        .iter()
        .map(|(pkg, (krate, pseudo_extern_crate_name))| {
            let mut edit = CodeEdit::new(cargo_equip_mod_name, &krate.src_path, || {
                (krate.crate_name(), &pkg.repr)
            })?;
            if let Some(out_dir) = out_dirs.get(pkg) {
                edit.expand_includes(out_dir)?;
            }
            Ok((*pkg, (*krate, &**pseudo_extern_crate_name, edit)))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;

    let (graph, indices) = normal_non_host_dep_graph(&resolve_nodes, libs_to_bundle);

    let libs_using_proc_macros = macro_name_to_dependent_crates(
        metadata,
        libs_to_bundle,
        &proc_macro_names,
        &graph,
        &indices,
    );

    let libs_with_local_inner_macros = propagate_local_inner_macros(&libs, &graph, &indices);

    let libs = libs
        .into_iter()
        .map(
            |(lib_package, (lib_target, pseudo_extern_crate_name, edit))| {
                let lib_package: &cm::Package = &metadata[lib_package];

                if let Some(names) = proc_macro_names.get(&lib_package.id) {
                    debug_assert_eq!(["proc-macro".to_owned()], *lib_target.kind);
                    return Ok(bundle_proc_macro_crate(
                        lib_package,
                        pseudo_extern_crate_name,
                        names,
                        cargo_equip_mod_name,
                    ));
                }

                bundle_lib(
                    lib_package,
                    pseudo_extern_crate_name,
                    edit,
                    metadata,
                    libs_to_bundle,
                    resolve_nodes[&lib_package.id],
                    &libs_with_local_inner_macros[&lib_package.id],
                    resolve_cfgs,
                    remove,
                    shell,
                )
            },
        )
        .collect::<anyhow::Result<Vec<BundledLib<'_>>>>()?;

    if !libs.is_empty() {
        if !root_crate.package().authors.is_empty() {
            shell.warn(
                "`package.authors` are no longer used to skip Copyright and License Notices",
            )?;
            shell.warn("instead, add `--mine github.com/{your username}` to the arguments")?;
        }

        code = rust::insert_prelude_for_main_crate(&code, cargo_equip_mod_name)?;

        code =
            rust::allow_unused_imports_for_seemingly_proc_macros(&code, |mod_name, item_name| {
                matches!(
                    libs_using_proc_macros.get(item_name), Some(pseudo_extern_crate_names)
                    if pseudo_extern_crate_names.contains(mod_name)
                )
            })?;

        let doc = &render_doc(
            &libs,
            metadata,
            cargo_equip_mod_name,
            mine,
            cache_dir,
            shell,
        )?;

        append_cargo_equip_mod(
            &mut code,
            &libs,
            &libs_with_local_inner_macros,
            root_crate,
            cargo_equip_mod_name,
            doc,
            minify,
        )?;
    }

    if minify == Minify::All {
        code = rustminify::minify_file(&rust::parse_file(&code)?);
    }

    if rustfmt {
        code = rustfmt::rustfmt(
            &metadata.workspace_root,
            &code,
            &root_crate.package().edition,
        )?;
    }

    Ok(code)
}

/// Rewrites one ordinary library into the modules it contributes to the output.
///
/// The `crate` module is the library's own code with paths translated; the `macros`
/// module carries its `#[macro_export]`ed macros; the `preludes` module reproduces
/// what the library's extern prelude and `#[macro_use]` imports used to provide.
#[allow(clippy::too_many_arguments)]
fn bundle_lib<'a>(
    lib_package: &'a cm::Package,
    pseudo_extern_crate_name: &'a str,
    mut edit: CodeEdit<'_>,
    metadata: &cm::Metadata,
    libs_to_bundle: &BTreeMap<&cm::PackageId, (&cm::Target, String)>,
    resolve_node: &cm::Node,
    libs_with_local_inner_macros: &BTreeSet<&str>,
    resolve_cfgs: bool,
    remove: &[Remove],
    shell: &mut Shell,
) -> anyhow::Result<BundledLib<'a>> {
    let translate_extern_crate_name = |dst: &_| -> _ {
        let dst_package = metadata.dep_lib_by_extern_crate_name(&lib_package.id, dst)?;
        let (_, dst_pseudo_extern_crate_name) =
            libs_to_bundle.get(&dst_package.id).unwrap_or_else(|| {
                panic!(
                    "missing `extern_crate_name` for `{}`. generated one should be given \
                     beforehead. this is a bug",
                    dst_package.id,
                );
            });
        Some(dst_pseudo_extern_crate_name.clone())
    };

    edit.translate_crate_path(pseudo_extern_crate_name)?;
    edit.translate_extern_crate_paths(translate_extern_crate_name)?;
    edit.process_extern_crates_in_lib(translate_extern_crate_name, shell)?;
    let macro_mod = edit.modify_declarative_macros(pseudo_extern_crate_name)?;
    let prelude_mod =
        edit.resolve_pseudo_prelude(pseudo_extern_crate_name, libs_with_local_inner_macros, &{
            metadata
                .libs_with_extern_crate_names(
                    &lib_package.id,
                    &libs_to_bundle.keys().copied().collect(),
                )?
                .into_iter()
                .map(|(package_id, extern_crate_name)| {
                    let (_, pseudo_extern_crate_name) =
                        libs_to_bundle.get(package_id).with_context(|| {
                            "could not translate pseudo extern crate names. this is a bug"
                        })?;
                    Ok((extern_crate_name, pseudo_extern_crate_name.clone()))
                })
                .collect::<anyhow::Result<_>>()?
        })?;
    if resolve_cfgs {
        edit.resolve_cfgs(&resolve_node.features)?;
    }
    if remove.contains(&Remove::Docs) {
        edit.allow_missing_docs();
        edit.erase_docs()?;
    }
    if remove.contains(&Remove::Comments) {
        edit.erase_comments()?;
    }

    Ok(BundledLib {
        pseudo_extern_crate_name,
        package: lib_package,
        crate_mod: edit.finish()?,
        macro_mod,
        prelude_mod,
    })
}

/// Leaves a stub behind for a `proc-macro` crate.
///
/// Its macros are expanded away at the call sites, so no code from the crate itself
/// reaches the output. What remains is a `macro_rules!` per macro that fails to
/// compile with a clear message, in case an invocation was somehow missed.
fn bundle_proc_macro_crate<'a>(
    lib_package: &'a cm::Package,
    pseudo_extern_crate_name: &'a str,
    macro_names: &BTreeSet<String>,
    cargo_equip_mod_name: &syn::Ident,
) -> BundledLib<'a> {
    let names = macro_names
        .iter()
        .map(|name| {
            let rename = format!(
                "{}_macro_def_{}_{}",
                cargo_equip_mod_name, pseudo_extern_crate_name, name,
            );
            (name, rename)
        })
        .collect::<Vec<_>>();

    let crate_mod = format!(
        "pub use crate::{}::macros::{}::*;{}",
        cargo_equip_mod_name,
        pseudo_extern_crate_name,
        names
            .iter()
            .map(|(name, rename)| {
                let msg = format!(
                    "`{}` from `{} {}` should have been expanded",
                    name, lib_package.name, lib_package.version,
                );
                format!(
                    "#[macro_export]macro_rules!{}{{($(_:tt)*)=>(::std::compile_error!({});)}}",
                    rename,
                    quote!(#msg),
                )
            })
            .join("")
    );

    let macro_mod = format!(
        "pub use crate::{}{}{};",
        if names.len() == 1 { " " } else { "{" },
        names
            .iter()
            .map(|(name, rename)| format!("{} as {}", rename, name))
            .format(","),
        if names.len() == 1 { "" } else { "}" },
    );

    BundledLib {
        pseudo_extern_crate_name,
        package: lib_package,
        crate_mod,
        macro_mod,
        prelude_mod: "".to_owned(),
    }
}

/// Runs `cargo check --message-format json` as many times as the situation needs.
///
/// Two things are read out of the messages: the build scripts' `OUT_DIR`s and the
/// proc-macro dylib paths. They can come from one run when both are needed and the
/// proc-macro server uses the active toolchain; otherwise each is run separately, or
/// skipped entirely when nothing needs it.
///
/// Returns `(messages for OUT_DIRs, messages for proc-macro dylibs)`.
fn run_cargo_checks(
    metadata: &cm::Metadata,
    root_crate: RootCrate<'_>,
    libs_to_bundle: &BTreeMap<&cm::PackageId, (&cm::Target, String)>,
    active_toolchain: &str,
    toolchain_for_proc_macro_srv: &str,
    shell: &mut Shell,
) -> anyhow::Result<(CargoMessages, Option<CargoMessages>)> {
    let run = |toolchain: &str, shell: &mut Shell| {
        let (package, krate) = root_crate.split();
        workspace::cargo_check_message_format_json(toolchain, metadata, package, krate, shell)
            .map(Rc::new)
    };

    let has_custom_build = libs_to_bundle
        .keys()
        .any(|p| metadata[p].has_custom_build());
    let has_proc_macro = libs_to_bundle.keys().any(|p| metadata[p].has_proc_macro());

    if has_custom_build && has_proc_macro && active_toolchain == toolchain_for_proc_macro_srv {
        let messages = run(active_toolchain, shell)?;
        return Ok((messages.clone(), Some(messages)));
    }

    let for_out_dirs = if has_custom_build {
        run(active_toolchain, shell)?
    } else {
        Default::default()
    };
    let for_proc_macro_dll_paths = has_proc_macro
        .then(|| run(toolchain_for_proc_macro_srv, shell))
        .transpose()?;

    Ok((for_out_dirs, for_proc_macro_dll_paths))
}

/// Groups the expandable macro names by the `proc-macro` package that defines them.
fn proc_macro_names_by_package<'msg>(
    macro_expander: &ProcMacroExpander<'msg>,
) -> HashMap<&'msg cm::PackageId, BTreeSet<String>> {
    let mut names = HashMap::<_, BTreeSet<_>>::new();
    for (pkg, macro_names) in macro_expander.macro_names() {
        for macro_name in macro_names {
            names.entry(pkg).or_default().insert(macro_name.to_owned());
        }
    }
    names
}

/// Appends the `mod __cargo_equip { .. }` tree — `crates`, `macros`, `prelude` and
/// `preludes` — to the already-rewritten main code.
fn append_cargo_equip_mod(
    code: &mut String,
    libs: &[BundledLib<'_>],
    libs_with_local_inner_macros: &HashMap<&cm::PackageId, BTreeSet<&str>>,
    root_crate: RootCrate<'_>,
    cargo_equip_mod_name: &syn::Ident,
    doc: &str,
    minify: Minify,
) -> anyhow::Result<()> {
    *code += "\n";
    *code += &match root_crate {
        RootCrate::BinLike(..) => {
            "// The following code was expanded by `cargo-equip`.\n".to_owned()
        }
        RootCrate::Lib(..) => format!("use {}::prelude::*;\n", cargo_equip_mod_name),
    };
    *code += "\n";

    let mods_of = |pick: for<'a> fn(&'a BundledLib<'_>) -> &'a str| {
        libs.iter()
            .map(|lib| (lib.pseudo_extern_crate_name, pick(lib)))
            .collect::<Vec<_>>()
    };
    let crate_mods = mods_of(|lib| &lib.crate_mod);
    let macro_mods = mods_of(|lib| &lib.macro_mod);
    let prelude_mods = mods_of(|lib| &lib.prelude_mod);

    let render_mods = |code: &mut String, mods: &[(&str, &str)]| -> anyhow::Result<()> {
        if minify == Minify::Libs {
            for (pseudo_extern_crate_name, mod_content) in mods {
                *code += "        pub mod ";
                *code += pseudo_extern_crate_name;
                *code += " {";
                *code += &rustminify::minify_file(&rust::parse_file(mod_content)?);
                *code += "}\n";
            }
        } else {
            for (i, (pseudo_extern_crate_name, mod_content)) in mods.iter().enumerate() {
                if i > 0 {
                    *code += "\n";
                }
                *code += "        pub mod ";
                *code += pseudo_extern_crate_name;
                *code += " {\n";
                *code += &rust::indent_code(mod_content, 3);
                *code += "    }\n";
            }
        }
        Ok(())
    };

    for doc in doc.lines() {
        *code += "///";
        if !code.is_empty() {
            *code += " ";
        }
        *code += doc;
        *code += "\n";
    }
    if minify == Minify::Libs {
        *code += "#[cfg_attr(any(), rustfmt::skip)]\n";
    }
    *code += "#[allow(unused)]\n";
    *code += &format!("mod {} {{\n", cargo_equip_mod_name);
    *code += "    pub(crate) mod crates {\n";
    render_mods(code, &crate_mods)?;
    *code += "    }\n";
    *code += "\n";
    *code += "    pub(crate) mod macros {\n";
    render_mods(code, &macro_mods)?;
    *code += "    }\n";
    *code += "\n";
    *code += "    pub(crate) mod prelude {";
    match root_crate {
        RootCrate::BinLike(..) => {
            let prelude_for_main = {
                let local_macro_uses_in_main_crate = libs_with_local_inner_macros
                    .values()
                    .flatten()
                    .unique()
                    .sorted()
                    .map(|name| format!("{}::*", name))
                    .collect::<Vec<_>>();

                let local_macro_uses_in_main_crate = match &*local_macro_uses_in_main_crate {
                    [] => None,
                    [part] => Some(part.clone()),
                    parts => Some(format!("{{{}}}", parts.iter().format(","))),
                };

                format!(
                    "pub use crate::{}::{};",
                    cargo_equip_mod_name,
                    if let Some(local_macro_uses_in_main_crate) = local_macro_uses_in_main_crate {
                        format!("{{crates::*,macros::{}}}", local_macro_uses_in_main_crate)
                    } else {
                        "crates::*".to_owned()
                    }
                )
            };
            *code += &if minify == Minify::Libs {
                prelude_for_main
            } else {
                format!("\n    {}\n    ", prelude_for_main)
            };
        }
        RootCrate::Lib(_, krate) => {
            *code += &format!("pub use crate::{}::crates::", cargo_equip_mod_name);
            *code += &krate.crate_name();
            *code += ";";
        }
    }
    *code += "}\n";
    *code += "\n";
    *code += "    mod preludes {\n";
    render_mods(code, &prelude_mods)?;
    *code += "    }\n";
    *code += "}\n";
    Ok(())
}

/// Renders the `# Bundled libraries` / `# Procedural macros` tables and the
/// `# License and Copyright Notices` section that go into the output's doc comment.
fn render_doc(
    libs: &[BundledLib<'_>],
    metadata: &cm::Metadata,
    cargo_equip_mod_name: &syn::Ident,
    mine: &[User],
    cache_dir: &Path,
    shell: &mut Shell,
) -> anyhow::Result<String> {
    fn list_packages<'a>(
        doc: &mut String,
        title: &str,
        cargo_equip_mod_name: &syn::Ident,
        contents: impl Iterator<Item = (Option<&'a str>, &'a cm::Package)>,
    ) {
        let mut table = Table::new();

        *table.get_format() = FormatBuilder::new()
            .column_separator(' ')
            .borders(' ')
            .build();

        let contents = contents.collect::<Vec<_>>();
        let any_from_local_filesystem = contents.iter().any(|(_, p)| p.source.is_none());

        for (pseudo_extern_crate_name, package) in contents {
            let mut row = row![format!("- `{}`", package.id.mask_path())];

            if any_from_local_filesystem {
                row.add_cell(if package.source.is_some() {
                    cell!("")
                } else if let Some(repository) = &package.repository {
                    cell!(format!("published in {}", repository))
                } else {
                    cell!("published in **missing**")
                });
            }

            row.add_cell(if let Some(license) = &package.license {
                cell!(format!("licensed under `{}`", license))
            } else {
                cell!("licensed under **missing**")
            });

            if let Some(pseudo_extern_crate_name) = pseudo_extern_crate_name {
                row.add_cell(cell!(format!(
                    "as `crate::{}::crates::{}`",
                    cargo_equip_mod_name, pseudo_extern_crate_name,
                )));
            }

            table.add_row(row);
        }

        if !table.is_empty() {
            if !doc.is_empty() {
                *doc += "\n";
            }
            *doc += &format!(" # {}\n\n", title);
            for line in table.to_string().lines() {
                *doc += line.trim_end();
                *doc += "\n";
            }
        }
    }

    let mut doc = "".to_owned();

    list_packages(
        &mut doc,
        "Bundled libraries",
        cargo_equip_mod_name,
        libs.iter()
            .filter(|lib| lib.package.has_lib())
            .map(|lib| (Some(lib.pseudo_extern_crate_name), lib.package)),
    );

    list_packages(
        &mut doc,
        "Procedural macros",
        cargo_equip_mod_name,
        libs.iter()
            .filter(|lib| lib.package.has_proc_macro())
            .map(|lib| (None, lib.package)),
    );

    let notices = libs
        .iter()
        .map(|lib| lib.package)
        .filter(|p| p.has_lib() && !metadata.workspace_members.contains(&p.id))
        .map(|lib_package| {
            shell.status("Checking", format!("the license of `{}`", lib_package.id))?;
            lib_package
                .read_license_text(mine, cache_dir)
                .map(|license_text| {
                    license_text.map(|license_text| (&lib_package.id, license_text))
                })
        })
        .flat_map(Result::transpose)
        .collect::<Result<Vec<_>, _>>()?;

    if !notices.is_empty() {
        doc += "\n # License and Copyright Notices\n";
        for (package_id, license_text) in notices {
            doc += &format!("\n - `{}`\n\n", package_id.mask_path());
            let backquotes = {
                let (mut n, mut m) = (2, None);
                for c in license_text.chars() {
                    if c == '`' {
                        m = Some(m.unwrap_or(0) + 1);
                    } else if let Some(m) = m.take() {
                        n = cmp::max(n, m);
                    }
                }
                "`".repeat(cmp::max(n, m.unwrap_or(0)) + 1)
            };
            doc += &format!("     {}text\n", backquotes);
            for line in license_text.lines() {
                match line {
                    "" => doc += "\n",
                    line => doc += &format!("     {}\n", line),
                }
            }
            doc += &format!("     {}\n", backquotes);
        }
    }

    Ok(doc)
}

/// For each expandable proc-macro name, the set of bundled crates that may invoke it.
///
/// A macro is reachable not only from the `proc-macro` crate that defines it but from
/// everything that depends on that crate, so the dependency graph is walked from each
/// `proc-macro` crate. Used to silence "unused import" warnings for names that look
/// like proc-macro imports in the output.
fn macro_name_to_dependent_crates<'cm>(
    metadata: &cm::Metadata,
    libs_to_bundle: &'cm BTreeMap<&'cm cm::PackageId, (&cm::Target, String)>,
    proc_macro_names: &'cm HashMap<&cm::PackageId, BTreeSet<String>>,
    graph: &Graph<&'cm cm::PackageId, ()>,
    indices: &HashMap<&'cm cm::PackageId, NodeIndex>,
) -> BTreeMap<&'cm str, HashSet<&'cm str>> {
    let mut acc = BTreeMap::<_, HashSet<_>>::new();
    for (pkg, names) in proc_macro_names {
        let (_, pseudo_extern_crate_name) = &libs_to_bundle[pkg];
        for name in names {
            acc.entry(&**name)
                .or_default()
                .insert(&**pseudo_extern_crate_name);
        }
    }
    for goal in libs_to_bundle.keys() {
        if metadata[goal].has_proc_macro() {
            let mut dfs = Dfs::new(graph, indices[goal]);
            while let Some(next) = dfs.next(graph) {
                let (_, pseudo_extern_crate_name) = &libs_to_bundle[graph[next]];
                for name in &proc_macro_names[goal] {
                    acc.entry(name)
                        .or_default()
                        .insert(pseudo_extern_crate_name);
                }
            }
        }
    }
    acc
}

/// For each bundled library, the set of crates whose `macro_use(local_inner_macros)`
/// macros have to be in scope there.
///
/// `#[macro_use(local_inner_macros)]` resolves macro calls inside a macro body against
/// the defining crate, so every crate that can reach such a definition needs the glob
/// import, not just the direct dependent.
fn propagate_local_inner_macros<'cm, 'name>(
    libs: &BTreeMap<&'cm cm::PackageId, (&cm::Target, &'name str, CodeEdit<'_>)>,
    graph: &Graph<&'cm cm::PackageId, ()>,
    indices: &HashMap<&'cm cm::PackageId, NodeIndex>,
) -> HashMap<&'cm cm::PackageId, BTreeSet<&'name str>> {
    let mut acc = libs
        .keys()
        .map(|pkg| (*pkg, btreeset!()))
        .collect::<HashMap<_, _>>();
    for (goal, (_, pseudo_extern_crate_name, edit)) in libs {
        if edit.has_local_inner_macros_attr() {
            acc.get_mut(*goal)
                .unwrap()
                .insert(*pseudo_extern_crate_name);
            let mut dfs = Dfs::new(graph, indices[goal]);
            while let Some(next) = dfs.next(graph) {
                acc.get_mut(graph[next])
                    .unwrap()
                    .insert(*pseudo_extern_crate_name);
            }
        }
    }
    acc
}

fn normal_non_host_dep_graph<'cm>(
    resolve_nodes: &HashMap<&'cm cm::PackageId, &cm::Node>,
    libs_to_bundle: &BTreeMap<&'cm cm::PackageId, (&cm::Target, String)>,
) -> (
    Graph<&'cm cm::PackageId, ()>,
    HashMap<&'cm cm::PackageId, NodeIndex>,
) {
    let mut graph = Graph::new();
    let mut indices = hashmap!();
    for pkg in libs_to_bundle.keys() {
        indices.insert(*pkg, graph.add_node(*pkg));
    }
    for (from_pkg, (from_crate, _)) in libs_to_bundle {
        if from_crate.is_lib() {
            for cm::NodeDep {
                pkg: to, dep_kinds, ..
            } in &resolve_nodes[from_pkg].deps
            {
                if *from_pkg != to
                    && dep_kinds
                        .iter()
                        .any(|cm::DepKindInfo { kind, .. }| *kind == cm::DependencyKind::Normal)
                    && libs_to_bundle.contains_key(to)
                {
                    graph.add_edge(indices[to], indices[*from_pkg], ());
                }
            }
        }
    }
    (graph, indices)
}

#[derive(Clone, Copy)]
enum RootCrate<'cm> {
    BinLike(&'cm cm::Package, &'cm cm::Target),
    Lib(&'cm cm::Package, &'cm cm::Target),
}

impl<'cm> RootCrate<'cm> {
    fn split(self) -> (&'cm cm::Package, &'cm cm::Target) {
        match self {
            RootCrate::BinLike(p, t) | RootCrate::Lib(p, t) => (p, t),
        }
    }

    fn package(self) -> &'cm cm::Package {
        match self {
            RootCrate::BinLike(p, _) | RootCrate::Lib(p, _) => p,
        }
    }

    fn bin_like(self) -> Option<(&'cm cm::Package, &'cm cm::Target)> {
        match self {
            RootCrate::BinLike(p, t) => Some((p, t)),
            RootCrate::Lib(..) => None,
        }
    }
}
