//! Generate guest ABI adapters from a single typed behavior function.
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Expr, FnArg, Ident, ItemFn, Lit, MetaNameValue, ReturnType, Token, Type, parse::Parse,
    parse::ParseStream, parse_macro_input,
};

struct Args {
    module: Ident,
    name: Option<String>,
    level: Ident,
    leaf: bool,
}
impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let module = input.parse()?;
        let mut name = None;
        let mut level = None;
        let mut leaf = false;
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            if input.peek(Ident) && input.fork().parse::<Ident>()? == "leaf" {
                input.parse::<Ident>()?;
                if leaf {
                    return Err(input.error("duplicate leaf"));
                }
                leaf = true;
                continue;
            }
            let option: MetaNameValue = input.parse()?;
            if option.path.is_ident("name") {
                if name.is_some() {
                    return Err(syn::Error::new_spanned(option, "duplicate name"));
                }
                if let Expr::Lit(expr) = &option.value {
                    if let Lit::Str(value) = &expr.lit {
                        name = Some(value.value());
                        continue;
                    }
                }
                return Err(syn::Error::new_spanned(
                    option.value,
                    "name must be an export-name string",
                ));
            } else if option.path.is_ident("level") {
                if level.is_some() {
                    return Err(syn::Error::new_spanned(option, "duplicate level"));
                }
                if let Expr::Path(expr) = &option.value {
                    if let Some(ident) = expr.path.get_ident() {
                        level = Some(ident.clone());
                        continue;
                    }
                }
                return Err(syn::Error::new_spanned(
                    option.value,
                    "level must be implement, state or safe_fail",
                ));
            } else {
                return Err(syn::Error::new_spanned(option.path, "unknown win32 option"));
            }
        }
        Ok(Self {
            module,
            name,
            leaf,
            level: level.ok_or_else(|| {
                syn::Error::new(Span::call_site(), "implementation level is required")
            })?,
        })
    }
}

fn kind(name: &str) -> proc_macro2::TokenStream {
    let variant = Ident::new(
        match name {
            "void" => "Void",
            "u32" => "U32",
            "i32" => "I32",
            "u64" => "U64",
            "i64" => "I64",
            "f32" => "F32",
            "f64" => "F64",
            "ptr" => "Pointer",
            "handle" => "Handle",
            "str8" => "AnsiString",
            "str16" => "WideString",
            "aggregate" => "Aggregate",
            _ => "Unsupported",
        },
        Span::call_site(),
    );
    quote!(::adv_abi::AbiKind::#variant)
}

fn expand(args: Args, function: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let sig = &function.sig;
    if sig.asyncness.is_some()
        || sig.unsafety.is_some()
        || sig.abi.is_some()
        || sig.variadic.is_some()
        || !sig.generics.params.is_empty()
    {
        return Err(syn::Error::new_spanned(
            sig,
            "win32 behavior functions must be safe, synchronous and non-generic",
        ));
    }
    let level = Ident::new(
        match args.level.to_string().as_str() {
            "implement" => "Implement",
            "state" => "State",
            "safe_fail" => "SafeFail",
            _ => {
                return Err(syn::Error::new_spanned(
                    args.level,
                    "level must be implement, state or safe_fail",
                ));
            }
        },
        args.level.span(),
    );
    let module = format!("{}.dll", args.module.to_string().to_ascii_lowercase());
    let name = args.name.unwrap_or_else(|| sig.ident.to_string());
    let fields = if let Some((interface, method)) = name.split_once("::") {
        include_str!("../../../data/abi/com.tsv").lines().filter(|l| !l.starts_with('#')).find_map(|line| {
            let f: Vec<_> = line.split('\t').collect();
            (f.len() == 6 && f[0] == interface && f[2] == method).then(|| vec![module.as_str(), f[2], "stdcall", f[3], f[4], f[5]])
        })
    } else {
        include_str!("../../../data/abi/signatures.tsv").lines().filter(|l| !l.starts_with('#')).find_map(|line| {
            let f: Vec<_> = line.split('\t').collect();
            (f.len() == 6 && f[0] == module && f[1] == name).then_some(f)
        })
    }.ok_or_else(|| syn::Error::new_spanned(&sig.ident, format!("{module}!{name} is absent from pinned x86 Win32Metadata; add an authoritative source rather than guessing a signature")))?;
    let metadata: Vec<_> = if fields[5].is_empty() {
        vec![]
    } else {
        fields[5]
            .split('|')
            .map(|p| p.split(':').collect::<Vec<_>>())
            .collect()
    };
    let first = sig
        .inputs
        .first()
        .ok_or_else(|| syn::Error::new_spanned(sig, "first parameter must be &mut Env"))?;
    if !matches!(first, FnArg::Typed(p) if matches!(p.ty.as_ref(), Type::Reference(r) if r.mutability.is_some()))
    {
        return Err(syn::Error::new_spanned(
            first,
            "first parameter must be &mut Env (or an Env alias)",
        ));
    }
    if sig.inputs.len() - 1 != metadata.len() {
        return Err(syn::Error::new_spanned(
            sig,
            format!(
                "metadata has {} parameters, behavior has {}",
                metadata.len(),
                sig.inputs.len() - 1
            ),
        ));
    }
    if matches!(fields[3], "unsupported" | "aggregate")
        || metadata.iter().any(|p| p[2] == "unsupported")
    {
        return Err(syn::Error::new_spanned(
            sig,
            "this metadata signature needs an unsupported x86 ABI projection",
        ));
    }
    let mut checks = Vec::new();
    let mut decode = Vec::new();
    let mut leaf_decode = Vec::new();
    let mut names = Vec::new();
    let mut stack_words: u16 = 0;
    let mut selectors = Vec::new();
    for (i, (parameter, expected)) in sig.inputs.iter().skip(1).zip(metadata).enumerate() {
        let FnArg::Typed(parameter) = parameter else {
            return Err(syn::Error::new_spanned(
                parameter,
                "methods are unsupported",
            ));
        };
        let ty = &parameter.ty;
        let expected_kind = kind(expected[2]);
        let words: u8 = expected[3]
            .parse()
            .map_err(|_| syn::Error::new_spanned(ty, "invalid generated stack width"))?;
        if words == 1
            && matches!(expected[2], "u32" | "i32")
            && matches!(
                expected[0],
                "State"
                    | "Type"
                    | "Format"
                    | "CheckFormat"
                    | "Usage"
                    | "Pool"
                    | "Stage"
                    | "Sampler"
                    | "DeviceType"
            )
        {
            let selector = expected[0];
            let word = stack_words as u8;
            selectors.push(quote!((#selector, #word)));
        }
        stack_words += u16::from(words);
        let parameter_name = format_ident!("__win32_arg_{i}");
        let message = format!(
            "{module}!{name} parameter {} has incompatible guest type/stack width",
            i + 1
        );
        checks.push(quote! {
            assert!(#expected_kind.accepts(<#ty as ::adv_abi::GuestArg>::KIND), #message);
            assert!(<#ty as ::adv_abi::GuestArg>::WORDS == #words, #message);
        });
        decode.push(quote! { let #parameter_name: #ty = match __win32_arguments.next() { Ok(value) => value, Err(fault) => return ::adv_abi::guest::memory_failure(fault) }; });
        leaf_decode.push(quote! { let #parameter_name: #ty = __win32_arguments.next().ok()?; });
        names.push(parameter_name);
    }
    let stack_words = u8::try_from(stack_words)
        .map_err(|_| syn::Error::new_spanned(sig, "x86 signature exceeds thunk stack capacity"))?;
    let conv = match fields[2] {
        "stdcall" => quote!(::adv_abi::Conv::Stdcall { argc: #stack_words }),
        "cdecl" => quote!(::adv_abi::Conv::Cdecl { argc: #stack_words }),
        _ => {
            return Err(syn::Error::new_spanned(
                sig,
                "variadic or unsupported convention requires a dedicated ABI adapter",
            ));
        }
    };
    let return_type: Type = match &sig.output {
        ReturnType::Default => syn::parse_quote!(()),
        ReturnType::Type(_, ty) => *ty.clone(),
    };
    let return_kind = kind(fields[3]);
    let ident = &sig.ident;
    let adapter = format_ident!("{ident}__win32_adapter");
    let descriptor = format_ident!("{ident}_ABI");
    let visibility = &function.vis;
    let return_message = format!("{module}!{name} has an incompatible guest return type");
    let invoke = quote!(::adv_abi::GuestReturn::into_outcome(#ident(__win32_env, #(#names),*)));
    let invoke = if module == "d3d9.dll" && fields[4] == "Windows.Win32.Foundation.HRESULT" {
        let api = format!("{module}!{name}");
        quote! {
            ::adv_abi::capability::begin(__win32_env, #api, #stack_words, &[#(#selectors),*]);
            let __win32_outcome = #invoke;
            ::adv_abi::capability::finish(__win32_env, __win32_outcome)
        }
    } else {
        invoke
    };
    let leaf_adapter = format_ident!("{ident}__env_leaf_adapter");
    let (leaf_function, leaf_binding) = if args.leaf {
        (
            quote! {
                const _: () = assert!(!matches!(<#return_type as ::adv_abi::GuestReturn>::KIND,
                    ::adv_abi::AbiKind::Outcome | ::adv_abi::AbiKind::F32 | ::adv_abi::AbiKind::F64),
                    "leaf behavior must return a synchronous integer value, not Outcome");
                #[allow(non_snake_case)]
                fn #leaf_adapter(__win32_env: &mut ::adv_abi::Env<'_>) -> Option<u64> {
                    let mut __win32_arguments = ::adv_abi::Arguments::new(__win32_env.cpu);
                    #(#leaf_decode)*
                    let __win32_outcome = { #invoke };
                    match __win32_outcome {
                        ::adv_abi::Outcome::Return(value) => Some((u64::from(::adv_abi::leaf_edx(__win32_env)) << 32) | u64::from(value)),
                        ::adv_abi::Outcome::Return64(value) => Some(value),
                        _ => unreachable!("synchronous leaf return"),
                    }
                }
            },
            quote!(Some(#leaf_adapter as ::adv_abi::EnvLeafHandler)),
        )
    } else {
        (quote!(), quote!(None))
    };
    Ok(quote! {
        #function
        #leaf_function
        const _: () = {
            #(#checks)*
            assert!(matches!(<#return_type as ::adv_abi::GuestReturn>::KIND, ::adv_abi::AbiKind::Outcome)
                || #return_kind.accepts(<#return_type as ::adv_abi::GuestReturn>::KIND), #return_message);
        };
        #[allow(non_snake_case)]
        fn #adapter(__win32_env: &mut ::adv_abi::Env<'_>) -> ::adv_abi::Outcome {
            let mut __win32_arguments = ::adv_abi::Arguments::new(__win32_env.cpu);
            #(#decode)*
            #invoke
        }
        #[allow(non_upper_case_globals)]
        #visibility const #descriptor: ::adv_abi::Api = ::adv_abi::Api {
            module: #module, name: #name, conv: #conv, handler: #adapter, env_leaf: #leaf_binding,
            level: ::adv_abi::ImplementationLevel::#level,
        };
    })
}

#[proc_macro_attribute]
pub fn win32(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as Args);
    let function = parse_macro_input!(item as ItemFn);
    expand(args, function)
        .unwrap_or_else(|error| error.into_compile_error())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_missing_signatures_and_argument_counts() {
        let args =
            syn::parse_str::<Args>("kernel32, name=\"NoSuchExport\", level=implement").unwrap();
        let function = syn::parse_str::<ItemFn>("fn fake(cx: &mut Env) -> u32 { 0 }").unwrap();
        assert!(
            expand(args, function)
                .unwrap_err()
                .to_string()
                .contains("absent")
        );
        let args = syn::parse_str::<Args>("kernel32, name=\"Sleep\", level=implement").unwrap();
        let function = syn::parse_str::<ItemFn>("fn fake(cx: &mut Env) -> u32 { 0 }").unwrap();
        assert!(
            expand(args, function)
                .unwrap_err()
                .to_string()
                .contains("metadata has 1")
        );
    }
}
