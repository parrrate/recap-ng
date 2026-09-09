use std::convert::identity;

use darling::{FromDeriveInput, util::SpannedValue};
use indexmap::IndexMap;
use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use regex_syntax::hir::{Capture, Hir, HirKind, Look};
use syn::{DeriveInput, Ident, parse_macro_input};

#[derive(FromDeriveInput)]
#[darling(attributes(recap))]
struct Args {
    regex: SpannedValue<String>,
}

#[derive(Clone)]
enum SimpleFragment {
    Start,
    End,
    Literal(String),
    Capture(String),
}

fn to_fragments(hir: &Hir) -> Result<Vec<SimpleFragment>, String> {
    match hir.kind() {
        HirKind::Empty => Err("empty regex isn't supported".into()),
        HirKind::Literal(literal) => String::from_utf8(literal.0.as_ref().into())
            .map(|s| vec![SimpleFragment::Literal(s)])
            .map_err(|e| e.to_string()),
        HirKind::Class(_) => Err("classes (outside captures) aren't supported".into()),
        HirKind::Look(Look::Start) => Ok(vec![SimpleFragment::Start]),
        HirKind::Look(Look::End) => Ok(vec![SimpleFragment::End]),
        HirKind::Look(_) => Ok(vec![]),
        HirKind::Repetition(_) => Err("repetitions (outside captures) aren't supported".into()),
        HirKind::Capture(Capture {
            name: Some(name), ..
        }) => Ok(vec![SimpleFragment::Capture(name.to_string())]),
        HirKind::Capture(_) => Err("nameless captures aren't supported".into()),
        HirKind::Concat(hirs) => hirs
            .iter()
            .map(to_fragments)
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.concat()),
        HirKind::Alternation(_) => {
            Err("alternations (outside captures) aren't supported (yet)".into())
        }
    }
}

enum InnerFragments {
    Literal(String),
    Capture(String),
}

fn inner_fragments(fragments: Vec<SimpleFragment>) -> Result<Vec<InnerFragments>, String> {
    let mut start = false;
    let mut end = false;
    let mut result = Vec::new();
    for fragment in fragments {
        if end {
            return Err("fragments after $".into());
        }
        match fragment {
            SimpleFragment::Start if start => return Err("duplicate ^".into()),
            SimpleFragment::Start => start = true,
            _ if !start => return Err("no ^ at the start".into()),
            SimpleFragment::End => end = true,
            SimpleFragment::Literal(s) => result.push(InnerFragments::Literal(s)),
            SimpleFragment::Capture(s) => result.push(InnerFragments::Capture(s)),
        }
    }
    if !end { Err("no $".into()) } else { Ok(result) }
}

struct Fragments {
    captures: IndexMap<String, (String, Ident)>,
    tail: String,
}

impl Fragments {
    fn new(
        fragments: Vec<InnerFragments>,
        span: Span,
        idents: &IndexMap<String, &Ident>,
    ) -> Result<Self, syn::Error> {
        let mut tail = String::new();
        let mut captures = IndexMap::new();
        for fragment in fragments {
            match fragment {
                InnerFragments::Literal(literal) => tail.push_str(&literal),
                InnerFragments::Capture(name) => {
                    let Some(&ident) = idents.get(name.as_str()) else {
                        return Err(syn::Error::new(span, format!("field {name} not found")));
                    };
                    if captures.contains_key(name.as_str()) {
                        return Err(syn::Error::new(
                            ident.span(),
                            "field appears twice in captures",
                        ));
                    }
                    captures.insert(name, (std::mem::take(&mut tail), ident.clone()));
                }
            }
        }
        Ok(Self { captures, tail })
    }

    fn make_from_str(&self) -> TokenStream {
        let fields = self.captures.iter().map(|(key, (_, ident))| {
            quote_spanned! { ident.span() =>
                #ident: __captures[#key].parse().map_err(|e| ::recap_ng::Error::field(
                    #key,
                    e,
                ))?
            }
        });
        quote! {
            #(#fields),*
        }
    }

    fn make_display(&self) -> TokenStream {
        let fields = self.captures.iter().map(|(_, (head, ident))| {
            quote_spanned! { ident.span() =>
                __f.write_str(#head)?;
                ::core::fmt::Display::fmt(&self.#ident, __f)?;
            }
        });
        let tail = &self.tail;
        quote! {
            #(#fields)*
            __f.write_str(#tail)?;
        }
    }
}

fn recap_impl(input: DeriveInput) -> Result<TokenStream, TokenStream> {
    let Args { regex } = Args::from_derive_input(&input).map_err(|e| e.write_errors())?;
    let hir = regex_syntax::parse(&regex)
        .map_err(|e| syn::Error::new(regex.span(), e.to_string()).into_compile_error())?;
    let data = match input.data {
        syn::Data::Struct(data) => data,
        syn::Data::Enum(data) => {
            return Err(syn::Error::new_spanned(
                data.enum_token,
                "`enum`s are not supported (yet)",
            )
            .into_compile_error());
        }
        syn::Data::Union(data) => {
            return Err(
                syn::Error::new_spanned(data.union_token, "`union`s are not supported")
                    .into_compile_error(),
            );
        }
    };
    let fields = match data.fields {
        syn::Fields::Named(fields) => fields,
        syn::Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                data.semi_token,
                "tuple `struct`s are not supported (yet)",
            )
            .into_compile_error());
        }
        syn::Fields::Unit => {
            return Err(syn::Error::new_spanned(
                data.semi_token,
                "unit `struct`s are not supported, use `MustBe` instead",
            )
            .into_compile_error());
        }
    };
    let idents = fields
        .named
        .iter()
        .map(|field| field.ident.as_ref().unwrap())
        .map(|ident| (ident.to_string(), ident))
        .collect();
    let fragments =
        to_fragments(&hir).map_err(|e| syn::Error::new(regex.span(), e).into_compile_error())?;
    let fragments = inner_fragments(fragments)
        .map_err(|e| syn::Error::new(regex.span(), e).into_compile_error())?;
    let fragments =
        Fragments::new(fragments, regex.span(), &idents).map_err(|e| e.into_compile_error())?;
    let missing_captures = idents
        .iter()
        .filter(|(k, _)| !fragments.captures.contains_key(*k))
        .map(|(_, v)| syn::Error::new(v.span(), "field not in captures").into_compile_error())
        .collect::<Vec<_>>();
    let name = input.ident;
    let (i, t, w) = input.generics.split_for_impl();
    let regex = regex.as_str();
    let from_str = fragments.make_from_str();
    let display = fragments.make_display();
    let schema_name = name.to_string();
    let schemars = if cfg!(feature = "schemars") {
        quote! {
            impl #i ::recap_ng::schemars::JsonSchema for #name #t #w {
                fn schema_name() -> ::std::borrow::Cow<'static, str> {
                    #schema_name.into()
                }

                fn json_schema(generator: &mut ::recap_ng::schemars::SchemaGenerator)
                    -> ::recap_ng::schemars::Schema
                {
                    let mut schema: ::recap_ng::schemars::Schema = <
                        ::std::string::String as ::recap_ng::schemars::JsonSchema
                    >::json_schema(generator);
                    schema.ensure_object().insert("pattern".into(), #regex.into());
                    schema
                }
            }
        }
    } else {
        quote! {}
    };
    let ts = quote! {
        const _: () = {
            static __REGEX: ::std::sync::LazyLock<::recap_ng::regex::Regex> =
                ::std::sync::LazyLock::new(|| ::recap_ng::regex::Regex::new(#regex).unwrap());
            impl #i ::core::str::FromStr for #name #t #w {
                type Err = ::recap_ng::Error;

                fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                    let __captures = __REGEX.captures(s).ok_or(::recap_ng::Error::NoMatch)?;
                    Ok(Self { #from_str })
                }
            }
            impl #i ::core::fmt::Display for #name #t #w {
                fn fmt(&self, __f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    #display;
                    Ok(())
                }
            }
            impl<'__de> #i ::recap_ng::serde::Deserialize<'__de> for #name #t #w {
                fn deserialize<__D>(deserializer: __D) -> ::core::result::Result<Self, __D::Error>
                where
                    __D: ::recap_ng::serde::Deserializer<'__de>,
                {
                    <::std::string::String as ::recap_ng::serde::Deserialize<'__de>>::deserialize(
                        deserializer,
                    )?.parse().map_err(::recap_ng::serde::de::Error::custom)
                }
            }
            impl #i ::recap_ng::serde::Serialize for #name #t #w {
                fn serialize<__S>(&self, serializer: __S)
                    -> ::core::result::Result<__S::Ok, __S::Error>
                where
                    __S: ::recap_ng::serde::Serializer,
                {
                    ::std::string::ToString::to_string(self).serialize(serializer)
                }
            }
            #schemars

            #(#missing_captures)*
        };
    };
    Ok(ts)
}

#[proc_macro_derive(Recap, attributes(recap))]
pub fn recap(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    recap_impl(input).unwrap_or_else(identity).into()
}
