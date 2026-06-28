extern crate proc_macro;

mod dim;
mod round;

use proc_macro::TokenStream;

#[proc_macro_derive(Dim)]
pub fn derive_dim(input: TokenStream) -> TokenStream {
    dim::derive(input)
}

#[proc_macro_derive(Round)]
pub fn derive_round(input: TokenStream) -> TokenStream {
    round::derive(input)
}
