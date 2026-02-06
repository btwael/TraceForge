use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields};

pub fn round_key_derive(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let variants = match &ast.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new(ast.span(), "RoundKey can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };

    let mut index_arms = Vec::new();
    let mut name_arms = Vec::new();
    for (idx, var) in variants.iter().enumerate() {
        if !matches!(var.fields, Fields::Unit) {
            return syn::Error::new(var.span(), "RoundKey requires fieldless enum variants")
                .to_compile_error()
                .into();
        }
        let vname = &var.ident;
        let vname_str = vname.to_string();
        index_arms.push(quote! { #name::#vname => #idx, });
        name_arms.push(quote! { #name::#vname => #vname_str, });
    }
    let count = variants.len();

    let gen = quote! {
        impl ::traceforge::comm_close::RoundKey for #name {
            const COUNT: usize = #count;
            fn index(self) -> usize {
                match self {
                    #(#index_arms)*
                }
            }
            fn name(self) -> &'static str {
                match self {
                    #(#name_arms)*
                }
            }
        }
    };
    gen.into()
}

pub fn round_enum_derive(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let variants = match &ast.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new(ast.span(), "RoundEnum can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };
    if variants.is_empty() {
        return syn::Error::new(ast.span(), "RoundEnum requires at least one variant")
            .to_compile_error()
            .into();
    }

    let mut to_u32_arms = Vec::new();
    let mut from_u32_arms = Vec::new();
    for (idx, var) in variants.iter().enumerate() {
        if !matches!(var.fields, Fields::Unit) {
            return syn::Error::new(var.span(), "RoundEnum requires fieldless enum variants")
                .to_compile_error()
                .into();
        }
        let vname = &var.ident;
        let idx_u32 = idx as u32;
        to_u32_arms.push(quote! { #name::#vname => #idx_u32, });
        from_u32_arms.push(quote! { #idx_u32 => Some(#name::#vname), });
    }
    let count = variants.len() as u32;
    let first = &variants[0].ident;
    let name_str = name.to_string();

    let gen = quote! {
        impl ::traceforge::comm_close::RoundEnum for #name {
            const NAME: &'static str = #name_str;
            const LOW: Self = #name::#first;
            const SIZE: u32 = #count;

            fn to_u32(self) -> u32 {
                match self {
                    #(#to_u32_arms)*
                }
            }

            fn try_from_u32(v: u32) -> Option<Self> {
                match v {
                    #(#from_u32_arms)*
                    _ => None,
                }
            }
        }
    };
    gen.into()
}
