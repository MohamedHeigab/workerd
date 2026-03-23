// Copyright (c) 2026 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

//! Code generation for `#[jsg_resource]` on structs and impl blocks.
//!
//! - On a **struct**: emits `jsg::Type`, `jsg::ToJS`, `jsg::FromJS`, and
//!   `jsg::GarbageCollected` implementations.
//! - On an **impl block**: emits the `jsg::Resource` trait with method, static
//!   method, static constant, and constructor registrations.

use proc_macro::TokenStream;
use quote::quote;
use syn::FnArg;
use syn::ItemImpl;

use crate::trace::generate_trace_statements;
use crate::utils::error;
use crate::utils::extract_name_attribute;
use crate::utils::extract_named_fields;
use crate::utils::is_attr;
use crate::utils::is_lock_ref;
use crate::utils::snake_to_camel;

/// Entry point called from `lib.rs` for `#[jsg_resource]` on a struct.
pub(crate) fn generate_resource_struct(attr: TokenStream, input: syn::DeriveInput) -> TokenStream {
    let name: &syn::Ident = &input.ident;

    let class_name = if attr.is_empty() {
        name.to_string()
    } else {
        extract_name_attribute(attr).unwrap_or_else(|| name.to_string())
    };

    let fields = match extract_named_fields(&input, "jsg_resource") {
        Ok(fields) => fields,
        Err(err) => return err,
    };

    let trace_statements = generate_trace_statements(&fields);
    let name_str = name.to_string();

    let gc_impl = quote! {
        #[automatically_derived]
        impl jsg::GarbageCollected for #name {
            fn trace(&self, visitor: &mut jsg::GcVisitor) {
                // Suppress unused warning when there are no traceable fields.
                let _ = visitor;
                #(#trace_statements)*
            }

            fn memory_name(&self) -> &'static ::std::ffi::CStr {
                // from_bytes_with_nul on a concat!(name, "\0") literal is a
                // compile-time constant expression — the compiler folds the
                // unwrap and emits a direct pointer into the read-only data
                // segment. The C++ side constructs a kj::StringPtr directly
                // from data()+size() with no allocation.
                ::std::ffi::CStr::from_bytes_with_nul(concat!(#name_str, "\0").as_bytes())
                    .unwrap()
            }
        }
    };

    quote! {
        #input

        #[automatically_derived]
        impl jsg::Type for #name {
            fn class_name() -> &'static str { #class_name }

            fn is_exact(value: &jsg::v8::Local<jsg::v8::Value>) -> bool {
                value.is_object()
            }
        }

        #[automatically_derived]
        impl jsg::ToJS for #name {
            fn to_js<'a, 'b>(self, lock: &'a mut jsg::Lock) -> jsg::v8::Local<'b, jsg::v8::Value>
            where
                'b: 'a,
            {
                let r = jsg::Rc::new(self);
                r.to_js(lock)
            }
        }

        #[automatically_derived]
        impl jsg::FromJS for #name {
            type ResultType = jsg::Rc<Self>;

            fn from_js(
                lock: &mut jsg::Lock,
                value: jsg::v8::Local<jsg::v8::Value>,
            ) -> Result<Self::ResultType, jsg::Error> {
                <jsg::Rc<Self> as jsg::FromJS>::from_js(lock, value)
            }
        }

        #gc_impl
    }
    .into()
}

/// Entry point called from `lib.rs` for `#[jsg_resource]` on an impl block.
pub(crate) fn generate_resource_impl(impl_block: &ItemImpl) -> TokenStream {
    let self_ty = &impl_block.self_ty;

    if !matches!(&**self_ty, syn::Type::Path(_)) {
        return error(
            self_ty,
            "#[jsg_resource] impl blocks must use a simple path type (e.g., `impl MyResource`)",
        );
    }

    let method_registrations: Vec<_> = impl_block
        .items
        .iter()
        .filter_map(|item| {
            // Skip non-function items (e.g. type aliases, consts).
            let syn::ImplItem::Fn(method) = item else {
                return None;
            };

            // Only methods annotated with #[jsg_method] are registered.
            let attr = method.attrs.iter().find(|a| is_attr(a, "jsg_method"))?;

            let rust_method_name = &method.sig.ident;
            // Use explicit name from #[jsg_method(name = "...")] if provided,
            // otherwise convert snake_case to camelCase.
            let js_name = attr
                .meta
                .require_list()
                .ok()
                .map(|list| list.tokens.clone().into())
                .and_then(extract_name_attribute)
                .unwrap_or_else(|| snake_to_camel(&rust_method_name.to_string()));
            let callback_name = syn::Ident::new(
                &format!("{rust_method_name}_callback"),
                rust_method_name.span(),
            );

            // Methods with a receiver (&self, &mut self) become instance methods on the prototype.
            // Methods without a receiver become static methods on the constructor.
            let has_self = method
                .sig
                .inputs
                .iter()
                .any(|arg| matches!(arg, FnArg::Receiver(_)));

            let member = if has_self {
                quote! {
                    jsg::Member::Method {
                        name: #js_name.to_owned(),
                        callback: Self::#callback_name,
                    }
                }
            } else {
                quote! {
                    jsg::Member::StaticMethod {
                        name: #js_name.to_owned(),
                        callback: Self::#callback_name,
                    }
                }
            };
            Some(member)
        })
        .collect();

    let constant_registrations: Vec<_> = impl_block
        .items
        .iter()
        .filter_map(|item| {
            let syn::ImplItem::Const(constant) = item else {
                return None;
            };
            let attr = constant.attrs.iter().find(|a| {
                a.path().is_ident("jsg_static_constant")
                    || a.path()
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "jsg_static_constant")
            })?;

            let rust_name = &constant.ident;
            let js_name = attr
                .meta
                .require_list()
                .ok()
                .map(|list| list.tokens.clone().into())
                .and_then(extract_name_attribute)
                .unwrap_or_else(|| rust_name.to_string());

            Some(quote! {
                jsg::Member::StaticConstant {
                    name: #js_name.to_owned(),
                    value: jsg::ConstantValue::from(Self::#rust_name),
                }
            })
        })
        .collect();

    let constructor_registration = generate_constructor_registration(impl_block, self_ty);
    let constructor_vec: Vec<_> = constructor_registration.into_iter().collect();

    quote! {
        #impl_block

        #[automatically_derived]
        impl jsg::Resource for #self_ty {
            fn members() -> Vec<jsg::Member>
            where
                Self: Sized,
            {
                vec![
                    #(#constructor_vec,)*
                    #(#method_registrations,)*
                    #(#constant_registrations,)*
                ]
            }
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// Constructor helpers
// ---------------------------------------------------------------------------

/// Validates that a `#[jsg_constructor]` method has the right shape.
///
/// Returns a `compile_error!` token stream if the method has a `self` receiver
/// or does not return `Self`; returns `None` if the method is valid.
fn validate_constructor(method: &syn::ImplItemFn) -> Option<quote::__private::TokenStream> {
    let has_self = method
        .sig
        .inputs
        .iter()
        .any(|arg| matches!(arg, FnArg::Receiver(_)));
    if has_self {
        return Some(quote! {
            compile_error!("#[jsg_constructor] must be a static method (no self receiver)");
        });
    }

    let returns_self = matches!(&method.sig.output,
        syn::ReturnType::Type(_, ty) if matches!(&**ty,
            syn::Type::Path(p) if p.path.is_ident("Self")
        )
    );
    if !returns_self {
        return Some(quote! {
            compile_error!("#[jsg_constructor] must return Self");
        });
    }

    None
}

/// Extracts constructor argument unwrap statements and argument expressions.
fn extract_constructor_params(
    method: &syn::ImplItemFn,
) -> (
    bool,
    Vec<quote::__private::TokenStream>,
    Vec<quote::__private::TokenStream>,
) {
    let params: Vec<_> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                Some((*pat_type.ty).clone())
            } else {
                None
            }
        })
        .collect();

    let has_lock_param = params.first().is_some_and(is_lock_ref);
    let js_arg_offset = usize::from(has_lock_param);

    let (unwraps, arg_exprs) = params
        .iter()
        .enumerate()
        .skip(js_arg_offset)
        .map(|(i, ty)| {
            let js_index = i - js_arg_offset;
            let var = syn::Ident::new(&format!("arg{js_index}"), method.sig.ident.span());
            let unwrap = quote! {
                let #var = match <#ty as jsg::FromJS>::from_js(&mut lock, args.get(#js_index)) {
                    Ok(v) => v,
                    Err(e) => {
                        lock.throw_exception(&e);
                        return;
                    }
                };
            };
            (unwrap, quote! { #var })
        })
        .unzip();

    (has_lock_param, unwraps, arg_exprs)
}

/// Scans an impl block for a `#[jsg_constructor]` attribute and generates the
/// constructor callback registration. Returns `None` if no constructor is defined.
/// Validates that a `#[jsg_constructor]` method has the right shape and returns
/// a compile-error token stream if it doesn't.
fn generate_constructor_registration(
    impl_block: &ItemImpl,
    self_ty: &syn::Type,
) -> Option<quote::__private::TokenStream> {
    let constructors: Vec<_> = impl_block
        .items
        .iter()
        .filter_map(|item| match item {
            syn::ImplItem::Fn(m) if m.attrs.iter().any(|a| is_attr(a, "jsg_constructor")) => {
                Some(m)
            }
            _ => None,
        })
        .collect();

    if constructors.len() > 1 {
        return Some(quote! {
            compile_error!("only one #[jsg_constructor] is allowed per impl block");
        });
    }

    constructors
        .into_iter()
        .map(|method| {
            if let Some(err) = validate_constructor(method) {
                return err;
            }

            let rust_method_name = &method.sig.ident;
            let callback_name = syn::Ident::new(
                &format!("{rust_method_name}_constructor_callback"),
                rust_method_name.span(),
            );

            let (has_lock_param, unwraps, arg_exprs) = extract_constructor_params(method);
            let lock_arg = if has_lock_param {
                quote! { &mut lock, }
            } else {
                quote! {}
            };

            quote! {
                jsg::Member::Constructor {
                    callback: {
                        unsafe extern "C" fn #callback_name(
                            info: *mut jsg::v8::ffi::FunctionCallbackInfo,
                        ) {
                            let mut lock = unsafe { jsg::Lock::from_args(info) };
                            jsg::catch_panic(&mut lock, || {
                                // SAFETY: info is a valid V8 FunctionCallbackInfo from the constructor call.
                                let mut args = unsafe { jsg::v8::FunctionCallbackInfo::from_ffi(info) };
                                let mut lock = unsafe { jsg::Lock::from_args(info) };

                                #(#unwraps)*

                                let resource = #self_ty::#rust_method_name(#lock_arg #(#arg_exprs),*);
                                let rc = jsg::Rc::new(resource);
                                rc.attach_to_this(&mut args);
                            });
                        }
                        #callback_name
                    },
                }
            }
        })
        .next()
}

#[cfg(test)]
mod tests {
    use syn::parse_quote;

    use super::*;

    #[test]
    fn validate_constructor_valid() {
        // A valid constructor: static (no self), returns Self.
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor(name: String) -> Self { todo!() }
        };
        assert!(validate_constructor(&method).is_none());
    }

    #[test]
    fn validate_constructor_rejects_self_receiver() {
        // Instance method — must not have &self.
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor(&self) -> Self { todo!() }
        };
        assert!(validate_constructor(&method).is_some());
    }

    #[test]
    fn validate_constructor_rejects_non_self_return() {
        // Returns String, not Self.
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor() -> String { todo!() }
        };
        assert!(validate_constructor(&method).is_some());
    }

    #[test]
    fn extract_constructor_params_no_lock() {
        // Plain constructor — no Lock param, two JS args.
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor(name: String, value: u32) -> Self { todo!() }
        };
        let (has_lock, unwraps, arg_exprs) = extract_constructor_params(&method);
        assert!(!has_lock);
        assert_eq!(unwraps.len(), 2);
        assert_eq!(arg_exprs.len(), 2);
    }

    #[test]
    fn extract_constructor_params_with_lock() {
        // First param is `&mut jsg::Lock` — skipped from JS args.
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor(lock: &mut jsg::Lock, name: String) -> Self { todo!() }
        };
        let (has_lock, unwraps, arg_exprs) = extract_constructor_params(&method);
        assert!(has_lock);
        // Only one JS arg (name); lock is not counted.
        assert_eq!(unwraps.len(), 1);
        assert_eq!(arg_exprs.len(), 1);
    }

    #[test]
    fn extract_constructor_params_no_args() {
        let method: syn::ImplItemFn = parse_quote! {
            fn constructor() -> Self { todo!() }
        };
        let (has_lock, unwraps, arg_exprs) = extract_constructor_params(&method);
        assert!(!has_lock);
        assert!(unwraps.is_empty());
        assert!(arg_exprs.is_empty());
    }
}
