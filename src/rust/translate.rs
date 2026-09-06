//! Rewriting paths so that a library keeps working after being moved into
//! `crate::__cargo_equip::crates::<name>`.
//!
//! This covers `extern crate` items, `crate::`-rooted paths, `$crate` inside
//! `macro_rules!`, and the glob imports that stand in for the extern prelude and
//! `#[macro_use]`.

use super::CodeEdit;
use crate::shell::Shell;
use itertools::Itertools as _;
use maplit::btreemap;
use proc_macro2::{LineColumn, Span, TokenStream};
use quote::{quote, ToTokens};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    str,
};
use syn::{
    spanned::Spanned, visit::Visit, Attribute, Ident, Item, ItemExternCrate, ItemMacro, ItemMod,
    ItemUse, Macro, PathSegment, UseGroup, UseName, UsePath, UseRename, UseTree, VisRestricted,
};

impl<'opt> CodeEdit<'opt> {
    pub(super) fn process_extern_crate_in_bin(
        &mut self,
        is_lib_to_bundle: impl FnMut(&str) -> bool,
    ) -> anyhow::Result<()> {
        self.apply()?;
        Visitor {
            replacements: &mut self.replacements,
            cargo_equip_mod_name: self.cargo_equip_mod_name,
            is_lib_to_bundle,
        }
        .visit_file(&self.file);
        return Ok(());

        struct Visitor<'a, F> {
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
            cargo_equip_mod_name: &'a Ident,
            is_lib_to_bundle: F,
        }

        impl<F: FnMut(&str) -> bool> Visit<'_> for Visitor<'_, F> {
            fn visit_item_extern_crate(&mut self, item_use: &ItemExternCrate) {
                let ItemExternCrate {
                    attrs,
                    vis,
                    ident,
                    rename,
                    ..
                } = item_use;

                if (self.is_lib_to_bundle)(&ident.to_string()) {
                    let is_macro_use = attrs
                        .iter()
                        .flat_map(Attribute::parse_meta)
                        .any(|m| m.path().is_ident("macro_use"));
                    let vis = vis.to_token_stream();

                    let mut insertion = "".to_owned();

                    if let Some((_, rename)) = rename {
                        if rename != "_" {
                            insertion = format!(
                                "{} use crate::{}::crates::{} as {};",
                                vis, self.cargo_equip_mod_name, ident, rename
                            );
                        }
                    } else {
                        insertion = format!(
                            "{} use crate::{}::crates::{};",
                            vis, self.cargo_equip_mod_name, ident,
                        );
                    }

                    if is_macro_use {
                        insertion += &format!(
                            "{} use crate::{}::macros::{}::*;",
                            vis, self.cargo_equip_mod_name, ident,
                        );
                    }

                    let insertion = insertion.trim_start();

                    let pos = item_use.span().start();
                    self.replacements.insert((pos, pos), "/*".to_owned());
                    let pos = item_use.span().end();
                    self.replacements
                        .insert((pos, pos), "*/".to_owned() + insertion);
                }
            }
        }
    }

    pub(crate) fn process_extern_crates_in_lib(
        &mut self,
        convert_extern_crate_name: impl FnMut(&str) -> Option<String>,
        shell: &mut Shell,
    ) -> anyhow::Result<()> {
        self.apply()?;

        for item in &self.file.items {
            if let Item::ExternCrate(ItemExternCrate {
                vis,
                ident,
                rename: Some((_, rename)),
                ..
            }) = item
            {
                shell.warn(format!(
                    "declaring `extern crate .. as ..` in a root module is not recommended: \
                     `{} extern crate {} as {}`",
                    vis.to_token_stream(),
                    ident,
                    rename,
                ))?;
            }
        }

        Visitor {
            replacements: &mut self.replacements,
            cargo_equip_mod_name: self.cargo_equip_mod_name,
            convert_extern_crate_name,
        }
        .visit_file(&self.file);
        return Ok(());

        struct Visitor<'a, F> {
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
            cargo_equip_mod_name: &'a Ident,
            convert_extern_crate_name: F,
        }

        impl<F: FnMut(&str) -> Option<String>> Visit<'_> for Visitor<'_, F> {
            fn visit_item_extern_crate(&mut self, item_use: &ItemExternCrate) {
                let ItemExternCrate {
                    attrs,
                    vis,
                    ident,
                    rename,
                    semi_token,
                    ..
                } = item_use;

                if let Some(to) = (self.convert_extern_crate_name)(&ident.to_string()) {
                    let to = Ident::new(&to, Span::call_site());
                    let Self {
                        cargo_equip_mod_name,
                        ..
                    } = self;
                    self.replacements.insert(
                        (item_use.span().start(), semi_token.span().end()),
                        if let Some((_, rename)) = rename {
                            quote!(
                                #(#attrs)* #vis use crate::#cargo_equip_mod_name::crates::#to as #rename;
                            )
                            .to_string()
                        } else {
                            quote!(
                                #(#attrs)* #vis use crate::#cargo_equip_mod_name::crates::#to as #ident;
                            )
                            .to_string()
                        },
                    );
                }
            }
        }
    }
    pub(crate) fn translate_extern_crate_paths(
        &mut self,
        translate_extern_crate_name: impl FnMut(&str) -> Option<String>,
    ) -> anyhow::Result<()> {
        self.apply()?;
        Visitor {
            replacements: &mut self.replacements,
            cargo_equip_mod_name: self.cargo_equip_mod_name,
            translate_extern_crate_name,
        }
        .visit_file(&self.file);
        return Ok(());

        struct Visitor<'a, F> {
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
            cargo_equip_mod_name: &'a Ident,
            translate_extern_crate_name: F,
        }

        impl<F: FnMut(&str) -> Option<String>> Visitor<'_, F> {
            fn attempt_translate(&mut self, leading_colon: Span, extern_crate_name: &Ident) {
                if let Some(pseudo_extern_crate_name) =
                    (self.translate_extern_crate_name)(&extern_crate_name.to_string())
                {
                    self.replacements.insert(
                        (leading_colon.start(), leading_colon.end()),
                        format!("/*::*/crate::{}::crates::", self.cargo_equip_mod_name),
                    );

                    if extern_crate_name != &*pseudo_extern_crate_name {
                        let span = extern_crate_name.span();
                        self.replacements.insert(
                            (span.start(), span.end()),
                            format!("/*{}*/{}", extern_crate_name, pseudo_extern_crate_name),
                        );
                    }
                }
            }
        }

        impl<F: FnMut(&str) -> Option<String>> Visit<'_> for Visitor<'_, F> {
            fn visit_item_use(&mut self, i: &'_ ItemUse) {
                if let Some(leading_colon) = i.leading_colon {
                    for extern_crate_name in extract_first_segments(&i.tree) {
                        self.attempt_translate(leading_colon.span(), extern_crate_name);
                    }
                }

                fn extract_first_segments(tree: &UseTree) -> Vec<&Ident> {
                    match tree {
                        UseTree::Path(UsePath { ident, .. })
                        | UseTree::Name(UseName { ident })
                        | UseTree::Rename(UseRename { ident, .. }) => {
                            vec![ident]
                        }
                        UseTree::Glob(_) => vec![],
                        UseTree::Group(UseGroup { items, .. }) => {
                            items.iter().flat_map(extract_first_segments).collect()
                        }
                    }
                }
            }

            fn visit_path(&mut self, i: &'_ syn::Path) {
                if let Some(leading_colon) = i.leading_colon {
                    let PathSegment { ident, .. } = i
                        .segments
                        .last()
                        .expect("`syn::Path::segments` is considered not to be empty");
                    self.attempt_translate(leading_colon.span(), ident);
                }
            }
        }
    }

    pub(crate) fn translate_crate_path(&mut self, extern_crate_name: &str) -> anyhow::Result<()> {
        self.apply()?;
        Visitor {
            extern_crate_name,
            cargo_equip_mod_name: self.cargo_equip_mod_name,
            replacements: &mut self.replacements,
        }
        .visit_file(&self.file);
        return Ok(());

        struct Visitor<'a> {
            extern_crate_name: &'a str,
            cargo_equip_mod_name: &'a Ident,
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
        }

        impl Visitor<'_> {
            fn insert(&mut self, crate_token: &Ident) {
                let pos = crate_token.span().end();
                self.replacements.insert(
                    (pos, pos),
                    format!(
                        "::{}::crates::{}",
                        self.cargo_equip_mod_name, self.extern_crate_name,
                    ),
                );
            }
        }

        impl Visit<'_> for Visitor<'_> {
            fn visit_path(&mut self, path: &'_ syn::Path) {
                if let (None, Some(first)) = (path.leading_colon, path.segments.first()) {
                    if first.ident == "crate" && first.arguments.is_empty() {
                        self.insert(&first.ident);
                    }
                }
            }

            fn visit_item_use(&mut self, item_use: &'_ ItemUse) {
                if item_use.leading_colon.is_none() {
                    self.visit_use_tree(&item_use.tree);
                }
            }

            fn visit_use_tree(&mut self, use_tree: &UseTree) {
                match &use_tree {
                    UseTree::Path(UsePath { ident, .. })
                    | UseTree::Name(UseName { ident })
                    | UseTree::Rename(UseRename { ident, .. })
                        if ident == "crate" =>
                    {
                        self.insert(ident);
                    }
                    UseTree::Group(UseGroup { items, .. }) => {
                        for item in items {
                            self.visit_use_tree(item);
                        }
                    }
                    _ => {}
                }
            }

            fn visit_vis_restricted(&mut self, vis_restricted: &VisRestricted) {
                if vis_restricted.in_token.is_some() {
                    self.visit_path(&vis_restricted.path);
                } else if let Some(ident) = vis_restricted.path.get_ident() {
                    if ident == "crate" {
                        let pos = vis_restricted.path.span().start();
                        self.replacements.insert((pos, pos), "in ".to_owned());

                        self.insert(ident);
                    }
                }
            }
        }
    }

    pub(crate) fn modify_declarative_macros(
        &mut self,
        pseudo_extern_crate_name: &str,
    ) -> anyhow::Result<String> {
        self.apply()?;

        let mut macro_names = btreemap!();

        for item_macro in collect_item_macros(&self.file) {
            if let ItemMacro {
                attrs,
                ident: Some(ident),
                mac: Macro { tokens, .. },
                ..
            } = item_macro
            {
                replace_dollar_crates(
                    tokens.clone(),
                    self.cargo_equip_mod_name,
                    pseudo_extern_crate_name,
                    &mut self.replacements,
                );
                if attrs
                    .iter()
                    .flat_map(Attribute::parse_meta)
                    .any(|m| m.path().is_ident("macro_export"))
                {
                    let rename = format!(
                        "{}_macro_def_{}_{}",
                        self.cargo_equip_mod_name, pseudo_extern_crate_name, ident,
                    );
                    self.replacements.insert(
                        (ident.span().start(), ident.span().end()),
                        format!("/*{}*/{}", ident, rename),
                    );
                    let pos = item_macro.span().end();
                    self.replacements.insert(
                        (pos, pos),
                        format!(
                            "\nmacro_rules!{}{{($($tt:tt)*)=>(crate::{}!{{$($tt)*}})}}",
                            ident, rename,
                        ),
                    );
                    macro_names.insert(rename, ident);
                }
            }
        }

        if !macro_names.is_empty() {
            if let Some(first) = self.file.items.first() {
                let pos = first.span().start();
                self.replacements.entry((pos, pos)).or_default().insert_str(
                    0,
                    &format!(
                        "pub use crate::{}::macros::{}::*;",
                        self.cargo_equip_mod_name, pseudo_extern_crate_name,
                    ),
                );
            }
        }

        let macro_mod_content = if macro_names.is_empty() {
            "".to_owned()
        } else {
            format!(
                "pub use crate::{}{}{};\n",
                if macro_names.len() > 1 { "{" } else { "" },
                macro_names
                    .iter()
                    .map(|(rename, name)| if *name == rename {
                        name.to_string()
                    } else {
                        format!("{} as {}", rename, name)
                    })
                    .format(", "),
                if macro_names.len() > 1 { "}" } else { "" },
            )
        };

        return Ok(macro_mod_content);

        fn collect_item_macros(file: &syn::File) -> Vec<&ItemMacro> {
            let mut acc = vec![];
            Visitor { acc: &mut acc }.visit_file(file);
            return acc;

            struct Visitor<'a, 'b> {
                acc: &'b mut Vec<&'a ItemMacro>,
            }

            impl<'a, 'b> Visit<'a> for Visitor<'a, 'b> {
                fn visit_item_macro(&mut self, i: &'a ItemMacro) {
                    self.acc.push(i);
                }
            }
        }

        fn replace_dollar_crates(
            token_stream: TokenStream,
            cargo_equip_mod_name: &Ident,
            pseudo_extern_crate_name: &str,
            acc: &mut BTreeMap<(LineColumn, LineColumn), String>,
        ) {
            let mut token_stream = token_stream.into_iter().peekable();

            if let Some(proc_macro2::TokenTree::Group(group)) = token_stream.peek() {
                replace_dollar_crates(
                    group.stream(),
                    cargo_equip_mod_name,
                    pseudo_extern_crate_name,
                    acc,
                );
            }

            for (tt1, tt2) in token_stream.tuple_windows() {
                if let proc_macro2::TokenTree::Group(group) = &tt2 {
                    replace_dollar_crates(
                        group.stream(),
                        cargo_equip_mod_name,
                        pseudo_extern_crate_name,
                        acc,
                    );
                }

                if matches!(
                    (&tt1, &tt2),
                    (proc_macro2::TokenTree::Punct(p), proc_macro2::TokenTree::Ident(i))
                    if p.as_char() == '$' && i == "crate"
                ) {
                    let pos = tt2.span().end();
                    acc.insert(
                        (pos, pos),
                        format!(
                            "::{}::crates::{}",
                            cargo_equip_mod_name, pseudo_extern_crate_name,
                        ),
                    );
                }
            }
        }
    }

    pub(crate) fn resolve_pseudo_prelude(
        &mut self,
        pseudo_extern_crate_name: &str,
        libs_with_local_inner_macros: &BTreeSet<&str>,
        extern_crate_name_translation: &BTreeMap<String, String>,
    ) -> anyhow::Result<String> {
        if extern_crate_name_translation.is_empty() && libs_with_local_inner_macros.is_empty() {
            return Ok("".to_owned());
        }

        self.apply()?;

        let syn::File { attrs, items, .. } = &self.file;

        let external_local_inner_macros = {
            let macros = libs_with_local_inner_macros
                .iter()
                .map(|name| format!("{}::*", name))
                .join(", ");
            match libs_with_local_inner_macros.len() {
                0 => None,
                1 => Some(macros),
                _ => Some(format!("{{{}}}", macros)),
            }
        };

        let pseudo_extern_crates = {
            let uses = extern_crate_name_translation
                .iter()
                .map(|(extern_crate_name, pseudo_extern_crate_name)| {
                    if extern_crate_name == pseudo_extern_crate_name {
                        extern_crate_name.clone()
                    } else {
                        format!("{} as {}", pseudo_extern_crate_name, extern_crate_name)
                    }
                })
                .join(", ");
            match extern_crate_name_translation.len() {
                0 => None,
                1 => Some(uses),
                _ => Some(format!("{{{}}}", uses)),
            }
        };

        let mut prelude = "".to_owned();
        if let Some(external_local_inner_macros) = &external_local_inner_macros {
            prelude += &format!(
                "pub(in crate::{0}) use crate::{0}::macros::{1};",
                self.cargo_equip_mod_name, external_local_inner_macros,
            );
        }
        if let Some(pseudo_extern_crates) = &pseudo_extern_crates {
            prelude += &format!(
                "pub(in crate::{0}) use crate::{0}::crates::{1};",
                self.cargo_equip_mod_name, pseudo_extern_crates,
            );
        }

        self.replacements.insert(
            {
                let pos = if let Some(item) = items.first() {
                    item.span().start()
                } else if let Some(attr) = attrs.last() {
                    attr.span().end()
                } else {
                    LineColumn { line: 0, column: 0 }
                };
                (pos, pos)
            },
            {
                format!(
                    "use crate::{}::preludes::{}::*;",
                    self.cargo_equip_mod_name, pseudo_extern_crate_name,
                )
            },
        );

        let mut queue = items
            .iter()
            .flat_map(|item| match item {
                Item::Mod(item_mod) => Some((1, item_mod)),
                _ => None,
            })
            .collect::<VecDeque<_>>();

        while let Some((depth, ItemMod { attrs, content, .. })) = queue.pop_front() {
            let (_, items) = content.as_ref().expect("should be expanded");
            let pos = if let Some(item) = items.first() {
                item.span().start()
            } else if let Some(attr) = attrs.last() {
                attr.span().end()
            } else {
                LineColumn { line: 0, column: 0 }
            };
            self.replacements.insert(
                (pos, pos),
                format!(
                    "use crate::{}::preludes::{}::*;",
                    self.cargo_equip_mod_name, pseudo_extern_crate_name,
                ),
            );
            for item in items {
                if let Item::Mod(item_mod) = item {
                    queue.push_back((depth + 1, item_mod));
                }
            }
        }
        Ok(prelude)
    }
}
