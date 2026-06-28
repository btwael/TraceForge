use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

pub fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            input.generics,
            "Dim derive does not support generics",
        ));
    }

    let enum_ident = input.ident;
    let variants = match input.data {
        Data::Enum(data) => data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                enum_ident,
                "Dim can only be derived for enums",
            ));
        }
    };

    if variants.is_empty() {
        return Err(syn::Error::new_spanned(
            enum_ident,
            "Dim requires at least one enum variant",
        ));
    }

    for variant in &variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "Dim variants must not have fields",
            ));
        }
    }

    let variant_idents = variants
        .iter()
        .map(|variant| &variant.ident)
        .collect::<Vec<_>>();
    let first = variant_idents[0];
    let indexes = 0u32..variant_idents.len() as u32;
    let next_arms = variant_idents
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let next = variant_idents.get(index + 1);
            match next {
                Some(next) => quote! { Self::#variant => Some(Self::#next), },
                None => quote! { Self::#variant => None, },
            }
        })
        .collect::<Vec<_>>();

    Ok(quote! {
        impl ::traceforge_rounds::Dim for #enum_ident {
            fn initial() -> Self {
                Self::#first
            }

            fn next(self) -> Option<Self> {
                match self {
                    #(#next_arms)*
                }
            }
        }

        impl ::std::cmp::PartialOrd for #enum_ident {
            fn partial_cmp(&self, other: &Self) -> Option<::std::cmp::Ordering> {
                Some(self.__traceforge_rounds_dim_index().cmp(&other.__traceforge_rounds_dim_index()))
            }
        }

        impl #enum_ident {
            fn __traceforge_rounds_dim_index(&self) -> u32 {
                match self {
                    #(Self::#variant_idents => #indexes,)*
                }
            }
        }
    })
}
