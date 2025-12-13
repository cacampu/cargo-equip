use anyhow::{anyhow, Context as _};
use cargo_metadata as cm;
use itertools::chain;
use maplit::btreemap;
use ra_ap_paths::AbsPath;
use ra_ap_proc_macro_api::{MacroDylib, ProcMacro, ProcMacroClient, ProcMacroKind};
use ra_ap_span as span;
use ra_ap_tt::iter::TtElement;
use ra_ap_tt::{self as tt, DelimiterKind};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MSRV: Version = Version::new(1, 64, 0);

pub(crate) fn list_proc_macro_dylibs<P: FnMut(&cm::PackageId) -> bool>(
    cargo_messages: &[cm::Message],
    mut filter: P,
) -> BTreeMap<&cm::PackageId, &AbsPath> {
    cargo_messages
        .iter()
        .flat_map(|message| match message {
            cm::Message::CompilerArtifact(artifact) => Some(artifact),
            _ => None,
        })
        .filter(|cm::Artifact { target, .. }| *target.kind == ["proc-macro".to_owned()])
        .filter(|cm::Artifact { package_id, .. }| filter(package_id))
        .flat_map(
            |cm::Artifact {
                 package_id,
                 filenames,
                 ..
             }| {
                filenames
                    .get(0)
                    .map(|filename| (package_id, AbsPath::assert(filename.as_ref())))
            },
        )
        .collect()
}

pub struct ProcMacroExpander<'msg> {
    custom_derive: BTreeMap<String, (&'msg cm::PackageId, ProcMacro)>,
    func_like: BTreeMap<String, (&'msg cm::PackageId, ProcMacro)>,
    attr: BTreeMap<String, (&'msg cm::PackageId, ProcMacro)>,
}

impl<'msg> ProcMacroExpander<'msg> {
    pub(crate) fn spawn(
        proc_macro_srv_exe: &AbsPath,
        dylib_paths: &BTreeMap<&'msg cm::PackageId, &'msg AbsPath>,
    ) -> anyhow::Result<Self> {
        let server = ProcMacroClient::spawn(
            proc_macro_srv_exe,
            std::iter::empty::<(&std::ffi::OsStr, &Option<&std::ffi::OsStr>)>(),
            None::<&Version>,
        )
        .map_err(|e| anyhow!("{}", e))
        .with_context(|| "rust-analyzer error")?;

        let mut custom_derive = btreemap!();
        let mut func_like = btreemap!();
        let mut attr = btreemap!();

        for (&package_id, dylib_path) in dylib_paths {
            let proc_macros = server
                .load_dylib(MacroDylib::new((*dylib_path).to_owned()))
                .map_err(|e| anyhow!("{}", e))
                .with_context(|| "rust-analyzer error")?;

            for proc_macro in proc_macros {
                match proc_macro.kind() {
                    ProcMacroKind::CustomDerive => &mut custom_derive,
                    ProcMacroKind::Bang => &mut func_like,
                    ProcMacroKind::Attr => &mut attr,
                }
                .insert(proc_macro.name().to_owned(), (package_id, proc_macro));
            }
        }

        Ok(Self {
            custom_derive,
            func_like,
            attr,
        })
    }

    pub(crate) fn macro_names(
        &self,
    ) -> impl Iterator<Item = (&'msg cm::PackageId, BTreeSet<&str>)> {
        let mut names = BTreeMap::<_, BTreeSet<_>>::new();
        for (name, &(pkg, _)) in chain!(&self.custom_derive, &self.func_like, &self.attr) {
            names.entry(pkg).or_default().insert(&**name);
        }
        names.into_iter()
    }

    pub(crate) fn attempt_expand_custom_derive(
        &mut self,
        name: &str,
        body: impl FnOnce() -> proc_macro2::TokenStream,
    ) -> anyhow::Result<Option<proc_macro2::Group>> {
        self.attempt_expand(name, ProcMacroKind::CustomDerive, body, None::<fn() -> _>)
    }

    pub(crate) fn attempt_expand_func_like(
        &mut self,
        name: &str,
        body: impl FnOnce() -> proc_macro2::TokenStream,
    ) -> anyhow::Result<Option<proc_macro2::Group>> {
        self.attempt_expand(name, ProcMacroKind::Bang, body, None::<fn() -> _>)
    }

    pub(crate) fn attempt_expand_attr(
        &mut self,
        name: &str,
        body: impl FnOnce() -> proc_macro2::TokenStream,
        attr: impl FnOnce() -> proc_macro2::Group,
    ) -> anyhow::Result<Option<proc_macro2::Group>> {
        self.attempt_expand(name, ProcMacroKind::Attr, body, Some(attr))
    }

    fn attempt_expand(
        &self,
        name: &str,
        kind: ProcMacroKind,
        subtree: impl FnOnce() -> proc_macro2::TokenStream,
        attr: Option<impl FnOnce() -> proc_macro2::Group>,
    ) -> anyhow::Result<Option<proc_macro2::Group>> {
        match kind {
            ProcMacroKind::CustomDerive => &self.custom_derive,
            ProcMacroKind::Bang => &self.func_like,
            ProcMacroKind::Attr => &self.attr,
        }
        .get(name)
        .map(|(_, proc_macro)| {
            // Build input TopSubtree<Span> from proc_macro2 tokens
            let input_top = from_proc_macro2_group(&proc_macro2::Group::new(
                proc_macro2::Delimiter::None,
                subtree(),
            ));

            let attr_top = attr.map(|f| from_proc_macro2_group(&f()));
            let attr_view = attr_top.as_ref().map(|t| t.view());

            // create simple dummy spans for def_site / call_site / mixed_site
            let dummy_span = make_dummy_span();

            let expand_res = proc_macro
                .expand(
                    input_top.view(),
                    attr_view,
                    vec![],
                    dummy_span,
                    dummy_span,
                    dummy_span,
                    String::new(),
                )
                .map_err(|e| anyhow!("{}", e))
                .with_context(|| "rust-analyzer error")?;

            let output = expand_res.map_err(|s| anyhow!("proc macro paniced: {s:?}"))?;

            Ok(from_ra_top_subtree(&output))
        })
        .transpose()
    }
}

fn make_dummy_span() -> span::Span {
    use ra_ap_span::{Edition, FileId, TextRange, TextSize};
    let anchor = span::SpanAnchor {
        file_id: span::EditionedFileId::current_edition(FileId::from_raw(0)),
        ast_id: span::ROOT_ERASED_FILE_AST_ID,
    };
    span::Span {
        range: TextRange::empty(TextSize::new(0)),
        anchor,
        ctx: span::SyntaxContext::root(Edition::CURRENT),
    }
}

fn from_proc_macro2_group(group: &proc_macro2::Group) -> tt::TopSubtree<span::Span> {
    let span = make_dummy_span();
    let mut builder = tt::TopSubtreeBuilder::new(tt::Delimiter::invisible_spanned(span));

    fn process_stream(
        builder: &mut tt::TopSubtreeBuilder<span::Span>,
        stream: proc_macro2::TokenStream,
        span: span::Span,
    ) {
        for tt in stream.into_iter() {
            process_token_tree(builder, &tt, span);
        }
    }

    fn process_token_tree(
        builder: &mut tt::TopSubtreeBuilder<span::Span>,
        tt: &proc_macro2::TokenTree,
        span: span::Span,
    ) {
        match tt {
            proc_macro2::TokenTree::Group(g) => {
                let kind = match g.delimiter() {
                    proc_macro2::Delimiter::Parenthesis => tt::DelimiterKind::Parenthesis,
                    proc_macro2::Delimiter::Brace => tt::DelimiterKind::Brace,
                    proc_macro2::Delimiter::Bracket => tt::DelimiterKind::Bracket,
                    proc_macro2::Delimiter::None => tt::DelimiterKind::Invisible,
                };
                builder.open(kind, span);
                process_stream(builder, g.stream(), span);
                builder.close(span);
                builder.remove_last_subtree_if_invisible();
            }
            proc_macro2::TokenTree::Ident(i) => {
                let text = i.to_string();
                let ident = tt::Ident::new(&text, span);
                builder.push(tt::Leaf::Ident(ident));
            }
            proc_macro2::TokenTree::Punct(p) => {
                let punct = tt::Punct {
                    char: p.as_char(),
                    spacing: match p.spacing() {
                        proc_macro2::Spacing::Alone => tt::Spacing::Alone,
                        proc_macro2::Spacing::Joint => tt::Spacing::Joint,
                    },
                    span,
                };
                builder.push(tt::Leaf::Punct(punct));
            }
            proc_macro2::TokenTree::Literal(l) => {
                let lit = tt::token_to_literal(&l.to_string(), span);
                builder.push(tt::Leaf::Literal(lit));
            }
        }
    }

    process_stream(&mut builder, group.stream(), span);
    builder.build()
}

fn from_ra_top_subtree(subtree: &tt::TopSubtree<impl Copy>) -> proc_macro2::Group {
    fn delim_to_proc(d: tt::Delimiter<impl Copy>) -> proc_macro2::Delimiter {
        match d.kind {
            DelimiterKind::Parenthesis => proc_macro2::Delimiter::Parenthesis,
            DelimiterKind::Brace => proc_macro2::Delimiter::Brace,
            DelimiterKind::Bracket => proc_macro2::Delimiter::Bracket,
            DelimiterKind::Invisible => proc_macro2::Delimiter::None,
        }
    }

    fn tt_element_to_token_tree<S: Copy>(el: TtElement<'_, S>) -> proc_macro2::TokenTree {
        match el {
            TtElement::Subtree(sub, iter) => {
                let mut ts = proc_macro2::TokenStream::new();
                for child in iter {
                    ts.extend(std::iter::once(tt_element_to_token_tree(child)));
                }
                proc_macro2::TokenTree::Group(proc_macro2::Group::new(
                    delim_to_proc(sub.delimiter),
                    ts,
                ))
            }
            TtElement::Leaf(leaf) => match leaf {
                tt::Leaf::Ident(i) => {
                    let mut name = i.sym.to_string();
                    if i.is_raw.yes() {
                        name = format!("r#{}", name);
                    }
                    proc_macro2::Ident::new(&name, proc_macro2::Span::call_site()).into()
                }
                tt::Leaf::Punct(p) => {
                    let spacing = match p.spacing {
                        tt::Spacing::Alone => proc_macro2::Spacing::Alone,
                        tt::Spacing::Joint | tt::Spacing::JointHidden => {
                            proc_macro2::Spacing::Joint
                        }
                    };
                    proc_macro2::Punct::new(p.char, spacing).into()
                }
                tt::Leaf::Literal(l) => {
                    let sym = l.symbol.to_string();
                    let suff = l.suffix.as_ref().map(|s| s.to_string()).unwrap_or_default();
                    match l.kind {
                        tt::LitKind::Str => proc_macro2::Literal::string(&sym).into(),
                        tt::LitKind::StrRaw(n) => {
                            let hashes = "#".repeat(n as usize);
                            let s = format!("r{hashes}\"{sym}\"{hashes}");
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse raw string literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::ByteStr => {
                            let s = format!("b\"{}\"{}", sym, suff);
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse byte string literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::ByteStrRaw(n) => {
                            let hashes = "#".repeat(n as usize);
                            let s = format!("br{hashes}\"{sym}\"{hashes}");
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse raw byte string literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::CStr => {
                            let s = format!("c\"{}\"{}", sym, suff);
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse cstr literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::CStrRaw(n) => {
                            let hashes = "#".repeat(n as usize);
                            let s = format!("cr{hashes}\"{sym}\"{hashes}");
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse raw cstr literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::Char => {
                            let s = format!("'{}'{}", sym, suff);
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse char literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::Byte => {
                            let s = format!("b'{}'{}", sym, suff);
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse byte literal {}: {}", s, e)
                            })
                        }
                        tt::LitKind::Integer | tt::LitKind::Float | tt::LitKind::Err(()) => {
                            let s = format!("{}{}", sym, suff);
                            syn::parse_str(&s).unwrap_or_else(|e| {
                                panic!("could not parse numeric literal {}: {}", s, e)
                            })
                        }
                    }
                }
            },
        }
    }

    let mut ts = proc_macro2::TokenStream::new();
    for tt in subtree.token_trees().iter() {
        ts.extend(std::iter::once(tt_element_to_token_tree(tt)));
    }
    proc_macro2::Group::new(delim_to_proc(subtree.top_subtree().delimiter), ts)
}
