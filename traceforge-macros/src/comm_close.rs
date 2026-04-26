use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, LitStr, Meta, Type};

pub fn dimension_enum_derive(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let variants = match &ast.data {
        Data::Enum(data) => &data.variants,
        _ => {
            return syn::Error::new(ast.span(), "DimensionEnum can only be derived for enums")
                .to_compile_error()
                .into();
        }
    };
    if variants.is_empty() {
        return syn::Error::new(ast.span(), "DimensionEnum requires at least one variant")
            .to_compile_error()
            .into();
    }

    let mut to_u32_arms = Vec::new();
    let mut from_u32_arms = Vec::new();
    for (idx, var) in variants.iter().enumerate() {
        if !matches!(var.fields, Fields::Unit) {
            return syn::Error::new(var.span(), "DimensionEnum requires fieldless enum variants")
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
        impl ::traceforge::comm_close::DimensionEnum for #name {
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

pub fn round_derive(input: TokenStream) -> TokenStream {
    let ast = syn::parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let access_trait = syn::Ident::new(&format!("{}Access", name), name.span());
    let stamp_access_trait = syn::Ident::new(&format!("{}StampAccess", name), name.span());
    let filter_access_trait = syn::Ident::new(&format!("{}FilterAccess", name), name.span());
    let advance_to_trait = syn::Ident::new(&format!("{}AdvanceTo", name), name.span());

    if !ast.generics.params.is_empty() {
        return syn::Error::new(
            ast.generics.span(),
            "Round can only be derived for non-generic structs",
        )
        .to_compile_error()
        .into();
    }

    let fields = match &ast.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            _ => {
                return syn::Error::new(
                    data.fields.span(),
                    "Round requires a struct with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new(ast.span(), "Round can only be derived for structs")
                .to_compile_error()
                .into();
        }
    };

    let mut dimensions = Vec::new();
    let mut marker_constructors = Vec::new();
    let mut accessor_signatures = Vec::new();
    let mut accessor_impls = Vec::new();
    let mut stamp_accessor_impls = Vec::new();
    let mut filter_signatures = Vec::new();
    let mut filter_impls = Vec::new();
    let mut advance_to_signatures = Vec::new();
    let mut advance_to_impls = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        let field_ident = field.ident.as_ref().expect("named fields");
        let field_name = field_ident.to_string();
        let advance_to_name = format_ident!("advance_{}_to", field_ident);
        let match_kind = match parse_dimension_match(field) {
            Ok(kind) => kind,
            Err(err) => return err.to_compile_error().into(),
        };
        let match_tokens = match match_kind {
            ParsedMatchKind::Eq => quote!(::traceforge::comm_close::MatchKind::Eq),
            ParsedMatchKind::Gt => quote!(::traceforge::comm_close::MatchKind::Gt),
            ParsedMatchKind::Gte => quote!(::traceforge::comm_close::MatchKind::Gte),
            ParsedMatchKind::Any => quote!(::traceforge::comm_close::MatchKind::Any),
        };

        let dimension = match &field.ty {
            Type::Path(ty) if is_u32(ty) => {
                quote! {
                    ::traceforge::comm_close::DimensionSpec::u32(#field_name, #match_tokens)
                }
            }
            Type::Path(ty) => {
                quote! {
                    ::traceforge::comm_close::DimensionSpec::enumeration::<#ty>(#field_name, #match_tokens)
                }
            }
            other => {
                return syn::Error::new(
                    other.span(),
                    "Round fields must be either `u32` or a path type implementing DimensionEnum",
                )
                .to_compile_error()
                .into();
            }
        };
        dimensions.push(dimension);
        marker_constructors.push(quote! {
            pub fn #field_ident() -> ::traceforge::comm_close::Dimension<Self> {
                ::traceforge::comm_close::Dimension::new(#index)
            }
        });

        let accessor_signature = match &field.ty {
            Type::Path(ty) if is_u32(ty) => {
                quote! {
                    fn #field_ident(&self) -> u32;
                }
            }
            Type::Path(ty) => {
                quote! {
                    fn #field_ident(&self) -> #ty;
                }
            }
            _ => unreachable!(),
        };
        accessor_signatures.push(accessor_signature);

        let accessor_impl = match &field.ty {
            Type::Path(ty) if is_u32(ty) => {
                quote! {
                    fn #field_ident(&self) -> u32 {
                        self.component(#name::#field_ident())
                    }
                }
            }
            Type::Path(ty) => {
                quote! {
                    fn #field_ident(&self) -> #ty {
                        let raw = self.component(#name::#field_ident());
                        <#ty as ::traceforge::comm_close::DimensionEnum>::try_from_u32(raw)
                            .unwrap_or_else(|| {
                                panic!(
                                    "invalid value {} for dimension {}",
                                    raw,
                                    #field_name
                                )
                            })
                    }
                }
            }
            _ => unreachable!(),
        };
        accessor_impls.push(accessor_impl);
        let stamp_accessor_impl = match &field.ty {
            Type::Path(ty) if is_u32(ty) => {
                quote! {
                    fn #field_ident(&self) -> u32 {
                        self.components()[#index]
                    }
                }
            }
            Type::Path(ty) => {
                quote! {
                    fn #field_ident(&self) -> #ty {
                        let raw = self.components()[#index];
                        <#ty as ::traceforge::comm_close::DimensionEnum>::try_from_u32(raw)
                            .unwrap_or_else(|| {
                                panic!(
                                    "invalid value {} for dimension {}",
                                    raw,
                                    #field_name
                                )
                            })
                    }
                }
            }
            _ => unreachable!(),
        };
        stamp_accessor_impls.push(stamp_accessor_impl);

        filter_signatures.push(quote! {
            fn #field_ident<C>(self, cmp: C) -> ::traceforge::comm_close::RoundFilter<#name>
            where
                C: ::core::convert::Into<::traceforge::comm_close::MatchKind>;
        });
        filter_impls.push(quote! {
            fn #field_ident<C>(self, cmp: C) -> ::traceforge::comm_close::RoundFilter<#name>
            where
                C: ::core::convert::Into<::traceforge::comm_close::MatchKind>,
            {
                let kind: ::traceforge::comm_close::MatchKind = cmp.into();
                self.with_match(#name::#field_ident(), kind)
            }
        });

        match &field.ty {
            Type::Path(ty) if is_u32(ty) => {
                advance_to_signatures.push(quote! {
                    fn #advance_to_name(&mut self, target: u32);
                });
                advance_to_impls.push(quote! {
                    fn #advance_to_name(&mut self, target: u32) {
                        self.advance_to(#name::#field_ident(), target);
                    }
                });
            }
            Type::Path(ty) => {
                advance_to_signatures.push(quote! {
                    fn #advance_to_name(&mut self, target: #ty);
                });
                advance_to_impls.push(quote! {
                    fn #advance_to_name(&mut self, target: #ty) {
                        self.advance_to(
                            #name::#field_ident(),
                            <#ty as ::traceforge::comm_close::DimensionEnum>::to_u32(target),
                        );
                    }
                });
            }
            _ => unreachable!(),
        }
    }

    let gen = quote! {
        impl ::traceforge::comm_close::RoundDescriptor for #name {
            fn scheme() -> ::traceforge::comm_close::Scheme {
                ::traceforge::comm_close::Scheme::from_dimensions(vec![
                    #(#dimensions),*
                ])
            }
        }

        impl #name {
            #(#marker_constructors)*
        }

        trait #access_trait {
            #(#accessor_signatures)*
        }

        impl #access_trait for ::traceforge::comm_close::Round<#name> {
            #(#accessor_impls)*
        }

        trait #stamp_access_trait {
            #(#accessor_signatures)*
        }

        impl #stamp_access_trait for ::traceforge::comm_close::RoundStamp<#name> {
            #(#stamp_accessor_impls)*
        }

        trait #filter_access_trait {
            #(#filter_signatures)*
        }

        impl #filter_access_trait for ::traceforge::comm_close::RoundFilter<#name> {
            #(#filter_impls)*
        }

        trait #advance_to_trait {
            #(#advance_to_signatures)*
        }

        impl #advance_to_trait for ::traceforge::comm_close::Rounds<#name> {
            #(#advance_to_impls)*
        }
    };

    gen.into()
}

#[derive(Clone, Copy)]
enum ParsedMatchKind {
    Eq,
    Gt,
    Gte,
    Any,
}

fn parse_dimension_match(field: &syn::Field) -> syn::Result<ParsedMatchKind> {
    let mut found = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("dimension") {
            continue;
        }
        if found.is_some() {
            return Err(syn::Error::new(
                attr.span(),
                "duplicate #[dimension(...)] attribute",
            ));
        }
        found = Some(parse_dimension_attr(attr)?);
    }
    Ok(found.unwrap_or(ParsedMatchKind::Any))
}

fn parse_dimension_attr(attr: &syn::Attribute) -> syn::Result<ParsedMatchKind> {
    match &attr.meta {
        Meta::Path(_) => Ok(ParsedMatchKind::Any),
        Meta::List(list) if list.tokens.is_empty() => Ok(ParsedMatchKind::Any),
        Meta::List(_) => {
            if let Ok(value) = attr.parse_args::<LitStr>() {
                return parse_match_value(&value);
            }

            let mut match_kind = None;
            attr.parse_nested_meta(|meta| parse_dimension_nested(meta, &mut match_kind))?;
            Ok(match_kind.unwrap_or(ParsedMatchKind::Any))
        }
        Meta::NameValue(meta) => Err(syn::Error::new(
            meta.span(),
            "expected #[dimension], #[dimension(\"...\")], or #[dimension(match = \"...\")]",
        )),
    }
}

fn parse_dimension_nested(
    meta: ParseNestedMeta,
    match_kind: &mut Option<ParsedMatchKind>,
) -> syn::Result<()> {
    if meta.path.is_ident("match") {
        if match_kind.is_some() {
            return Err(meta.error("duplicate `match` in #[dimension(...)]"));
        }
        let value: LitStr = meta.value()?.parse()?;
        *match_kind = Some(parse_match_value(&value)?);
        return Ok(());
    }

    Err(meta.error("unsupported #[dimension(...)] option"))
}

fn parse_match_value(value: &LitStr) -> syn::Result<ParsedMatchKind> {
    match value.value().as_str() {
        "=" => Ok(ParsedMatchKind::Eq),
        ">" => Ok(ParsedMatchKind::Gt),
        ">=" => Ok(ParsedMatchKind::Gte),
        "*" => Ok(ParsedMatchKind::Any),
        _ => Err(syn::Error::new(
            value.span(),
            "match kind must be one of \"=\", \">\", \">=\", or \"*\"",
        )),
    }
}

fn is_u32(ty: &syn::TypePath) -> bool {
    ty.qself.is_none()
        && ty.path.leading_colon.is_none()
        && ty.path.segments.len() == 1
        && ty.path.segments[0].ident == "u32"
}
