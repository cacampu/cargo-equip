//! Removing parts of the code that the judge does not need: `#[cfg(..)]` that is
//! statically known, doc comments (`--remove docs`) and ordinary comments
//! (`--remove comments`).

use super::{set_span, CodeEdit};
use anyhow::{anyhow, Context as _};
use fixedbitset::FixedBitSet;
use itertools::Itertools as _;
use proc_macro2::{Delimiter, LineColumn, TokenStream, TokenTree};
use quote::ToTokens;
use std::{borrow::Cow, collections::BTreeMap, str};
use syn::{
    spanned::Spanned,
    visit::{self, Visit},
    Arm, Attribute, BareFnArg, ConstParam, ExprArray, ExprAssign, ExprAssignOp, ExprAsync,
    ExprAwait, ExprBinary, ExprBlock, ExprBox, ExprBreak, ExprCall, ExprCast, ExprClosure,
    ExprContinue, ExprField, ExprForLoop, ExprGroup, ExprIf, ExprIndex, ExprLet, ExprLit, ExprLoop,
    ExprMacro, ExprMatch, ExprMethodCall, ExprParen, ExprPath, ExprRange, ExprReference,
    ExprRepeat, ExprReturn, ExprStruct, ExprTry, ExprTryBlock, ExprTuple, ExprType, ExprUnary,
    ExprUnsafe, ExprWhile, ExprYield, Field, FieldPat, FieldValue, ForeignItemFn, ForeignItemMacro,
    ForeignItemStatic, ForeignItemType, ImplItemConst, ImplItemMacro, ImplItemMethod, ImplItemType,
    ItemConst, ItemEnum, ItemExternCrate, ItemFn, ItemForeignMod, ItemImpl, ItemMacro, ItemMacro2,
    ItemMod, ItemStatic, ItemStruct, ItemTrait, ItemTraitAlias, ItemType, ItemUnion, ItemUse,
    LifetimeDef, Local, Meta, MetaList, NestedMeta, PatBox, PatIdent, PatLit, PatMacro, PatOr,
    PatPath, PatRange, PatReference, PatRest, PatSlice, PatStruct, PatTuple, PatTupleStruct,
    PatType, PatWild, Receiver, TraitItemConst, TraitItemMacro, TraitItemMethod, TraitItemType,
    TypeParam, Variadic, Variant,
};

impl<'opt> CodeEdit<'opt> {
    pub(crate) fn resolve_cfgs(&mut self, features: &[&str]) -> anyhow::Result<()> {
        self.apply()?;
        Visitor {
            replacements: &mut self.replacements,
            features,
        }
        .visit_file(&self.file);
        return Ok(());

        struct Visitor<'a> {
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
            features: &'a [&'a str],
        }

        impl Visitor<'_> {
            fn proceed<'a, T: ToTokens>(
                &mut self,
                i: &'a T,
                attrs: fn(&T) -> &[Attribute],
                visit: fn(&mut Self, &'a T),
            ) {
                let sufficiencies = attrs(i)
                    .iter()
                    .flat_map(|a| a.parse_meta().map(|m| (a.span(), m)))
                    .flat_map(|(span, meta)| match meta {
                        Meta::List(meta_list) => Some((span, meta_list)),
                        _ => None,
                    })
                    .filter(|(_, MetaList { path, .. })| path.is_ident("cfg"))
                    .flat_map(|(span, MetaList { nested, .. })| {
                        let expr =
                            cfg_expr::Expression::parse(&nested.to_token_stream().to_string())
                                .ok()?;
                        Some((span, expr))
                    })
                    .map(|(span, expr)| {
                        let sufficiency = expr.eval(|pred| match pred {
                            cfg_expr::Predicate::Test | cfg_expr::Predicate::ProcMacro => {
                                Some(false)
                            }
                            cfg_expr::Predicate::Flag("cargo_equip") => Some(true),
                            cfg_expr::Predicate::Feature(feature) => {
                                Some(self.features.contains(feature))
                            }
                            _ => None,
                        });
                        (span, sufficiency)
                    })
                    .collect::<Vec<_>>();

                if sufficiencies.iter().any(|&(_, p)| p == Some(false)) {
                    self.replacements
                        .insert((i.span().start(), i.span().end()), "".to_owned());
                } else {
                    for (span, p) in sufficiencies {
                        if p == Some(true) {
                            self.replacements
                                .insert((span.start(), span.end()), "".to_owned());
                        }
                    }
                    visit(self, i);
                }
            }
        }

        macro_rules! impl_visits {
            ($(fn $method:ident(&mut self, _: &'_ $ty:path) { _(_, _, $visit:path) })*) => {
                $(
                    fn $method(&mut self, i: &'_ $ty) {
                        self.proceed(i, |$ty { attrs, .. }| attrs, $visit);
                    }
                )*
            };
        }

        impl Visit<'_> for Visitor<'_> {
            impl_visits! {
                fn visit_arm                (&mut self, _: &'_ Arm              ) { _(_, _, visit::visit_arm                ) }
                fn visit_bare_fn_arg        (&mut self, _: &'_ BareFnArg        ) { _(_, _, visit::visit_bare_fn_arg        ) }
                fn visit_const_param        (&mut self, _: &'_ ConstParam       ) { _(_, _, visit::visit_const_param        ) }
                fn visit_expr_array         (&mut self, _: &'_ ExprArray        ) { _(_, _, visit::visit_expr_array         ) }
                fn visit_expr_assign        (&mut self, _: &'_ ExprAssign       ) { _(_, _, visit::visit_expr_assign        ) }
                fn visit_expr_assign_op     (&mut self, _: &'_ ExprAssignOp     ) { _(_, _, visit::visit_expr_assign_op     ) }
                fn visit_expr_async         (&mut self, _: &'_ ExprAsync        ) { _(_, _, visit::visit_expr_async         ) }
                fn visit_expr_await         (&mut self, _: &'_ ExprAwait        ) { _(_, _, visit::visit_expr_await         ) }
                fn visit_expr_binary        (&mut self, _: &'_ ExprBinary       ) { _(_, _, visit::visit_expr_binary        ) }
                fn visit_expr_block         (&mut self, _: &'_ ExprBlock        ) { _(_, _, visit::visit_expr_block         ) }
                fn visit_expr_box           (&mut self, _: &'_ ExprBox          ) { _(_, _, visit::visit_expr_box           ) }
                fn visit_expr_break         (&mut self, _: &'_ ExprBreak        ) { _(_, _, visit::visit_expr_break         ) }
                fn visit_expr_call          (&mut self, _: &'_ ExprCall         ) { _(_, _, visit::visit_expr_call          ) }
                fn visit_expr_cast          (&mut self, _: &'_ ExprCast         ) { _(_, _, visit::visit_expr_cast          ) }
                fn visit_expr_closure       (&mut self, _: &'_ ExprClosure      ) { _(_, _, visit::visit_expr_closure       ) }
                fn visit_expr_continue      (&mut self, _: &'_ ExprContinue     ) { _(_, _, visit::visit_expr_continue      ) }
                fn visit_expr_field         (&mut self, _: &'_ ExprField        ) { _(_, _, visit::visit_expr_field         ) }
                fn visit_expr_for_loop      (&mut self, _: &'_ ExprForLoop      ) { _(_, _, visit::visit_expr_for_loop      ) }
                fn visit_expr_group         (&mut self, _: &'_ ExprGroup        ) { _(_, _, visit::visit_expr_group         ) }
                fn visit_expr_if            (&mut self, _: &'_ ExprIf           ) { _(_, _, visit::visit_expr_if            ) }
                fn visit_expr_index         (&mut self, _: &'_ ExprIndex        ) { _(_, _, visit::visit_expr_index         ) }
                fn visit_expr_let           (&mut self, _: &'_ ExprLet          ) { _(_, _, visit::visit_expr_let           ) }
                fn visit_expr_lit           (&mut self, _: &'_ ExprLit          ) { _(_, _, visit::visit_expr_lit           ) }
                fn visit_expr_loop          (&mut self, _: &'_ ExprLoop         ) { _(_, _, visit::visit_expr_loop          ) }
                fn visit_expr_macro         (&mut self, _: &'_ ExprMacro        ) { _(_, _, visit::visit_expr_macro         ) }
                fn visit_expr_match         (&mut self, _: &'_ ExprMatch        ) { _(_, _, visit::visit_expr_match         ) }
                fn visit_expr_method_call   (&mut self, _: &'_ ExprMethodCall   ) { _(_, _, visit::visit_expr_method_call   ) }
                fn visit_expr_paren         (&mut self, _: &'_ ExprParen        ) { _(_, _, visit::visit_expr_paren         ) }
                fn visit_expr_path          (&mut self, _: &'_ ExprPath         ) { _(_, _, visit::visit_expr_path          ) }
                fn visit_expr_range         (&mut self, _: &'_ ExprRange        ) { _(_, _, visit::visit_expr_range         ) }
                fn visit_expr_reference     (&mut self, _: &'_ ExprReference    ) { _(_, _, visit::visit_expr_reference     ) }
                fn visit_expr_repeat        (&mut self, _: &'_ ExprRepeat       ) { _(_, _, visit::visit_expr_repeat        ) }
                fn visit_expr_return        (&mut self, _: &'_ ExprReturn       ) { _(_, _, visit::visit_expr_return        ) }
                fn visit_expr_struct        (&mut self, _: &'_ ExprStruct       ) { _(_, _, visit::visit_expr_struct        ) }
                fn visit_expr_try           (&mut self, _: &'_ ExprTry          ) { _(_, _, visit::visit_expr_try           ) }
                fn visit_expr_try_block     (&mut self, _: &'_ ExprTryBlock     ) { _(_, _, visit::visit_expr_try_block     ) }
                fn visit_expr_tuple         (&mut self, _: &'_ ExprTuple        ) { _(_, _, visit::visit_expr_tuple         ) }
                fn visit_expr_type          (&mut self, _: &'_ ExprType         ) { _(_, _, visit::visit_expr_type          ) }
                fn visit_expr_unary         (&mut self, _: &'_ ExprUnary        ) { _(_, _, visit::visit_expr_unary         ) }
                fn visit_expr_unsafe        (&mut self, _: &'_ ExprUnsafe       ) { _(_, _, visit::visit_expr_unsafe        ) }
                fn visit_expr_while         (&mut self, _: &'_ ExprWhile        ) { _(_, _, visit::visit_expr_while         ) }
                fn visit_expr_yield         (&mut self, _: &'_ ExprYield        ) { _(_, _, visit::visit_expr_yield         ) }
                fn visit_field              (&mut self, _: &'_ Field            ) { _(_, _, visit::visit_field              ) }
                fn visit_field_pat          (&mut self, _: &'_ FieldPat         ) { _(_, _, visit::visit_field_pat          ) }
                fn visit_field_value        (&mut self, _: &'_ FieldValue       ) { _(_, _, visit::visit_field_value        ) }
                fn visit_file               (&mut self, _: &'_ syn::File        ) { _(_, _, visit::visit_file               ) }
                fn visit_foreign_item_fn    (&mut self, _: &'_ ForeignItemFn    ) { _(_, _, visit::visit_foreign_item_fn    ) }
                fn visit_foreign_item_macro (&mut self, _: &'_ ForeignItemMacro ) { _(_, _, visit::visit_foreign_item_macro ) }
                fn visit_foreign_item_static(&mut self, _: &'_ ForeignItemStatic) { _(_, _, visit::visit_foreign_item_static) }
                fn visit_foreign_item_type  (&mut self, _: &'_ ForeignItemType  ) { _(_, _, visit::visit_foreign_item_type  ) }
                fn visit_impl_item_const    (&mut self, _: &'_ ImplItemConst    ) { _(_, _, visit::visit_impl_item_const    ) }
                fn visit_impl_item_macro    (&mut self, _: &'_ ImplItemMacro    ) { _(_, _, visit::visit_impl_item_macro    ) }
                fn visit_impl_item_method   (&mut self, _: &'_ ImplItemMethod   ) { _(_, _, visit::visit_impl_item_method   ) }
                fn visit_impl_item_type     (&mut self, _: &'_ ImplItemType     ) { _(_, _, visit::visit_impl_item_type     ) }
                fn visit_item_const         (&mut self, _: &'_ ItemConst        ) { _(_, _, visit::visit_item_const         ) }
                fn visit_item_enum          (&mut self, _: &'_ ItemEnum         ) { _(_, _, visit::visit_item_enum          ) }
                fn visit_item_extern_crate  (&mut self, _: &'_ ItemExternCrate  ) { _(_, _, visit::visit_item_extern_crate  ) }
                fn visit_item_fn            (&mut self, _: &'_ ItemFn           ) { _(_, _, visit::visit_item_fn            ) }
                fn visit_item_foreign_mod   (&mut self, _: &'_ ItemForeignMod   ) { _(_, _, visit::visit_item_foreign_mod   ) }
                fn visit_item_impl          (&mut self, _: &'_ ItemImpl         ) { _(_, _, visit::visit_item_impl          ) }
                fn visit_item_macro         (&mut self, _: &'_ ItemMacro        ) { _(_, _, visit::visit_item_macro         ) }
                fn visit_item_macro2        (&mut self, _: &'_ ItemMacro2       ) { _(_, _, visit::visit_item_macro2        ) }
                fn visit_item_mod           (&mut self, _: &'_ ItemMod          ) { _(_, _, visit::visit_item_mod           ) }
                fn visit_item_static        (&mut self, _: &'_ ItemStatic       ) { _(_, _, visit::visit_item_static        ) }
                fn visit_item_struct        (&mut self, _: &'_ ItemStruct       ) { _(_, _, visit::visit_item_struct        ) }
                fn visit_item_trait         (&mut self, _: &'_ ItemTrait        ) { _(_, _, visit::visit_item_trait         ) }
                fn visit_item_trait_alias   (&mut self, _: &'_ ItemTraitAlias   ) { _(_, _, visit::visit_item_trait_alias   ) }
                fn visit_item_type          (&mut self, _: &'_ ItemType         ) { _(_, _, visit::visit_item_type          ) }
                fn visit_item_union         (&mut self, _: &'_ ItemUnion        ) { _(_, _, visit::visit_item_union         ) }
                fn visit_item_use           (&mut self, _: &'_ ItemUse          ) { _(_, _, visit::visit_item_use           ) }
                fn visit_lifetime_def       (&mut self, _: &'_ LifetimeDef      ) { _(_, _, visit::visit_lifetime_def       ) }
                fn visit_local              (&mut self, _: &'_ Local            ) { _(_, _, visit::visit_local              ) }
                fn visit_pat_box            (&mut self, _: &'_ PatBox           ) { _(_, _, visit::visit_pat_box            ) }
                fn visit_pat_ident          (&mut self, _: &'_ PatIdent         ) { _(_, _, visit::visit_pat_ident          ) }
                fn visit_pat_lit            (&mut self, _: &'_ PatLit           ) { _(_, _, visit::visit_pat_lit            ) }
                fn visit_pat_macro          (&mut self, _: &'_ PatMacro         ) { _(_, _, visit::visit_pat_macro          ) }
                fn visit_pat_or             (&mut self, _: &'_ PatOr            ) { _(_, _, visit::visit_pat_or             ) }
                fn visit_pat_path           (&mut self, _: &'_ PatPath          ) { _(_, _, visit::visit_pat_path           ) }
                fn visit_pat_range          (&mut self, _: &'_ PatRange         ) { _(_, _, visit::visit_pat_range          ) }
                fn visit_pat_reference      (&mut self, _: &'_ PatReference     ) { _(_, _, visit::visit_pat_reference      ) }
                fn visit_pat_rest           (&mut self, _: &'_ PatRest          ) { _(_, _, visit::visit_pat_rest           ) }
                fn visit_pat_slice          (&mut self, _: &'_ PatSlice         ) { _(_, _, visit::visit_pat_slice          ) }
                fn visit_pat_struct         (&mut self, _: &'_ PatStruct        ) { _(_, _, visit::visit_pat_struct         ) }
                fn visit_pat_tuple          (&mut self, _: &'_ PatTuple         ) { _(_, _, visit::visit_pat_tuple          ) }
                fn visit_pat_tuple_struct   (&mut self, _: &'_ PatTupleStruct   ) { _(_, _, visit::visit_pat_tuple_struct   ) }
                fn visit_pat_type           (&mut self, _: &'_ PatType          ) { _(_, _, visit::visit_pat_type           ) }
                fn visit_pat_wild           (&mut self, _: &'_ PatWild          ) { _(_, _, visit::visit_pat_wild           ) }
                fn visit_receiver           (&mut self, _: &'_ Receiver         ) { _(_, _, visit::visit_receiver           ) }
                fn visit_trait_item_const   (&mut self, _: &'_ TraitItemConst   ) { _(_, _, visit::visit_trait_item_const   ) }
                fn visit_trait_item_macro   (&mut self, _: &'_ TraitItemMacro   ) { _(_, _, visit::visit_trait_item_macro   ) }
                fn visit_trait_item_method  (&mut self, _: &'_ TraitItemMethod  ) { _(_, _, visit::visit_trait_item_method  ) }
                fn visit_trait_item_type    (&mut self, _: &'_ TraitItemType    ) { _(_, _, visit::visit_trait_item_type    ) }
                fn visit_type_param         (&mut self, _: &'_ TypeParam        ) { _(_, _, visit::visit_type_param         ) }
                fn visit_variadic           (&mut self, _: &'_ Variadic         ) { _(_, _, visit::visit_variadic           ) }
                fn visit_variant            (&mut self, _: &'_ Variant          ) { _(_, _, visit::visit_variant            ) }
            }
        }
    }

    pub(crate) fn allow_missing_docs(&mut self) {
        Visitor {
            replacements: &mut self.replacements,
        }
        .visit_file(&self.file);

        struct Visitor<'a> {
            replacements: &'a mut BTreeMap<(LineColumn, LineColumn), String>,
        }

        impl Visit<'_> for Visitor<'_> {
            fn visit_attribute(&mut self, i: &Attribute) {
                if let Ok(Meta::List(MetaList { path, nested, .. })) = i.parse_meta() {
                    if ["warn", "deny", "forbid"]
                        .iter()
                        .any(|lint| path.is_ident(lint))
                    {
                        for meta in nested {
                            if let NestedMeta::Meta(Meta::Path(path)) = meta {
                                if ["missing_docs", "missing_crate_level_docs"]
                                    .iter()
                                    .any(|lint| path.is_ident(lint))
                                {
                                    let pos = path.span().start();
                                    self.replacements.insert((pos, pos), "/*".to_owned());
                                    let pos = path.span().end();
                                    self.replacements.insert((pos, pos), "*/".to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn erase_docs(&mut self) -> anyhow::Result<()> {
        return self.erase(
            |mask, token_stream| {
                visit_token_stream(mask, token_stream);
                Ok(())
            },
            || "broke the code during erasing doc comments",
        );

        // Walks the raw token stream instead of a `syn::File` so that doc comments
        // inside macro bodies (`macro_rules!` definitions, macro invocations) are
        // erased too. Note that `/// ..` and `//! ..` are lexed into
        // `#[doc = ".."]` / `#![doc = ".."]` token sequences.
        fn visit_token_stream(mask: &mut [FixedBitSet], token_stream: TokenStream) {
            let tts = token_stream.into_iter().collect::<Vec<_>>();
            let mut i = 0;
            while i < tts.len() {
                if let Some(len) = doc_attr_len(&tts[i..]) {
                    for tt in &tts[i..i + len] {
                        set_span(mask, tt.span(), true);
                    }
                    i += len;
                    continue;
                }
                if let TokenTree::Group(group) = &tts[i] {
                    visit_token_stream(mask, group.stream());
                }
                i += 1;
            }
        }

        /// Returns the number of leading token trees forming `#[doc ..]` or `#![doc ..]`.
        fn doc_attr_len(tts: &[TokenTree]) -> Option<usize> {
            let (rest, len) = match tts {
                [TokenTree::Punct(p), rest @ ..] if p.as_char() == '#' => (rest, 2),
                _ => return None,
            };
            let (rest, len) = match rest {
                [TokenTree::Punct(p), rest @ ..] if p.as_char() == '!' => (rest, len + 1),
                _ => (rest, len),
            };
            match rest {
                [TokenTree::Group(g), ..] if g.delimiter() == Delimiter::Bracket => {
                    match g.stream().into_iter().next() {
                        Some(TokenTree::Ident(i)) if i == "doc" => Some(len),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }

    pub(crate) fn erase_comments(&mut self) -> anyhow::Result<()> {
        return self.erase(
            |mask, token_stream| {
                for mask in &mut *mask {
                    mask.insert_range(..);
                }
                visit_token_stream(mask, token_stream);
                Ok(())
            },
            || "broke the code during erasing comments",
        );

        fn visit_token_stream(mask: &mut [FixedBitSet], token_stream: TokenStream) {
            for tt in token_stream {
                if let TokenTree::Group(group) = tt {
                    set_span(mask, group.span_open(), false);
                    visit_token_stream(mask, group.stream());
                    set_span(mask, group.span_close(), false);
                } else {
                    set_span(mask, tt.span(), false);
                }
            }
        }
    }

    fn erase(
        &mut self,
        visit_file: fn(&mut [FixedBitSet], TokenStream) -> syn::Result<()>,
        err_msg: fn() -> &'static str,
    ) -> anyhow::Result<()> {
        self.apply()?;

        let code = &if self.string.contains("\r\n") {
            Cow::from(self.string.replace("\r\n", "\n"))
        } else {
            Cow::from(&self.string)
        };

        let code = if code.starts_with("#!") {
            let (_, code) = code.split_at(code.find('\n').unwrap_or(code.len()));
            code
        } else {
            code
        };

        let token_stream = code
            .parse::<TokenStream>()
            .map_err(|e| anyhow!("{}", e))
            .with_context(|| "broke the code during modification")?;

        let mut erase = code
            .lines()
            .map(|l| FixedBitSet::with_capacity(l.chars().count()))
            .collect::<Vec<_>>();

        visit_file(&mut erase, token_stream)
            .map_err(|e| anyhow!("{:?}", e))
            .with_context(err_msg)?;

        let mut acc = "".to_owned();
        for (line, erase) in code.lines().zip_eq(erase) {
            for (j, ch) in line.chars().enumerate() {
                acc.push(if erase[j] { ' ' } else { ch });
            }
            acc += "\n";
        }
        self.string = acc.trim_start().to_owned();
        self.apply()
    }
}

#[cfg(test)]
mod tests {
    use crate::rust::CodeEdit;
    use pretty_assertions::assert_eq;
    use proc_macro2::Span;
    use syn::Ident;

    thread_local! {
        static DUMMY_MOD_NAME: Ident = Ident::new("__", Span::call_site());
    }

    /// `resolve_cfgs` had no test at all, and the feature list it takes is the one
    /// place the bundler feeds `cargo_metadata` data into the code rewriter, so it
    /// is the part most likely to break silently when that dependency moves.
    #[test]
    fn resolve_cfgs() -> anyhow::Result<()> {
        fn test(features: &[&str], input: &str, expected: &str) -> anyhow::Result<()> {
            DUMMY_MOD_NAME.with(|dummy_mod_name| {
                let mut edit = CodeEdit::from_code(dummy_mod_name, input)?;
                edit.resolve_cfgs(features)?;
                assert_eq!(expected, edit.finish()?);
                Ok(())
            })
        }

        // An enabled feature: the `#[cfg]` goes away, the item stays.
        test(
            &["std"],
            r#"#[cfg(feature = "std")]
struct Enabled;
"#,
            r#"
struct Enabled;
"#,
        )?;

        // A feature that is off takes the whole item with it.
        test(
            &["std"],
            r#"#[cfg(feature = "nope")]
struct Disabled;

struct Kept;
"#,
            r#"


struct Kept;
"#,
        )?;

        // `test` is always false in the bundled output, `cargo_equip` always true.
        test(
            &[],
            r#"#[cfg(test)]
mod tests {}

#[cfg(cargo_equip)]
struct Marked;
"#,
            r#"



struct Marked;
"#,
        )?;

        // An unknown predicate is left alone rather than guessed at.
        test(
            &[],
            r#"#[cfg(target_os = "linux")]
struct Unknown;
"#,
            r#"#[cfg(target_os = "linux")]
struct Unknown;
"#,
        )
    }

    #[test]
    fn erase_docs() -> anyhow::Result<()> {
        fn test(input: &str, expected: &str) -> anyhow::Result<()> {
            DUMMY_MOD_NAME.with(|dummy_mod_name| {
                let mut edit = CodeEdit::from_code(dummy_mod_name, input)?;
                edit.erase_docs()?;
                assert_eq!(expected, edit.finish()?);
                Ok(())
            })
        }

        test(
            r#"//! aaaaa
//! bbbbb

fn main() {}

/// ccccc
struct Foo;
"#,
            r#"fn main() {}

         
struct Foo;
"#,
        )?;

        test(
            r#"//! モジュールのドキュメント

/// アイテムのドキュメント
fn foo() {}
"#,
            r#"fn foo() {}
"#,
        )?;

        test(
            r#"#[doc = "aaaaa"]
struct Foo;

#[doc(hidden)]
struct Bar;
"#,
            r#"struct Foo;

              
struct Bar;
"#,
        )?;

        // doc comments inside macro bodies are not visited by `syn::visit`
        test(
            r#"macro_rules! m {
    () => {
        /// ccccc
        pub struct Baz;
    };
}

m! {
    #[doc = "ddddd"]
    pub struct Qux;
}
"#,
            r#"macro_rules! m {
    () => {
                 
        pub struct Baz;
    };
}

m! {
                    
    pub struct Qux;
}
"#,
        )
    }

    #[test]
    fn erase_comments() -> anyhow::Result<()> {
        fn test(input: &str, expected: &str) -> anyhow::Result<()> {
            DUMMY_MOD_NAME.with(|dummy_mod_name| {
                let mut edit = CodeEdit::from_code(dummy_mod_name, input)?;
                edit.erase_comments()?;
                assert_eq!(expected, edit.finish()?);
                Ok(())
            })
        }

        test(
            r#"// aaaaa
// bbbbb
fn main() {
    // ccccc
    /*ddddd*/println!("Hi!");/*eeeee*/
    // fffff
}
// ggggg
"#,
            r#"fn main() {
            
             println!("Hi!");         
            
}
        
"#,
        )?;

        test(
            r#"/* aaaaa */ type A = (i64, i64, i64); // bbbbb
"#,
            r#"type A = (i64, i64, i64);         
"#,
        )?;

        test(
            r#"// あああ
/*いいい*/fn foo() {
    let _ = 1 + 1; // ううううう
}
"#,
            r#"fn foo() {
    let _ = 1 + 1;         
}
"#,
        )
    }
}
