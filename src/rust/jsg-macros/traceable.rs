// Copyright (c) 2026 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

//! Code generation for `#[jsg_traceable]` on enums and plain structs.
//!
//! `#[jsg_traceable]` generates a `jsg::GarbageCollected` implementation whose
//! `trace()` body visits every GC-visible field — both on plain structs (the
//! `PrivateData` delegation pattern) and on enums (the `kj::OneOf` state-machine
//! pattern).
//!
//! The same field classifiers used by `#[jsg_resource]` are reused here, so
//! every field shape that works in a resource struct also works in a
//! `#[jsg_traceable]` type.
//!
//! # Enum tracing
//!
//! For each variant the macro emits one `match` arm. Within the arm, fields are
//! classified using the same cascade as `generate_trace_statements`:
//!
//! - `#[jsg_trace]`-annotated field → `jsg::GarbageCollected::trace(field, visitor)`
//! - `jsg::Rc<T>` → `visitor.visit_rc(field)`
//! - `jsg::v8::Global<T>` → `visitor.visit_global(field)`
//! - `Option<jsg::Rc<T>>` / `Nullable<jsg::Rc<T>>` → if-let
//! - `Vec<jsg::Rc<T>>`, `HashMap<K, jsg::Rc<T>>`, etc. → for-loop
//! - `Cell<T>` for any of the above → `unsafe { &*ptr }` read
//! - Variants with no traceable fields → `_variant => {}` arm (no-op)
//!
//! # Plain struct tracing
//!
//! Behaves identically to `generate_trace_statements`: each named field is
//! classified and a trace call is emitted. Useful for `PrivateData`-style helper
//! structs so you don't need to write `GarbageCollected` by hand.
//!
//! # `memory_name`
//!
//! The default `memory_name()` returns the type name as a `CStr` literal.
//! Override with `#[jsg_traceable(name = "CustomName")]`.

use proc_macro::TokenStream;
use quote::quote;
use syn::Data;
use syn::DeriveInput;
use syn::Fields;
use syn::spanned::Spanned;

use crate::trace::OptionalKind;
use crate::trace::TraceableType;
use crate::trace::extract_cell_inner;
use crate::trace::extract_collection_traceable;
use crate::trace::extract_optional_inner;
use crate::trace::generate_collection_trace_loop;
use crate::trace::generate_trace_statements;
use crate::trace::get_traceable_type;
use crate::trace::has_jsg_trace_attr;
use crate::utils::error;
use crate::utils::extract_name_attribute;

/// Strips `#[jsg_trace]` attributes from all fields in a `DeriveInput` — both
/// named struct/variant fields and tuple variant fields.
///
/// `#[jsg_trace]` is a jsg-macros-internal marker consumed during trace
/// generation. It must not appear in the emitted struct/enum definition.
fn strip_jsg_trace_attrs(input: &mut DeriveInput) {
    match &mut input.data {
        Data::Struct(data) => {
            if let Fields::Named(fields) = &mut data.fields {
                for field in &mut fields.named {
                    field.attrs.retain(|a| !a.path().is_ident("jsg_trace"));
                }
            }
        }
        Data::Enum(data) => {
            for variant in &mut data.variants {
                match &mut variant.fields {
                    Fields::Named(fields) => {
                        for field in &mut fields.named {
                            field.attrs.retain(|a| !a.path().is_ident("jsg_trace"));
                        }
                    }
                    Fields::Unnamed(fields) => {
                        for field in &mut fields.unnamed {
                            field.attrs.retain(|a| !a.path().is_ident("jsg_trace"));
                        }
                    }
                    Fields::Unit => {}
                }
            }
        }
        Data::Union(_) => {}
    }
}

/// Entry point called from `lib.rs` for `#[jsg_traceable]`.
pub fn generate_traceable(attr: TokenStream, mut input: DeriveInput) -> TokenStream {
    let memory_name = extract_name_attribute(attr).unwrap_or_else(|| input.ident.to_string());
    // Clone name before mutating input.
    let name = input.ident.clone();
    let name_str = memory_name;

    let trace_body = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => {
                let stmts = generate_trace_statements(&fields.named);
                quote! {
                    let _ = visitor;
                    #(#stmts)*
                }
            }
            Fields::Unit => quote! { let _ = visitor; },
            Fields::Unnamed(_) => {
                return error(&input, "#[jsg_traceable] does not support tuple structs");
            }
        },
        Data::Enum(data) => {
            let arms = generate_enum_trace_arms(data);
            quote! {
                let _ = visitor;
                match self {
                    #(#arms,)*
                }
            }
        }
        Data::Union(_) => {
            return error(&input, "#[jsg_traceable] cannot be applied to unions");
        }
    };

    // Strip `#[jsg_trace]` from all field definitions before emitting the type.
    // `#[jsg_trace]` is a jsg-macros-internal marker; leaving it in the output
    // would cause the compiler to reject it as an unrecognised attribute.
    strip_jsg_trace_attrs(&mut input);

    quote! {
        #input

        #[automatically_derived]
        impl jsg::GarbageCollected for #name {
            fn trace(&self, visitor: &mut jsg::GcVisitor) {
                #trace_body
            }

            fn memory_name(&self) -> &'static ::std::ffi::CStr {
                // from_bytes_with_nul on a concat!(name, "\0") literal is a
                // compile-time constant expression — the compiler folds the
                // unwrap and emits a direct pointer into the read-only data
                // segment, with no allocation.
                ::std::ffi::CStr::from_bytes_with_nul(concat!(#name_str, "\0").as_bytes())
                    .unwrap()
            }
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// Enum arm generation
// ---------------------------------------------------------------------------

/// Generates one `match` arm per enum variant, tracing every GC-visible field
/// within the arm's pattern.
fn generate_enum_trace_arms(data: &syn::DataEnum) -> Vec<quote::__private::TokenStream> {
    data.variants.iter().map(generate_variant_arm).collect()
}

/// Generates a single `match` arm for one enum variant.
///
/// - **Unit variants** (`Closed`) — emits `Self::Closed => {}` (no-op).
/// - **Named-field variants** (`Errored { reason, .. }`) — binds each
///   traceable field by name and traces it.
/// - **Tuple variants** (`Readable(inner)`) — binds each field by a generated
///   positional name (`_f0`, `_f1`, …) and traces it.
fn generate_variant_arm(variant: &syn::Variant) -> quote::__private::TokenStream {
    let variant_name = &variant.ident;

    match &variant.fields {
        // Unit variant — nothing to trace.
        Fields::Unit => quote! {
            Self::#variant_name => {}
        },

        // Named-field variant: `Errored { reason, count, .. }`
        Fields::Named(named) => {
            let (bindings, trace_stmts) = trace_named_fields(named);
            if trace_stmts.is_empty() {
                // No traceable fields — bind with `..` to silence unused-variable warnings.
                quote! {
                    Self::#variant_name { .. } => {}
                }
            } else {
                quote! {
                    Self::#variant_name { #(#bindings,)* .. } => {
                        #(#trace_stmts)*
                    }
                }
            }
        }

        // Tuple variant: `Readable(ReadableImpl)` or `Value(jsg::Rc<T>)`
        Fields::Unnamed(unnamed) => {
            let (bindings, trace_stmts) = trace_tuple_fields(unnamed);
            if trace_stmts.is_empty() {
                // No traceable fields — suppress unused-variable warnings.
                quote! {
                    Self::#variant_name(..) => {}
                }
            } else {
                quote! {
                    Self::#variant_name(#(#bindings),*) => {
                        #(#trace_stmts)*
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Named-field variant helpers
// ---------------------------------------------------------------------------

/// Returns `(field_bindings, trace_statements)` for a named-field variant.
///
/// Only fields that need tracing are included in the bindings; everything else
/// is captured by the trailing `..` in the match arm.
///
/// Because `trace` takes `&self`, match ergonomics give each binding a shared
/// reference automatically — no `ref` keyword is needed or allowed (Rust 2024).
fn trace_named_fields(
    named: &syn::FieldsNamed,
) -> (
    Vec<quote::__private::TokenStream>,
    Vec<quote::__private::TokenStream>,
) {
    let mut bindings = Vec::new();
    let mut stmts = Vec::new();

    for field in &named.named {
        let Some(field_name) = field.ident.as_ref() else {
            continue;
        };
        let ty = &field.ty;

        // #[jsg_trace] — delegate to GarbageCollected::trace.
        if has_jsg_trace_attr(field) {
            bindings.push(quote! { #field_name });
            stmts.push(quote! {
                jsg::GarbageCollected::trace(#field_name, visitor);
            });
            continue;
        }

        if let Some(stmt) = classify_field_for_variant(ty, &quote! { #field_name }) {
            bindings.push(quote! { #field_name });
            stmts.push(stmt);
        }
    }

    (bindings, stmts)
}

// ---------------------------------------------------------------------------
// Tuple-field variant helpers
// ---------------------------------------------------------------------------

/// Returns `(field_bindings, trace_statements)` for a tuple variant.
///
/// Traceable fields are bound as `_f0`, `_f1`, … Non-traceable fields use `_`
/// to suppress unused-variable warnings. Because `trace` takes `&self`, match
/// ergonomics give each named binding a shared reference automatically —
/// no `ref` keyword is needed or allowed (Rust 2024).
fn trace_tuple_fields(
    unnamed: &syn::FieldsUnnamed,
) -> (
    Vec<quote::__private::TokenStream>,
    Vec<quote::__private::TokenStream>,
) {
    let mut bindings = Vec::new();
    let mut stmts = Vec::new();

    for (i, field) in unnamed.unnamed.iter().enumerate() {
        let ty = &field.ty;
        // Positional binding name: `_f0`, `_f1`, …  The `_` prefix means the
        // compiler won't warn if the binding is unused (untraceable field).
        let var = syn::Ident::new(&format!("_f{i}"), field.ty.span());

        // #[jsg_trace] — delegate to GarbageCollected::trace.
        if has_jsg_trace_attr(field) {
            bindings.push(quote! { #var });
            stmts.push(quote! {
                jsg::GarbageCollected::trace(#var, visitor);
            });
            continue;
        }

        if let Some(stmt) = classify_field_for_variant(ty, &quote! { #var }) {
            bindings.push(quote! { #var });
            stmts.push(stmt);
        } else {
            // Not traceable — bind with `_` to mark intentionally unused.
            bindings.push(quote! { _ });
        }
    }

    (bindings, stmts)
}

// ---------------------------------------------------------------------------
// Shared field classifier for variant arms
// ---------------------------------------------------------------------------

/// Classifies a single field type and returns the trace statement for use
/// inside a `match` arm, or `None` if the field is not traceable.
///
/// `binding` is the token stream for the bound variable (e.g. `quote! { reason }`
/// for a named field or `quote! { _f0 }` for a tuple field). Because `trace`
/// takes `&self`, match ergonomics make each binding a `&FieldType` automatically;
/// callers must not add an extra `&`.
fn classify_field_for_variant(
    ty: &syn::Type,
    binding: &quote::__private::TokenStream,
) -> Option<quote::__private::TokenStream> {
    // Cell<T> — read through pointer with SAFETY invariant.
    // (Rare inside enum variants but supported for completeness.)
    if let Some(cell_inner_ty) = extract_cell_inner(ty) {
        return classify_cell_field_for_variant(cell_inner_ty, binding);
    }

    // Option<Traceable> / Nullable<Traceable>
    if let Some((kind, inner_ty)) = extract_optional_inner(ty) {
        let pattern = match kind {
            OptionalKind::Option => quote! { Some(ref __inner) },
            OptionalKind::Nullable => quote! { jsg::Nullable::Some(ref __inner) },
        };
        return match get_traceable_type(inner_ty) {
            TraceableType::Ref => Some(quote! {
                if let #pattern = #binding {
                    visitor.visit_rc(__inner);
                }
            }),
            TraceableType::Global => Some(quote! {
                if let #pattern = #binding {
                    visitor.visit_global(__inner);
                }
            }),
            TraceableType::Weak | TraceableType::None => None,
        };
    }

    // Vec<Traceable>, HashMap<K, Traceable>, etc.
    if let Some((style, traceable)) = extract_collection_traceable(ty) {
        // `binding` is already a `&T` borrow from the `ref` pattern — use it directly.
        return Some(generate_collection_trace_loop(binding, &style, traceable));
    }

    // Bare jsg::Rc<T> / jsg::v8::Global<T>
    match get_traceable_type(ty) {
        TraceableType::Ref => Some(quote! {
            visitor.visit_rc(#binding);
        }),
        TraceableType::Global => Some(quote! {
            visitor.visit_global(#binding);
        }),
        TraceableType::Weak | TraceableType::None => None,
    }
}

/// Classifies a `Cell<T>` field inside a variant arm.
///
/// Because the binding is a shared reference from the match pattern (`ref x`),
/// we use `Cell::as_ptr()` to read through it — the same soundness argument as
/// for struct fields applies: GC tracing is single-threaded and never re-entrant.
fn classify_cell_field_for_variant(
    cell_inner_ty: &syn::Type,
    binding: &quote::__private::TokenStream,
) -> Option<quote::__private::TokenStream> {
    match get_traceable_type(cell_inner_ty) {
        TraceableType::Ref => {
            return Some(quote! {
                // SAFETY: trace() is single-threaded and never re-entrant.
                unsafe { visitor.visit_rc(&*#binding.as_ptr()); }
            });
        }
        TraceableType::Global => {
            return Some(quote! {
                // SAFETY: Cell::as_ptr() dereference is sound — tracing is
                // single-threaded and never re-entrant on the same object.
                unsafe { visitor.visit_global(&*#binding.as_ptr()); }
            });
        }
        TraceableType::Weak | TraceableType::None => {}
    }

    if let Some((kind, inner_ty)) = extract_optional_inner(cell_inner_ty) {
        let pattern = match kind {
            OptionalKind::Option => quote! { Some(ref __inner) },
            OptionalKind::Nullable => quote! { jsg::Nullable::Some(ref __inner) },
        };
        return match get_traceable_type(inner_ty) {
            TraceableType::Ref => Some(quote! {
                // SAFETY: trace() is single-threaded and never re-entrant.
                if let #pattern = unsafe { &*#binding.as_ptr() } {
                    visitor.visit_rc(__inner);
                }
            }),
            TraceableType::Global => Some(quote! {
                // SAFETY: Cell::as_ptr() dereference is sound — tracing is
                // single-threaded and never re-entrant on the same object.
                if let #pattern = unsafe { &*#binding.as_ptr() } {
                    visitor.visit_global(__inner);
                }
            }),
            TraceableType::Weak | TraceableType::None => None,
        };
    }

    if let Some((style, traceable)) = extract_collection_traceable(cell_inner_ty) {
        let accessor = quote! { unsafe { &*#binding.as_ptr() } };
        return Some(generate_collection_trace_loop(&accessor, &style, traceable));
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use syn::parse_quote;

    use super::*;

    /// Helper: parse a variant and return its generated arm as a string for
    /// inspection. We only test the *structure* of the output (contains the
    /// expected calls), not exact formatting.
    fn arm_str(variant: &syn::Variant) -> String {
        generate_variant_arm(variant).to_string()
    }

    #[test]
    fn unit_variant_emits_empty_arm() {
        let variant: syn::Variant = parse_quote! { Closed };
        let s = arm_str(&variant);
        // Must match the variant and produce an empty body.
        assert!(s.contains("Closed"), "missing variant name: {s}");
        assert!(
            !s.contains("visit"),
            "unit variant must not visit anything: {s}"
        );
    }

    #[test]
    fn named_variant_rc_field_emits_visit_rc() {
        let variant: syn::Variant = parse_quote! {
            Errored { reason: jsg::Rc<ErrorReason> }
        };
        let s = arm_str(&variant);
        assert!(s.contains("visit_rc"), "must call visit_rc: {s}");
        assert!(s.contains("reason"), "must bind reason: {s}");
    }

    #[test]
    fn named_variant_global_field_emits_visit_global() {
        let variant: syn::Variant = parse_quote! {
            WithCallback { callback: jsg::v8::Global<jsg::v8::Value> }
        };
        let s = arm_str(&variant);
        assert!(s.contains("visit_global"), "must call visit_global: {s}");
        assert!(s.contains("callback"), "must bind callback: {s}");
    }

    #[test]
    fn named_variant_no_traceable_fields_emits_wildcard() {
        let variant: syn::Variant = parse_quote! {
            Config { name: String, count: u32 }
        };
        let s = arm_str(&variant);
        assert!(!s.contains("visit"), "no visit calls expected: {s}");
        // Must use `..` to ignore fields.
        assert!(
            s.contains(".."),
            "must use .. for non-traceable fields: {s}"
        );
    }

    #[test]
    fn tuple_variant_rc_field_emits_visit_rc() {
        let variant: syn::Variant = parse_quote! {
            Readable(jsg::Rc<ReadableImpl>)
        };
        let s = arm_str(&variant);
        assert!(s.contains("visit_rc"), "must call visit_rc: {s}");
    }

    #[test]
    fn tuple_variant_mixed_fields_traces_only_rc() {
        // First field is a plain String (not traceable), second is jsg::Rc<T>.
        let variant: syn::Variant = parse_quote! {
            Mixed(String, jsg::Rc<Foo>)
        };
        let s = arm_str(&variant);
        assert!(s.contains("visit_rc"), "must trace the Rc field: {s}");
        // The non-traceable String must be bound with `_` (not named).
        // The arm must not emit a visit call for the String.
        let visit_count = s.matches("visit_").count();
        assert_eq!(visit_count, 1, "exactly one visit call expected: {s}");
    }

    #[test]
    fn tuple_variant_no_traceable_fields_emits_wildcard() {
        let variant: syn::Variant = parse_quote! {
            Closed(u32)
        };
        let s = arm_str(&variant);
        assert!(!s.contains("visit"), "no visit calls expected: {s}");
        assert!(s.contains(".."), "must use .. for non-traceable tuple: {s}");
    }

    #[test]
    fn named_variant_option_rc_emits_if_let() {
        let variant: syn::Variant = parse_quote! {
            Pending { maybe_child: Option<jsg::Rc<Child>> }
        };
        let s = arm_str(&variant);
        assert!(s.contains("if let"), "must emit if-let for Option<Rc>: {s}");
        assert!(s.contains("visit_rc"), "must call visit_rc: {s}");
    }

    #[test]
    fn named_variant_vec_rc_emits_for_loop() {
        let variant: syn::Variant = parse_quote! {
            Buffered { items: Vec<jsg::Rc<Item>> }
        };
        let s = arm_str(&variant);
        assert!(s.contains("for"), "must emit for-loop for Vec<Rc>: {s}");
        assert!(s.contains("visit_rc"), "must call visit_rc: {s}");
    }

    #[test]
    fn named_variant_trace_attr_emits_delegate() {
        let variant: syn::Variant = parse_quote! {
            Active { #[jsg_trace] state: InnerState }
        };
        let s = arm_str(&variant);
        assert!(
            s.contains("GarbageCollected"),
            "must delegate via GarbageCollected::trace: {s}"
        );
        assert!(s.contains("state"), "must bind state field: {s}");
    }
}
