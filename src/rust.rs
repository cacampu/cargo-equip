use crate::ra_proc_macro::ProcMacroExpander;
use anyhow::{anyhow, bail, Context as _};
use camino::Utf8Path;
use fixedbitset::FixedBitSet;
use if_chain::if_chain;
use itertools::Itertools as _;
use maplit::btreemap;
use proc_macro2::{LineColumn, Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::{collections::BTreeMap, mem, str};
use syn::{
    parse_quote, punctuated::Punctuated, spanned::Spanned, visit::Visit, Expr, ExprLit, Ident,
    Item, ItemMacro, ItemMod, ItemUse, Lit, Meta, MetaList, MetaNameValue, Token, UseGroup,
    UsePath, UseTree,
};

mod expand;
mod strip;
mod translate;

/// The comma-separated `Meta`s inside `#[name(..)]`.
///
/// syn 1 handed these over as `MetaList::nested`. syn 2 keeps the contents of an
/// attribute list as raw tokens, so anything wanting to look inside has to parse
/// them. An unparseable list yields nothing rather than an error: these are
/// best-effort inspections of other people's attributes.
pub(crate) fn nested_metas(list: &MetaList) -> Punctuated<Meta, Token![,]> {
    list.parse_args_with(Punctuated::parse_terminated)
        .unwrap_or_default()
}

pub(crate) fn find_skip_attribute(code: &str) -> anyhow::Result<bool> {
    let syn::File { attrs, .. } = syn::parse_file(code)
        .map_err(|e| anyhow!("{:?}", e))
        .with_context(|| "could not parse the code")?;

    Ok(attrs
        .iter()
        .map(|attr| &attr.meta)
        .flat_map(|meta| match meta {
            Meta::List(meta_list) => Some(meta_list),
            _ => None,
        })
        .filter(|MetaList { path, .. }| path.is_ident("cfg_attr"))
        .any(|meta_list| {
            matches!(
                *nested_metas(meta_list).iter().collect::<Vec<_>>(),
                [pred, attr]
                if matches!(
                    cfg_expr::Expression::parse(&pred.to_token_stream().to_string()),
                    Ok(expr)
                    if expr.eval(|pred| match pred {
                        cfg_expr::Predicate::Flag("cargo_equip") => Some(true),
                        _ => None,
                    }) == Some(true)
                ) && *attr == parse_quote!(cargo_equip::skip)
            )
        }))
}

fn has_multi_line_literal(token_stream: TokenStream) -> bool {
    token_stream.into_iter().any(|tt| match tt {
        TokenTree::Group(group) => has_multi_line_literal(group.stream()),
        TokenTree::Literal(lit) => lit.span().start().line != lit.span().end().line,
        _ => false,
    })
}

pub(crate) fn indent_code(code: &str, n: usize) -> String {
    // Indenting a line that starts inside a multi-line literal would change the
    // literal's value, so such code is left as it is. The literal can be at any
    // depth: a string inside a function body sits in the body's brace group.
    let is_safe_to_indent = code
        .parse::<TokenStream>()
        .is_ok_and(|token_stream| !has_multi_line_literal(token_stream));

    if is_safe_to_indent {
        code.lines()
            .map(|line| match line {
                "" => "\n".to_owned(),
                line => format!("{}{}\n", "    ".repeat(n), line),
            })
            .join("")
    } else {
        code.to_owned()
    }
}

fn replace_ranges(code: &str, replacements: BTreeMap<(LineColumn, LineColumn), String>) -> String {
    if replacements.is_empty() {
        return code.to_owned();
    }
    let replacements = replacements.into_iter().collect::<Vec<_>>();
    let mut replacements = &*replacements;
    let mut skip_until = None;
    let mut ret = "".to_owned();
    let mut lines = code.trim_end().split('\n').enumerate().peekable();
    while let Some((i, s)) = lines.next() {
        for (j, c) in s.chars().enumerate() {
            if_chain! {
                if let Some(((start, end), replacement)) = replacements.first();
                if (i, j) == (start.line - 1, start.column);
                then {
                    ret += replacement;
                    if start == end {
                        ret.push(c);
                    } else {
                        skip_until = Some(*end);
                    }
                    replacements = &replacements[1..];
                } else {
                    if !matches!(skip_until, Some(LineColumn { line, column }) if (i, j) < (line - 1, column)) {
                        ret.push(c);
                        skip_until = None;
                    }
                }
            }
        }
        while let Some(((start, end), replacement)) = replacements.first() {
            if i == start.line - 1 {
                ret += replacement;
                if start < end {
                    skip_until = Some(*end);
                }
                replacements = &replacements[1..];
            } else {
                break;
            }
        }
        if lines.peek().is_some() || code.ends_with('\n') {
            ret += "\n";
        }
    }

    debug_assert!(syn::parse_file(code).is_ok());

    ret
}

pub(crate) fn insert_prelude_for_main_crate(
    code: &str,
    cargo_equip_mod_name: &Ident,
) -> syn::Result<String> {
    let file = &syn::parse_file(code)?;
    let mut replacements = btreemap!();
    Visitor {
        replacements: &mut replacements,
        cargo_equip_mod_name,
    }
    .visit_file(file);
    return Ok(replace_ranges(code, replacements));

    struct Visitor<'a> {
        replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
        cargo_equip_mod_name: &'a Ident,
    }

    impl Visitor<'_> {
        fn visit_items(&mut self, items: &[Item], crate_root: bool) {
            if let Some(first) = items.first() {
                let pos = first.span().start();
                self.replacements.insert(
                    (pos, pos),
                    format!(
                        "pub use {}{}::prelude::*;\n\n",
                        if crate_root { "" } else { "crate::" },
                        self.cargo_equip_mod_name,
                    ),
                );
            }
            for item in items {
                if let Item::Mod(item) = item {
                    self.visit_item_mod(item);
                }
            }
        }
    }

    impl Visit<'_> for Visitor<'_> {
        fn visit_file(&mut self, i: &syn::File) {
            self.visit_items(&i.items, true);
        }

        fn visit_item_mod(&mut self, i: &ItemMod) {
            if let Some((_, items)) = &i.content {
                self.visit_items(items, false);
            }
        }
    }
}

pub(crate) fn allow_unused_imports_for_seemingly_proc_macros(
    code: &str,
    mut seemingly_proc_macro: impl FnMut(&str, &str) -> bool,
) -> syn::Result<String> {
    let file = &syn::parse_file(code)?;
    let mut replacements = btreemap!();
    Visitor {
        replacements: &mut replacements,
        seemingly_proc_macro: &mut seemingly_proc_macro,
    }
    .visit_file(file);
    return Ok(if replacements.is_empty() {
        code.to_owned()
    } else {
        replace_ranges(code, replacements)
    });

    struct Visitor<'a, P> {
        replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
        seemingly_proc_macro: P,
    }

    impl<P: FnMut(&str, &str) -> bool> Visit<'_> for Visitor<'_, P> {
        fn visit_item_use(&mut self, i: &ItemUse) {
            if let UseTree::Path(UsePath { ident, tree, .. }) = &i.tree {
                match &**tree {
                    UseTree::Name(name)
                        if (self.seemingly_proc_macro)(
                            &ident.to_string(),
                            &name.ident.to_string(),
                        ) =>
                    {
                        self.replacements.insert(
                            (i.span().start(), i.span().start()),
                            "#[allow(unused_imports)]\n".to_owned(),
                        );
                    }
                    UseTree::Group(UseGroup { items, .. }) => {
                        for pair in items.pairs() {
                            let item = pair.value();
                            if let UseTree::Name(name) = item {
                                if (self.seemingly_proc_macro)(
                                    &ident.to_string(),
                                    &name.ident.to_string(),
                                ) {
                                    self.replacements.insert(
                                        (pair.span().start(), pair.span().start()),
                                        "/*".to_owned(),
                                    );
                                    self.replacements.insert(
                                        (pair.span().end(), pair.span().end()),
                                        "*/".to_owned(),
                                    );
                                    self.replacements.insert(
                                        (i.span().end(), i.span().end()),
                                        format!(
                                            "\n#[allow(unused_imports)]\n{}use {}::{};",
                                            (i.vis.to_token_stream().to_string() + " ").trim(),
                                            ident,
                                            name.ident,
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn set_span(mask: &mut [FixedBitSet], span: Span, p: bool) {
    let i1 = span.start().line - 1;
    if span.start().line == span.end().line {
        let l = span.start().column;
        let r = span.end().column;
        mask[i1].set_range(l..r, p);
    } else {
        let i2 = span.end().line - 1;
        let l = span.start().column;
        mask[i1].set_range(l.., p);
        for mask in &mut mask[i1 + 1..i2] {
            mask.set_range(.., p);
        }
        let r = span.end().column;
        mask[i2].set_range(..r, p);
    }
}

pub(crate) fn parse_file(code: &str) -> anyhow::Result<syn::File> {
    syn::parse_file(code)
        .map_err(|e| anyhow!("{}", e))
        .with_context(|| "broke the code during modification")
}

pub(crate) fn process_bin<'cm>(
    cargo_equip_mod_name: &Ident,
    src_path: &Utf8Path,
    proc_macro_expander: Option<&mut ProcMacroExpander<'_>>,
    translate_extern_crate_name: impl FnMut(&str) -> Option<String>,
    is_lib_to_bundle: impl FnMut(&str) -> bool,
    context: impl FnOnce() -> (String, &'cm str),
) -> anyhow::Result<String> {
    let mut edit = CodeEdit::new(cargo_equip_mod_name, src_path, context)?;
    if let Some(proc_macro_expander) = proc_macro_expander {
        edit.expand_proc_macros(proc_macro_expander)?;
    }
    edit.translate_extern_crate_paths(translate_extern_crate_name)?;
    edit.process_extern_crate_in_bin(is_lib_to_bundle)?;
    edit.finish()
}

pub(crate) struct CodeEdit<'opt> {
    cargo_equip_mod_name: &'opt Ident,
    has_local_inner_macros_attr: bool,
    string: String,
    file: syn::File,
    replacements: BTreeMap<(LineColumn, LineColumn), String>,
}

impl<'opt> CodeEdit<'opt> {
    pub(crate) fn new<'cm>(
        cargo_equip_mod_name: &'opt Ident,
        src_path: &Utf8Path,
        err_context: impl FnOnce() -> (String, &'cm str),
    ) -> anyhow::Result<Self> {
        return (|| {
            Self::from_code(cargo_equip_mod_name, &expand_mods(src_path, 0)?)
                .map_err(anyhow::Error::from)
        })()
        .with_context(|| {
            let (crate_name, package_id) = err_context();
            format!("could not expand `{}` from `{}`", crate_name, package_id)
        });

        fn expand_mods(src_path: &Utf8Path, depth: usize) -> anyhow::Result<String> {
            let content = cargo_util::paths::read(src_path.as_ref())?;

            let syn::File { items, .. } = syn::parse_file(&content)
                .map_err(|e| anyhow!("{:?}", e))
                .with_context(|| format!("could not parse `{}`", src_path))?;

            let replacements = items
                .into_iter()
                .flat_map(|item| match item {
                    Item::Mod(ItemMod {
                        attrs,
                        ident,
                        content: None,
                        semi,
                        ..
                    }) => Some((attrs, ident, semi)),
                    _ => None,
                })
                .map(|(attrs, ident, semi)| {
                    let paths = if let Some(path) = attrs
                        .iter()
                        .map(|attr| &attr.meta)
                        .flat_map(|meta| match meta {
                            Meta::NameValue(name_value) => Some(name_value),
                            _ => None,
                        })
                        .filter(|MetaNameValue { path, .. }| {
                            matches!(path.get_ident(), Some(i) if i == "path")
                        })
                        .find_map(|MetaNameValue { value, .. }| match value {
                            Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => Some(s.value()),
                            _ => None,
                        }) {
                        vec![src_path.with_file_name("").join(path)]
                    } else if depth == 0 || src_path.file_name() == Some("mod.rs") {
                        vec![
                            src_path
                                .with_file_name(ident.to_string())
                                .with_extension("rs"),
                            src_path.with_file_name(ident.to_string()).join("mod.rs"),
                        ]
                    } else {
                        vec![
                            src_path
                                .with_extension("")
                                .with_file_name(ident.to_string())
                                .with_extension("rs"),
                            src_path
                                .with_extension("")
                                .with_file_name(ident.to_string())
                                .join("mod.rs"),
                        ]
                    };

                    if let Some(path) = paths.iter().find(|p| p.exists()) {
                        let start = semi.span().start();
                        let end = semi.span().end();
                        let content = expand_mods(path, depth + 1)?;
                        let content = indent_code(&content, depth + 1);
                        let content = format!(" {{\n{}{}}}", content, "    ".repeat(depth + 1));
                        Ok(((start, end), content))
                    } else {
                        bail!("one of {:?} does not exist", paths);
                    }
                })
                .collect::<anyhow::Result<_>>()?;

            Ok(replace_ranges(&content, replacements))
        }
    }

    fn from_code(cargo_equip_mod_name: &'opt Ident, string: &str) -> syn::Result<Self> {
        let file = syn::parse_file(string)?;
        return Ok(Self {
            cargo_equip_mod_name,
            has_local_inner_macros_attr: check_local_inner_macros(&file),
            string: string.to_owned(),
            file,
            replacements: btreemap!(),
        });

        fn check_local_inner_macros(file: &syn::File) -> bool {
            let mut out = false;
            Visitor { out: &mut out }.visit_file(file);
            return out;

            struct Visitor<'a> {
                out: &'a mut bool,
            }

            impl Visit<'_> for Visitor<'_> {
                fn visit_item_macro(&mut self, i: &ItemMacro) {
                    *self.out |= i
                        .attrs
                        .iter()
                        .map(|attr| &attr.meta)
                        .flat_map(|meta| match meta {
                            Meta::List(meta_list) => Some(meta_list),
                            _ => None,
                        })
                        .any(|meta_list| {
                            meta_list.path.is_ident("macro_export")
                                && nested_metas(meta_list).iter().any(|meta| {
                                    matches!(
                                        meta,
                                        Meta::Path(path)
                                        if path.is_ident("local_inner_macros")
                                    )
                                })
                        });
                }
            }
        }
    }

    pub(crate) fn has_local_inner_macros_attr(&self) -> bool {
        self.has_local_inner_macros_attr
    }

    pub(crate) fn finish(mut self) -> anyhow::Result<String> {
        self.apply()?;
        Ok(self.string)
    }

    fn apply(&mut self) -> anyhow::Result<()> {
        if !self.replacements.is_empty() {
            self.force_apply()?;
        }
        Ok(())
    }

    fn force_apply(&mut self) -> anyhow::Result<()> {
        self.string = replace_ranges(&self.string, mem::take(&mut self.replacements));
        self.file =
            syn::parse_file(&self.string).with_context(|| "broke the code during modification")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::indent_code;
    use pretty_assertions::assert_eq;

    /// The check for multi-line literals only looked at the top level of the token
    /// stream, so a string inside a function body got indented and changed value.
    #[test]
    fn indent_code_leaves_multi_line_literals_alone() {
        assert_eq!("    fn f() {}\n", indent_code("fn f() {}\n", 1));

        let code = "fn f() -> &'static str {\n    \"a\nb\"\n}\n";
        assert_eq!(code, indent_code(code, 1));
    }
}
