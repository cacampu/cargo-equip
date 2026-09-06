//! Printing a `TokenStream` as the shortest text that still parses back to it.
//!
//! Vendored from [rustminify] 0.2.0 by Ryo Yamashita (qryxip), which is dual-licensed
//! MIT OR Apache-2.0 — the same terms as this crate. Only `minify_tokens` is taken;
//! the crate's other two functions (`minify_file` and `remove_docs`) are the only
//! reason it depends on `syn` 1, and neither is used here:
//!
//! - `minify_file(&file)` is `minify_tokens(file.to_token_stream())`.
//! - `remove_docs` is superseded by `CodeEdit::erase_docs`, which blanks out spans in
//!   the source text instead of rebuilding the AST. That keeps the original layout
//!   when `--remove docs` is used without `--minify`, and it reaches inside
//!   `macro_rules!` bodies, which an `attrs.retain` over AST nodes cannot.
//!
//! Vendoring it drops the last dependency on `syn` 1.
//!
//! [rustminify]: https://github.com/qryxip/rustminify

use proc_macro2::{Delimiter, LineColumn, Spacing, TokenStream, TokenTree};
use std::mem;

/// Minifies a [`TokenStream`].
///
/// Falls back to the plainly-spaced `tokens.to_string()` if the minified text does
/// not parse back into an equivalent token stream, so a bad result is never emitted.
pub fn minify_tokens(tokens: TokenStream) -> String {
    let safe = tokens.to_string();
    let mut acc = "".to_owned();
    minify_tokens(tokens.clone(), &mut acc);
    return if acc.parse().is_ok_and(|acc| equiv(acc, tokens)) {
        acc
    } else {
        safe
    };

    fn minify_tokens(tokens: TokenStream, acc: &mut String) {
        let mut st = State::None;
        for tt in tokens {
            match tt {
                TokenTree::Group(group) => {
                    if let State::PunctChars(puncts, _, _) = mem::replace(&mut st, State::None) {
                        *acc += &puncts;
                    }
                    let (left, right) = match group.delimiter() {
                        proc_macro2::Delimiter::Parenthesis => ('(', ')'),
                        proc_macro2::Delimiter::Brace => ('{', '}'),
                        proc_macro2::Delimiter::Bracket => ('[', ']'),
                        proc_macro2::Delimiter::None => (' ', ' '),
                    };
                    acc.push(left);
                    minify_tokens(group.stream(), acc);
                    acc.push(right);
                    st = State::None;
                }
                TokenTree::Ident(ident) => {
                    match mem::replace(
                        &mut st,
                        if ident.to_string().contains('#') {
                            State::PoundIdent
                        } else {
                            State::AlnumUnderscoreQuote
                        },
                    ) {
                        State::AlnumUnderscoreQuote | State::PoundIdent => *acc += " ",
                        State::PunctChars(puncts, _, _) => *acc += &puncts,
                        _ => {}
                    }
                    *acc += &ident.to_string();
                }
                TokenTree::Literal(literal) => {
                    let end = literal.span().end();
                    let literal = literal.to_string();
                    let (literal, next) = if let Some(literal) = literal.strip_suffix('.') {
                        (
                            literal,
                            State::PunctChars(".".to_owned(), end, Spacing::Alone),
                        )
                    } else {
                        (&*literal, State::AlnumUnderscoreQuote)
                    };
                    match mem::replace(&mut st, next) {
                        State::AlnumUnderscoreQuote | State::PoundIdent => *acc += " ",
                        State::PunctChars(puncts, _, _) => *acc += &puncts,
                        _ => {}
                    }
                    *acc += literal;
                }
                TokenTree::Punct(punct) => {
                    let cur_pos = punct.span().start();
                    if let State::PunctChars(puncts, prev_pos, spacing) = &mut st {
                        if *spacing == Spacing::Alone {
                            *acc += puncts;
                            // https://docs.rs/syn/1.0.46/syn/token/index.html
                            if !adjacent(*prev_pos, cur_pos)
                                && [
                                    ("!", '='),
                                    ("%", '='),
                                    ("&", '&'),
                                    ("&", '='),
                                    ("*", '='),
                                    ("+", '='),
                                    ("-", '='),
                                    ("-", '>'),
                                    (".", '.'),
                                    ("..", '.'),
                                    ("..", '='),
                                    ("/", '='),
                                    (":", ':'),
                                    ("<", '-'),
                                    ("<", '<'),
                                    ("<", '='),
                                    ("<<", '='),
                                    ("=", '='),
                                    ("=", '>'),
                                    (">", '='),
                                    (">", '>'),
                                    (">>", '='),
                                    ("^", '='),
                                    ("|", '='),
                                    ("|", '|'),
                                ]
                                .contains(&(puncts, punct.as_char()))
                            {
                                *acc += " ";
                            }
                            st = State::PunctChars(
                                punct.as_char().to_string(),
                                cur_pos,
                                punct.spacing(),
                            );
                        } else {
                            puncts.push(punct.as_char());
                            *spacing = punct.spacing();
                        }
                    } else {
                        if st == State::AlnumUnderscoreQuote && "#\"'".contains(punct.as_char()) {
                            *acc += " ";
                        }
                        st = State::PunctChars(
                            punct.as_char().to_string(),
                            cur_pos,
                            punct.spacing(),
                        );
                    }
                }
            }
        }
        if let State::PunctChars(puncts, _, _) = st {
            *acc += &puncts;
        }

        fn adjacent(pos1: LineColumn, pos2: LineColumn) -> bool {
            pos1.line == pos2.line && pos1.column + 1 == pos2.column
        }

        #[derive(PartialEq)]
        enum State {
            None,
            AlnumUnderscoreQuote,
            PoundIdent,
            PunctChars(String, LineColumn, Spacing),
        }
    }

    fn equiv(tokens1: TokenStream, tokens2: TokenStream) -> bool {
        return compress(tokens1) == compress(tokens2);

        fn compress(tokens: TokenStream) -> Vec<LossyTokenTree> {
            tokens.into_iter().map(Into::into).collect()
        }

        #[derive(PartialEq)]
        enum LossyTokenTree {
            Group(Delimiter, Vec<Self>),
            Ident(String),
            Punct(char),
            Literal(String),
        }

        impl From<TokenTree> for LossyTokenTree {
            fn from(tt: TokenTree) -> Self {
                match tt {
                    TokenTree::Group(group) => Self::Group(
                        group.delimiter(),
                        group.stream().into_iter().map(Into::into).collect(),
                    ),
                    TokenTree::Ident(ident) => Self::Ident(ident.to_string()),
                    TokenTree::Punct(punct) => Self::Punct(punct.as_char()),
                    TokenTree::Literal(literal) => Self::Literal(literal.to_string()),
                }
            }
        }
    }
}
