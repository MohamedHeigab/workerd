// Copyright (c) 2026 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

//! GC trace code generation for `#[jsg_resource]` structs.
//!
//! Analyses struct fields and emits `GarbageCollected::trace` body statements
//! that call `visitor.visit_rc` / `visitor.visit_global` for every field whose
//! type is — or contains — a traceable JSG handle.
//!
//! # Supported field shapes
//!
//! | Field type                            | Emitted trace call                         |
//! |---------------------------------------|--------------------------------------------|
//! | `jsg::Rc<T>`                          | `visitor.visit_rc(&self.f)`                |
//! | `jsg::v8::Global<T>`                  | `visitor.visit_global(&self.f)`            |
//! | `Option<jsg::Rc<T>>`                  | if-let + `visitor.visit_rc`                |
//! | `Nullable<jsg::Rc<T>>`               | if-let + `visitor.visit_rc`                |
//! | `Option<jsg::v8::Global<T>>`          | if-let + `visitor.visit_global`            |
//! | `Nullable<jsg::v8::Global<T>>`        | if-let + `visitor.visit_global`            |
//! | `Vec<jsg::Rc<T>>`                     | for-loop + `visitor.visit_rc`              |
//! | `HashMap<K, jsg::Rc<T>>`             | for-loop on `.values()` + `visit_rc`       |
//! | `BTreeMap<K, jsg::Rc<T>>`            | for-loop on `.values()` + `visit_rc`       |
//! | `HashSet<jsg::Rc<T>>`                | for-loop + `visitor.visit_rc`              |
//! | `BTreeSet<jsg::Rc<T>>`               | for-loop + `visitor.visit_rc`              |
//! | Same collection shapes with `Global` | same, using `visit_global`                 |
//! | `Cell<T>` for any of the above        | same via `unsafe { &*self.f.as_ptr() }`    |
//! | `jsg::Weak<T>`                        | nothing — weak refs are not traced         |

use syn::Type;

// ---------------------------------------------------------------------------
// Core classification types
// ---------------------------------------------------------------------------

/// Classification of a JSG type that participates (or not) in GC tracing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TraceableType {
    /// `jsg::Rc<T>` — strong GC edge; visited via `GcVisitor::visit_rc`.
    Ref,
    /// `jsg::Weak<T>` — weak reference, not traced (doesn't keep the target alive).
    Weak,
    /// `jsg::v8::Global<T>` — JS value strong/traced dual-mode handle;
    /// visited via `GcVisitor::visit_global`.
    Global,
    /// Not a traceable type.
    None,
}

/// Whether the wrapping optional type is `Option` or `jsg::Nullable`.
pub(crate) enum OptionalKind {
    Option,
    Nullable,
}

/// How to iterate over the traceable values in a collection field.
pub(crate) enum CollectionIterStyle {
    /// `Vec<T>`, `HashSet<T>`, `BTreeSet<T>` — iterate elements directly.
    IterElements,
    /// `HashMap<K, V>`, `BTreeMap<K, V>` — iterate `.values()`.
    IterValues,
}

// ---------------------------------------------------------------------------
// Type classification helpers
// ---------------------------------------------------------------------------

/// Returns the `TraceableType` for `jsg::Rc<T>`, `jsg::Weak<T>`, or
/// `jsg::v8::Global<T>`. Returns `None` for everything else.
pub(crate) fn get_traceable_type(ty: &Type) -> TraceableType {
    let Type::Path(type_path) = ty else {
        return TraceableType::None;
    };
    let segments = &type_path.path.segments;

    // `jsg::Rc<T>` or `jsg::Weak<T>` — exactly 2 segments.
    if segments.len() == 2 && segments[0].ident == "jsg" {
        match segments[1].ident.to_string().as_str() {
            "Rc" => return TraceableType::Ref,
            "Weak" => return TraceableType::Weak,
            _ => {}
        }
    }

    // `jsg::v8::Global<T>` — exactly 3 segments.
    if segments.len() == 3
        && segments[0].ident == "jsg"
        && segments[1].ident == "v8"
        && segments[2].ident == "Global"
    {
        return TraceableType::Global;
    }

    TraceableType::None
}

/// Extracts the inner type from `Option<T>` or `Nullable<T>` if present.
pub(crate) fn extract_optional_inner(ty: &Type) -> Option<(OptionalKind, &Type)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    let kind = match segment.ident.to_string().as_str() {
        "Option" => OptionalKind::Option,
        "Nullable" => OptionalKind::Nullable,
        _ => return None,
    };
    Some((kind, inner))
}

/// Extracts the inner type `T` from `Cell<T>` or `std::cell::Cell<T>`.
pub(crate) fn extract_cell_inner(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segments = &type_path.path.segments;

    let cell_seg = if segments.len() == 1 && segments[0].ident == "Cell" {
        &segments[0]
    } else if segments.len() == 3
        && segments[0].ident == "std"
        && segments[1].ident == "cell"
        && segments[2].ident == "Cell"
    {
        &segments[2]
    } else {
        return None;
    };

    let syn::PathArguments::AngleBracketed(args) = &cell_seg.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    Some(inner)
}

/// If `ty` is one of the supported collection wrappers (`Vec`, `HashMap`,
/// `BTreeMap`, `HashSet`, `BTreeSet`) and its value/element type argument is a
/// traceable JSG handle, returns the iteration style and the traceable kind.
///
/// Accepts both unqualified names (`Vec<T>`) and path-qualified forms
/// (`std::collections::HashMap<K, V>`, `std::vec::Vec<T>`, etc.) by matching
/// only on the **last** path segment.
pub(crate) fn extract_collection_traceable(
    ty: &Type,
) -> Option<(CollectionIterStyle, TraceableType)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let last = type_path.path.segments.last()?;
    let name = last.ident.to_string();

    // element collections use arg 0; map collections use arg 1 (the value type).
    let (style, value_arg_index): (CollectionIterStyle, usize) = match name.as_str() {
        "Vec" | "HashSet" | "BTreeSet" => (CollectionIterStyle::IterElements, 0),
        "HashMap" | "BTreeMap" => (CollectionIterStyle::IterValues, 1),
        _ => return None,
    };

    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };

    let value_ty = args
        .args
        .iter()
        .filter_map(|a| {
            if let syn::GenericArgument::Type(t) = a {
                Some(t)
            } else {
                None
            }
        })
        .nth(value_arg_index)?;

    let traceable = get_traceable_type(value_ty);
    if matches!(traceable, TraceableType::None | TraceableType::Weak) {
        return None;
    }
    Some((style, traceable))
}

// ---------------------------------------------------------------------------
// Code generation helpers
// ---------------------------------------------------------------------------

/// Emits a for-loop that visits every traceable element/value in a collection.
///
/// `accessor` is the expression that borrows the collection:
/// - direct field: `quote! { &self.#field_name }`
/// - `Cell<…>` field: `quote! { unsafe { &*self.#field_name.as_ptr() } }`
pub(crate) fn generate_collection_trace_loop(
    accessor: &quote::__private::TokenStream,
    style: &CollectionIterStyle,
    traceable: TraceableType,
) -> quote::__private::TokenStream {
    use quote::quote;
    let iter_expr = match style {
        CollectionIterStyle::IterElements => quote! { #accessor },
        CollectionIterStyle::IterValues => quote! { (#accessor).values() },
    };
    match traceable {
        TraceableType::Ref => quote! {
            for __item in #iter_expr {
                visitor.visit_rc(__item);
            }
        },
        TraceableType::Global => quote! {
            for __item in #iter_expr {
                visitor.visit_global(__item);
            }
        },
        TraceableType::Weak | TraceableType::None => quote! {},
    }
}

/// Generates a trace statement for a field that is wrapped in `Cell<T>`.
///
/// Because `GarbageCollected::trace` receives `&self`, we use `Cell::as_ptr` to
/// read through the cell without requiring `T: Copy`.  This is sound because V8
/// GC tracing is always single-threaded within an isolate and is never re-entrant
/// on the same object during a single GC cycle.
pub(crate) fn generate_cell_trace_statement(
    field_name: &syn::Ident,
    cell_inner_ty: &Type,
) -> Option<quote::__private::TokenStream> {
    use quote::quote;

    // Cell<jsg::Rc<T>> / Cell<jsg::v8::Global<T>>
    match get_traceable_type(cell_inner_ty) {
        TraceableType::Ref => {
            return Some(quote! {
                // SAFETY: trace() is single-threaded and never re-entrant.
                unsafe { visitor.visit_rc(&*self.#field_name.as_ptr()); }
            });
        }
        TraceableType::Global => {
            return Some(quote! {
                // SAFETY: Cell::as_ptr() dereference is sound because GC
                // tracing is single-threaded and never re-entrant on the same object.
                unsafe { visitor.visit_global(&*self.#field_name.as_ptr()); }
            });
        }
        TraceableType::Weak | TraceableType::None => {}
    }

    // Cell<Option<jsg::Rc<T>>> / Cell<Nullable<jsg::Rc<T>>> etc.
    if let Some((kind, inner_ty)) = extract_optional_inner(cell_inner_ty) {
        let pattern = match kind {
            OptionalKind::Option => quote! { Some(inner) },
            OptionalKind::Nullable => quote! { jsg::Nullable::Some(inner) },
        };
        match get_traceable_type(inner_ty) {
            TraceableType::Ref => {
                return Some(quote! {
                    // SAFETY: trace() is single-threaded and never re-entrant.
                    if let #pattern = unsafe { &*self.#field_name.as_ptr() } {
                        visitor.visit_rc(inner);
                    }
                });
            }
            TraceableType::Global => {
                return Some(quote! {
                    // SAFETY: Cell::as_ptr() dereference is sound because GC
                    // tracing is single-threaded and never re-entrant on the same object.
                    if let #pattern = unsafe { &*self.#field_name.as_ptr() } {
                        visitor.visit_global(inner);
                    }
                });
            }
            TraceableType::Weak | TraceableType::None => {}
        }
    }

    // Cell<Vec<jsg::Rc<T>>>, Cell<HashMap<K, jsg::Rc<T>>>, etc.
    if let Some((style, traceable)) = extract_collection_traceable(cell_inner_ty) {
        let accessor = quote! { unsafe { &*self.#field_name.as_ptr() } };
        let loop_body = generate_collection_trace_loop(&accessor, &style, traceable);
        return Some(quote! {
            // SAFETY: Cell::as_ptr() dereference is sound because GC tracing is
            // single-threaded and never re-entrant on the same object.
            #loop_body
        });
    }

    None
}

/// Generates all trace statements for the fields of a `#[jsg_resource]` struct.
pub(crate) fn generate_trace_statements(
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> Vec<quote::__private::TokenStream> {
    use quote::quote;

    fields
        .iter()
        .filter_map(|field| {
            let field_name = field.ident.as_ref()?;
            let ty = &field.ty;

            // Cell<T> — read through pointer with SAFETY invariant.
            if let Some(cell_inner_ty) = extract_cell_inner(ty) {
                return generate_cell_trace_statement(field_name, cell_inner_ty);
            }

            // Option<Traceable> / Nullable<Traceable>
            if let Some((kind, inner_ty)) = extract_optional_inner(ty) {
                let pattern = match kind {
                    OptionalKind::Option => quote! { Some(ref inner) },
                    OptionalKind::Nullable => quote! { jsg::Nullable::Some(ref inner) },
                };
                match get_traceable_type(inner_ty) {
                    TraceableType::Ref => {
                        return Some(quote! {
                            if let #pattern = self.#field_name {
                                visitor.visit_rc(inner);
                            }
                        });
                    }
                    TraceableType::Global => {
                        return Some(quote! {
                            if let #pattern = self.#field_name {
                                visitor.visit_global(inner);
                            }
                        });
                    }
                    TraceableType::Weak | TraceableType::None => {}
                }
            }

            // Vec<Traceable>, HashMap<K, Traceable>, etc.
            if let Some((style, traceable)) = extract_collection_traceable(ty) {
                let accessor = quote! { &self.#field_name };
                return Some(generate_collection_trace_loop(&accessor, &style, traceable));
            }

            // Bare jsg::Rc<T> / jsg::v8::Global<T>
            match get_traceable_type(ty) {
                TraceableType::Ref => Some(quote! {
                    visitor.visit_rc(&self.#field_name);
                }),
                TraceableType::Global => Some(quote! {
                    visitor.visit_global(&self.#field_name);
                }),
                TraceableType::Weak | TraceableType::None => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use syn::parse_quote;

    use super::*;

    #[test]
    fn get_traceable_type_cases() {
        // Recognised traceable types.
        assert_eq!(
            get_traceable_type(&parse_quote!(jsg::Rc<Foo>)),
            TraceableType::Ref
        );
        assert_eq!(
            get_traceable_type(&parse_quote!(jsg::Weak<Foo>)),
            TraceableType::Weak
        );
        assert_eq!(
            get_traceable_type(&parse_quote!(jsg::v8::Global<jsg::v8::Value>)),
            TraceableType::Global
        );
        // Must be fully qualified — bare `Rc` or wrapper types are not traceable.
        assert_eq!(
            get_traceable_type(&parse_quote!(Rc<Foo>)),
            TraceableType::None
        );
        assert_eq!(
            get_traceable_type(&parse_quote!(String)),
            TraceableType::None
        );
        // Option<jsg::Rc<T>> is not itself traceable — only the inner type is.
        assert_eq!(
            get_traceable_type(&parse_quote!(Option<jsg::Rc<Foo>>)),
            TraceableType::None
        );
    }

    #[test]
    fn extract_optional_inner_cases() {
        // Option<jsg::Rc<T>> → Option kind, inner is Ref.
        let ty: syn::Type = parse_quote!(Option<jsg::Rc<Foo>>);
        let (kind, inner) = extract_optional_inner(&ty).unwrap();
        assert!(matches!(kind, OptionalKind::Option));
        assert_eq!(get_traceable_type(inner), TraceableType::Ref);

        // Nullable<jsg::Rc<T>> → Nullable kind.
        let ty: syn::Type = parse_quote!(Nullable<jsg::Rc<Foo>>);
        let (kind, _) = extract_optional_inner(&ty).unwrap();
        assert!(matches!(kind, OptionalKind::Nullable));

        // Option<jsg::v8::Global<T>> → inner is Global.
        let ty: syn::Type = parse_quote!(Option<jsg::v8::Global<jsg::v8::Value>>);
        let (_, inner) = extract_optional_inner(&ty).unwrap();
        assert_eq!(get_traceable_type(inner), TraceableType::Global);

        // Not an Option/Nullable → None.
        assert!(extract_optional_inner(&parse_quote!(jsg::Rc<Foo>)).is_none());
    }

    #[test]
    fn extract_cell_inner_cases() {
        // Bare `Cell<T>` and std-qualified `std::cell::Cell<T>` both work.
        let ty: syn::Type = parse_quote!(Cell<jsg::Rc<Foo>>);
        let bare = extract_cell_inner(&ty).unwrap();
        assert_eq!(get_traceable_type(bare), TraceableType::Ref);

        let ty: syn::Type = parse_quote!(std::cell::Cell<jsg::Rc<Foo>>);
        let qualified = extract_cell_inner(&ty).unwrap();
        assert_eq!(get_traceable_type(qualified), TraceableType::Ref);

        // Not a Cell → None.
        assert!(extract_cell_inner(&parse_quote!(jsg::Rc<Foo>)).is_none());

        // Cell<Option<jsg::Rc<T>>> — inner is Option, not directly Ref;
        // but extract_optional_inner can unwrap the next level.
        let ty: syn::Type = parse_quote!(Cell<Option<jsg::Rc<Foo>>>);
        let inner = extract_cell_inner(&ty).unwrap();
        assert_eq!(get_traceable_type(inner), TraceableType::None);
        assert!(extract_optional_inner(inner).is_some());
    }

    #[test]
    fn extract_collection_traceable_element_collections() {
        // Vec, HashSet, BTreeSet all iterate elements (index 0).
        for ty in [
            parse_quote!(Vec<jsg::Rc<Foo>>),
            parse_quote!(HashSet<jsg::Rc<Foo>>),
            parse_quote!(BTreeSet<jsg::Rc<Foo>>),
        ] {
            let (style, traceable) = extract_collection_traceable(&ty).unwrap();
            assert!(matches!(style, CollectionIterStyle::IterElements));
            assert_eq!(traceable, TraceableType::Ref);
        }
    }

    #[test]
    fn extract_collection_traceable_map_collections() {
        // HashMap and BTreeMap iterate values (index 1).
        for ty in [
            parse_quote!(HashMap<String, jsg::Rc<Foo>>),
            parse_quote!(BTreeMap<String, jsg::Rc<Foo>>),
        ] {
            let (style, traceable) = extract_collection_traceable(&ty).unwrap();
            assert!(matches!(style, CollectionIterStyle::IterValues));
            assert_eq!(traceable, TraceableType::Ref);
        }

        // Global values are also traced.
        let (_, traceable) = extract_collection_traceable(
            &parse_quote!(BTreeMap<u32, jsg::v8::Global<jsg::v8::Value>>),
        )
        .unwrap();
        assert_eq!(traceable, TraceableType::Global);
    }

    #[test]
    fn extract_collection_traceable_non_traceable_cases() {
        // Vec<String> — plain element type, not traceable.
        assert!(extract_collection_traceable(&parse_quote!(Vec<String>)).is_none());
        // Vec<jsg::Weak<T>> — Weak is explicitly excluded.
        assert!(extract_collection_traceable(&parse_quote!(Vec<jsg::Weak<Foo>>)).is_none());
        // HashMap<jsg::Rc<K>, String> — only the value (index 1) is checked; String → None.
        assert!(extract_collection_traceable(&parse_quote!(HashMap<jsg::Rc<K>, String>)).is_none());
        // Not a collection at all.
        assert!(extract_collection_traceable(&parse_quote!(String)).is_none());
    }

    #[test]
    fn extract_collection_traceable_std_qualified_paths() {
        // std::vec::Vec and std::collections::HashMap match via last segment.
        assert!(extract_collection_traceable(&parse_quote!(std::vec::Vec<jsg::Rc<Foo>>)).is_some());
        let (style, _) = extract_collection_traceable(
            &parse_quote!(std::collections::HashMap<String, jsg::Rc<Foo>>),
        )
        .unwrap();
        assert!(matches!(style, CollectionIterStyle::IterValues));
    }
}
