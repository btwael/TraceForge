use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Ident, Type, Visibility};

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
            "Round derive does not support generics",
        ));
    }

    let struct_ident = input.ident;
    let vis = input.vis;
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    struct_ident,
                    "Round can only be derived for structs with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                struct_ident,
                "Round can only be derived for structs",
            ));
        }
    };

    if fields.is_empty() {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "Round requires at least one field",
        ));
    }

    let mut round_fields = Vec::new();
    for field in fields {
        if !matches!(field.vis, Visibility::Inherited) {
            return Err(syn::Error::new_spanned(
                field,
                "Round fields must be private",
            ));
        }

        let ident = field.ident.expect("named fields have identifiers");
        let kind = FieldKind::from_ty(&field.ty);
        round_fields.push(RoundField {
            variant: format_ident!("{}", to_pascal_case(&ident.to_string())),
            ident,
            ty: field.ty,
            kind,
        });
    }

    let dim_ident = format_ident!("{struct_ident}Dim");
    let rounds_ext_ident = format_ident!("{struct_ident}RoundsExt");
    let field_idents = round_fields
        .iter()
        .map(|field| &field.ident)
        .collect::<Vec<_>>();
    let field_tys = round_fields
        .iter()
        .map(|field| &field.ty)
        .collect::<Vec<_>>();
    let variants = round_fields
        .iter()
        .map(|field| &field.variant)
        .collect::<Vec<_>>();
    let getters = round_fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            let ty = &field.ty;
            quote! {
                pub fn #ident(&self) -> #ty {
                    self.#ident
                }
            }
        })
        .collect::<Vec<_>>();

    let dim_constructors = round_fields
        .iter()
        .map(|field| {
            let name = format_ident!("dim_{}", field.ident);
            let variant = &field.variant;
            quote! {
                pub fn #name() -> #dim_ident {
                    #dim_ident::#variant
                }
            }
        })
        .collect::<Vec<_>>();

    let initial_values = round_fields
        .iter()
        .map(|field| field.kind.initial(&field.ty))
        .collect::<Vec<_>>();

    let advance_arms = round_fields
        .iter()
        .enumerate()
        .map(|(advance_index, field)| {
            let variant = &field.variant;
            let values = round_fields
                .iter()
                .enumerate()
                .map(|(index, output_field)| {
                    let ident = &output_field.ident;
                    let ty = &output_field.ty;
                    if index < advance_index {
                        quote! { #ident: current.#ident }
                    } else if index == advance_index {
                        let value = output_field.kind.advance(ty, ident);
                        quote! { #ident: #value }
                    } else {
                        let value = output_field.kind.initial(ty);
                        quote! { #ident: #value }
                    }
                })
                .collect::<Vec<_>>();

            quote! {
                #dim_ident::#variant => Self {
                    #(#values,)*
                },
            }
        })
        .collect::<Vec<_>>();

    let tick_steps = round_fields
        .iter()
        .enumerate()
        .rev()
        .map(|(advance_index, field)| {
            let ident = &field.ident;
            let try_advance = field.kind.try_advance(&field.ty, ident);
            let values = round_fields
                .iter()
                .enumerate()
                .map(|(index, output_field)| {
                    let output_ident = &output_field.ident;
                    let output_ty = &output_field.ty;
                    if index < advance_index {
                        quote! { #output_ident: current.#output_ident }
                    } else if index == advance_index {
                        quote! { #output_ident: next }
                    } else {
                        let value = output_field.kind.initial(output_ty);
                        quote! { #output_ident: #value }
                    }
                })
                .collect::<Vec<_>>();

            quote! {
                if let Some(next) = #try_advance {
                    return Some(Self {
                        #(#values,)*
                    });
                }
            }
        })
        .collect::<Vec<_>>();

    let advance_to_signatures = round_fields
        .iter()
        .map(|field| {
            let name = format_ident!("advance_to_{}", field.ident);
            let ty = &field.ty;
            quote! {
                fn #name(&mut self, target: #ty) -> ::std::result::Result<&#struct_ident, ::traceforge_rounds::PastRound<#struct_ident>>;
            }
        })
        .collect::<Vec<_>>();

    let advance_to_impls = round_fields
        .iter()
        .enumerate()
        .map(|(target_index, field)| {
            let name = format_ident!("advance_to_{}", field.ident);
            let target_ty = &field.ty;
            let values = round_fields
                .iter()
                .enumerate()
                .map(|(index, output_field)| {
                    let ident = &output_field.ident;
                    let ty = &output_field.ty;
                    if index < target_index {
                        quote! { #ident: current.#ident }
                    } else if index == target_index {
                        quote! { #ident: target }
                    } else {
                        let value = output_field.kind.initial(ty);
                        quote! { #ident: #value }
                    }
                })
                .collect::<Vec<_>>();

            quote! {
                fn #name(&mut self, target: #target_ty) -> ::std::result::Result<&#struct_ident, ::traceforge_rounds::PastRound<#struct_ident>> {
                    let target_round = {
                        let current = self.current();
                        #struct_ident {
                            #(#values,)*
                        }
                    };
                    self.jump(target_round)
                }
            }
        })
        .collect::<Vec<_>>();

    let dim_name_arms = round_fields
        .iter()
        .map(|field| {
            let variant = &field.variant;
            let name = field.ident.to_string();
            quote! { #dim_ident::#variant => #name, }
        })
        .collect::<Vec<_>>();

    let partial_cmp_steps = round_fields
        .iter()
        .map(|field| {
            let ident = &field.ident;
            quote! {
                match self.#ident.partial_cmp(&other.#ident)? {
                    ::std::cmp::Ordering::Equal => {}
                    ord => return Some(ord),
                }
            }
        })
        .collect::<Vec<_>>();

    Ok(quote! {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[doc(hidden)]
        #vis enum #dim_ident {
            #(#variants,)*
        }

        impl #struct_ident {
            #(#getters)*
            #(#dim_constructors)*
        }

        #vis trait #rounds_ext_ident {
            #(#advance_to_signatures)*
        }

        impl #rounds_ext_ident for ::traceforge_rounds::Rounds<#struct_ident> {
            #(#advance_to_impls)*
        }

        impl ::std::cmp::PartialOrd for #struct_ident {
            fn partial_cmp(&self, other: &Self) -> Option<::std::cmp::Ordering> {
                #(#partial_cmp_steps)*
                Some(::std::cmp::Ordering::Equal)
            }
        }

        unsafe impl ::traceforge_rounds::__private::TrustedRound for #struct_ident {}

        unsafe impl ::traceforge_rounds::Round for #struct_ident {
            type Dim = #dim_ident;

            fn initial() -> Self {
                Self {
                    #(#field_idents: #initial_values,)*
                }
            }

            fn tick(current: &Self) -> Option<Self> {
                #(#tick_steps)*
                None
            }

            fn advance_dim(current: &Self, dim: Self::Dim) -> Self {
                match dim {
                    #(#advance_arms)*
                }
            }

            fn dim_name(dim: Self::Dim) -> &'static str {
                match dim {
                    #(#dim_name_arms)*
                }
            }
        }

        const _: fn() = || {
            fn assert_copy<T: Copy>() {}
            #(assert_copy::<#field_tys>();)*
        };
    })
}

struct RoundField {
    ident: Ident,
    variant: Ident,
    ty: Type,
    kind: FieldKind,
}

enum FieldKind {
    Counter,
    Dim,
}

impl FieldKind {
    fn from_ty(ty: &Type) -> Self {
        if let Type::Path(path) = ty {
            if path.qself.is_none()
                && path.path.segments.len() == 1
                && is_counter_type(&path.path.segments[0].ident)
            {
                return Self::Counter;
            }
        }

        Self::Dim
    }

    fn initial(&self, ty: &Type) -> proc_macro2::TokenStream {
        match self {
            Self::Counter => quote! { 0 },
            Self::Dim => quote! { <#ty as ::traceforge_rounds::Dim>::initial() },
        }
    }

    fn advance(&self, ty: &Type, ident: &Ident) -> proc_macro2::TokenStream {
        match self {
            Self::Counter => quote! {
                current.#ident.checked_add(1).expect("round dimension overflow")
            },
            Self::Dim => quote! {
                <#ty as ::traceforge_rounds::Dim>::next(current.#ident)
                    .expect("round dimension overflow")
            },
        }
    }

    fn try_advance(&self, ty: &Type, ident: &Ident) -> proc_macro2::TokenStream {
        match self {
            Self::Counter => quote! { current.#ident.checked_add(1) },
            Self::Dim => quote! { <#ty as ::traceforge_rounds::Dim>::next(current.#ident) },
        }
    }
}

fn is_counter_type(ident: &Ident) -> bool {
    matches!(
        ident.to_string().as_str(),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
    )
}

fn to_pascal_case(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    let name = name.strip_prefix("r#").unwrap_or(name);

    for ch in name.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }

    out
}
