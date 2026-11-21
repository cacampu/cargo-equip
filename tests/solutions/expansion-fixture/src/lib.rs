extern crate proc_macro;
use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn preserve_first(_: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro_attribute]
pub fn preserve_second(_: TokenStream, item: TokenStream) -> TokenStream {
    item
}

#[proc_macro]
pub fn expand_number(_: TokenStream) -> TokenStream {
    "42".parse().unwrap()
}
