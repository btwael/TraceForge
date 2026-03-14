extern crate proc_macro;

use convert_case::{Case, Casing};
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream, Result};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::token::{Comma, PathSep};
use syn::{DeriveInput, Generics, PathSegment, TypePath};

mod comm_close;
mod new_comm_close;

struct MsgTypes {
    types: Vec<MsgVariant>,
}

// name is the name of the struct type (defining the monitor) and mtype is the type of the messages
// the monitor is observing
struct MsgVariant {
    name: Ident,     // FOO or M2
    mtype: TypePath, // Foo or M2
}

// this is to define types of the form
// pub enum MyMonitorMsg (the monitor name written with upper case letters) {
//    Foo(Foo),
//    M2(M2),
//    Terminate
//}
// where MyMonitor is the name of the monitor, Foo and M2 are the types of the messages observed by the monitor
impl MsgTypes {
    fn enum_stream(&self, name: &Ident) -> TokenStream {
        let vars = self.types.iter().map(|t| {
            let MsgVariant { name, mtype } = t;
            quote! {
                #name(#mtype),
            }
        });

        quote! {
            #[derive(Clone, Debug, PartialEq)]
            pub enum #name {
                #(#vars)*
                Terminate
            }
        }
    }
}

// this seems to be about parsing something and transforming to a MsgTypes structure
impl Parse for MsgTypes {
    fn parse(input: ParseStream) -> Result<Self> {
        let vars = Punctuated::<TypePath, Comma>::parse_terminated(input)?;

        Ok(MsgTypes {
            types: vars
                .into_iter()
                .map(|t| MsgVariant {
                    name: get_name(&t.path.segments),
                    mtype: t,
                })
                .collect::<Vec<_>>(),
        })
    }
}

// this is used only in the "parse" function above
fn get_name(segments: &Punctuated<PathSegment, PathSep>) -> Ident {
    let vname = segments
        .iter()
        .map(|seg| {
            let ident = format!("{}", seg.ident);
            ident
                .split('_')
                .map(|s| {
                    let mut s = s.to_string();
                    if let Some(c) = s.get_mut(0..1) {
                        c.make_ascii_uppercase();
                    }
                    s
                })
                .collect::<String>()
        })
        .collect::<String>();
    syn::Ident::new(&vname, segments.span())
}

// this seems to be related to the name of the macro "#[monitor(M1, M2, M3)]"
#[proc_macro_attribute]
pub fn monitor(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let i = input.clone();
    let ast = syn::parse_macro_input!(i as DeriveInput);

    let name = format!("{}Msg", ast.ident);
    let name = syn::Ident::new(&name, ast.ident.span());

    // this seems to be related to taking the inputs in the paranthesis of the macro definition and transforming them to a MsgTypes
    let types = syn::parse_macro_input!(attr as MsgTypes);

    let menum = types.enum_stream(&name);
    let intos = intos(&name, &types);
    let rec = monitor_code(&ast.ident, &ast.generics, &name, &types);

    let input: TokenStream = input.into();
    let gen = quote! {
        #input

        #menum
        #intos

        #rec
    };

    gen.into()
}

fn intos(name: &Ident, types: &MsgTypes) -> TokenStream {
    let intos = types
        .types
        .iter()
        .map(|t| impl_into(name, &t.name, &t.mtype));
    quote! {
        #(#intos)*
    }
}

fn monitor_code(aname: &Ident, gen: &Generics, name: &Ident, types: &MsgTypes) -> TokenStream {
    let (impl_generics, ty_generics, where_clause) = gen.split_for_impl();

    let vars = types.types.iter().map(|t| {
        let vname = &t.name;
        let tname = &t.mtype;
        quote! {
            #name::#vname(msg) => (self as &mut dyn ::traceforge::monitor_types::Observer<#tname>).notify(who, whom, msg),
        }
    });

    let start_monitor_fnname = format!("start_monitor_{}", aname).to_case(Case::Snake);
    let start_monitor_fnname = syn::Ident::new(&start_monitor_fnname, aname.span());

    let create_msg_for_monitor_fnname =
        format!("create_msg_for_monitor_{}", aname).to_case(Case::Snake);
    let create_msg_for_monitor_fnname =
        syn::Ident::new(&create_msg_for_monitor_fnname, aname.span());

    let accept_msg_for_monitor_fnname =
        format!("accept_msg_for_monitor_{}", aname).to_case(Case::Snake);
    let accept_msg_for_monitor_fnname =
        syn::Ident::new(&accept_msg_for_monitor_fnname, aname.span());

    let terminate_monitor_fnname = format!("terminate_monitor_{}", aname).to_case(Case::Snake);
    let terminate_monitor_fnname = syn::Ident::new(&terminate_monitor_fnname, aname.span());

    let wrappings = types.types.iter().map(|t| {
        let tname = &t.mtype;
        quote! {
            let m = msg.clone();
            if let Ok(msg) = m.as_any().downcast::<(#tname)>() {
                let msg = *msg;
                let m: #name = <#tname as Into<#name>>::into(msg);
                return Some(traceforge::Val::new((Some(who),Some(whom),m)));
            }
        }
    });

    let acceptors = types.types.iter().map(|t| {
        let tname = &t.mtype;
        quote! {
            let m = msg.clone();
            if let Ok(msg) = m.as_any().downcast::<(#tname)>() {
                let msg = *msg;
                let mut mon = #aname::default();
                return (&mut mon as &mut dyn Acceptor<#tname>).accept(who, whom, &msg);
            }
        }
    });

    quote! {
        impl #impl_generics ::traceforge::monitor_types::Observer<#name> for #aname #ty_generics #where_clause {
            fn notify(&mut self,
                        who: ::traceforge::thread::ThreadId,
                        whom: ::traceforge::thread::ThreadId,
                        msg: &#name,
                        ) -> ::traceforge::monitor_types::MonitorResult {
                match msg {
                    #(#vars)*
                    #name::Terminate => Ok(()),
                }
            }
        }

        pub fn #start_monitor_fnname(m: #aname) -> ::traceforge::thread::JoinHandle<::traceforge::monitor_types::MonitorResult> {
            let cloned = m.clone();
            let mon1 = std::sync::Arc::new(std::sync::Mutex::new(cloned));
            let mon2 = mon1.clone();
            let jh = ::traceforge::spawn_monitor(move || {
                loop {
                    let (who, whom, msg): (Option<::traceforge::thread::ThreadId>, Option<::traceforge::thread::ThreadId>, #name) = traceforge::recv_msg_block();
                    let unwrapped = &mut (*mon1.lock().expect("Failed to lock mon1"));
                    if let #name::Terminate = msg {
                        let res = traceforge::invoke_on_stop(unwrapped);
                        return res;
                    }
                    // here I call the handler of that message
                    let observer = unwrapped as &mut dyn ::traceforge::monitor_types::Observer<#name>;
                    let res = observer.notify(who.unwrap(), whom.unwrap(), &msg);
                    if let Err(e) = res {
                        println!(" {e:?} is the error returned");
                        traceforge::assert(false);
                    }
                }
            },
                #create_msg_for_monitor_fnname as fn(::traceforge::thread::ThreadId,::traceforge::thread::ThreadId,traceforge::Val) -> Option<traceforge::Val>,
                #accept_msg_for_monitor_fnname as fn(::traceforge::thread::ThreadId,::traceforge::thread::ThreadId,traceforge::Val) -> bool,
                mon2);
            return jh;
        }

        pub fn #create_msg_for_monitor_fnname(who: ::traceforge::thread::ThreadId, whom: ::traceforge::thread::ThreadId, msg: traceforge::Val) -> Option<(traceforge::Val)> { //#name
            #(#wrappings)*

            return None;
        }

        pub fn #accept_msg_for_monitor_fnname(who: ::traceforge::thread::ThreadId, whom: ::traceforge::thread::ThreadId, msg: traceforge::Val) -> bool { //#name
            #(#acceptors)*

            return false;
        }

        pub fn #terminate_monitor_fnname(t: ::traceforge::thread::ThreadId) {
            let who: Option<::traceforge::thread::ThreadId> = None;
            let whom: Option<::traceforge::thread::ThreadId> = None;
            traceforge::send_msg(t, (who, whom, #name::Terminate));
        }
    }
}

fn impl_into(name: &Ident, vname: &Ident, ty: &TypePath) -> TokenStream {
    quote! {
        impl Into<#name> for #ty {
            fn into(self) -> #name {
                #name::#vname(self)
            }
        }
    }
}

#[proc_macro_derive(RoundKey)]
pub fn round_key_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    comm_close::round_key_derive(input)
}

#[proc_macro_derive(RoundEnum)]
pub fn round_enum_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    comm_close::round_enum_derive(input)
}

#[proc_macro_derive(DimensionEnum)]
pub fn dimension_enum_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    new_comm_close::dimension_enum_derive(input)
}

#[proc_macro_derive(Round, attributes(dimension))]
pub fn round_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    new_comm_close::round_derive(input)
}
