// Copyright (c) 2026 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

//! GC tracing tests for `#[jsg_traceable]` on enums and plain structs.
//!
//! Covers:
//! - Enum state machines where some variants hold `jsg::Rc<T>` (the `kj::OneOf` pattern).
//! - Plain structs annotated with `#[jsg_traceable]` instead of writing
//!   `GarbageCollected` by hand.
//! - Both used as `#[jsg_trace]` fields inside `#[jsg_resource]` structs.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use jsg::ToJS;
use jsg_macros::jsg_resource;
use jsg_macros::jsg_traceable;

// =============================================================================
// Shared leaf resource
// =============================================================================

static LEAF_DROPS: AtomicUsize = AtomicUsize::new(0);

#[jsg_resource]
struct Leaf {
    #[expect(
        dead_code,
        reason = "set on construction; only the Drop side-effect matters"
    )]
    pub value: u32,
}

impl Drop for Leaf {
    fn drop(&mut self) {
        LEAF_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

#[jsg_resource]
impl Leaf {}

// =============================================================================
// #[jsg_traceable] on an enum — state machine pattern
//
// Models:
//   kj::OneOf<StreamStates::Closed, StreamStates::Errored, Readable> state;
// =============================================================================

static ENUM_PARENT_DROPS: AtomicUsize = AtomicUsize::new(0);

/// State enum equivalent to the C++ `kj::OneOf` pattern.
///
/// - `Closed` — unit variant, nothing to trace.
/// - `Errored { reason }` — named-field variant, traces `reason: jsg::Rc<Leaf>`.
/// - `Readable(jsg::Rc<Leaf>)` — tuple variant, traces the inner `Rc`.
#[jsg_traceable]
enum StreamState {
    /// No GC-visible fields.
    Closed,
    /// Named-field variant — `reason` is traced.
    Errored { reason: jsg::Rc<Leaf> },
    /// Tuple variant — the `jsg::Rc<Leaf>` is traced.
    Readable(jsg::Rc<Leaf>),
}

/// Resource whose state is held in a `#[jsg_trace]`-delegated enum.
#[jsg_resource]
struct StreamController {
    #[jsg_trace]
    pub state: StreamState,
}

impl Drop for StreamController {
    fn drop(&mut self) {
        ENUM_PARENT_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

#[jsg_resource]
impl StreamController {}

// ---------------------------------------------------------------------------

/// Unit variant — nothing to trace, no crash.
#[test]
fn enum_unit_variant_does_not_crash_during_gc() {
    ENUM_PARENT_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let parent = jsg::Rc::new(StreamController {
            state: StreamState::Closed,
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });
}

/// Named-field variant — `reason: jsg::Rc<Leaf>` is kept alive.
#[test]
fn enum_named_variant_rc_field_kept_alive_through_gc() {
    LEAF_DROPS.store(0, Ordering::SeqCst);
    ENUM_PARENT_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let reason = jsg::Rc::new(Leaf { value: 1 });

        let parent = jsg::Rc::new(StreamController {
            state: StreamState::Errored {
                reason: reason.clone(),
            },
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);

        std::mem::drop(reason);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });
}

/// Tuple variant — the `jsg::Rc<Leaf>` inside is kept alive.
#[test]
fn enum_tuple_variant_rc_field_kept_alive_through_gc() {
    LEAF_DROPS.store(0, Ordering::SeqCst);
    ENUM_PARENT_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let readable = jsg::Rc::new(Leaf { value: 2 });

        let parent = jsg::Rc::new(StreamController {
            state: StreamState::Readable(readable.clone()),
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);

        std::mem::drop(readable);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });
}

/// Two parents with different variants — both are collected once their globals
/// are gone, taking their respective children with them.
#[test]
fn enum_two_parents_different_variants_both_collected() {
    LEAF_DROPS.store(0, Ordering::SeqCst);
    ENUM_PARENT_DROPS.store(0, Ordering::SeqCst);

    // First harness: Errored variant with a named-field child.
    let harness1 = crate::Harness::new();
    harness1.run_in_context(|lock, ctx| {
        let reason = jsg::Rc::new(Leaf { value: 10 });
        let parent = jsg::Rc::new(StreamController {
            state: StreamState::Errored {
                reason: reason.clone(),
            },
        });
        ctx.set_global("parent", parent.clone().to_js(lock));
        std::mem::drop(reason);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        // Still reachable via global.
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });
    harness1.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });

    // Second harness: Readable variant with a tuple child.
    let harness2 = crate::Harness::new();
    harness2.run_in_context(|lock, ctx| {
        let readable = jsg::Rc::new(Leaf { value: 20 });
        let parent = jsg::Rc::new(StreamController {
            state: StreamState::Readable(readable.clone()),
        });
        ctx.set_global("parent", parent.clone().to_js(lock));
        std::mem::drop(readable);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        // Still reachable via global.
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1); // only the first harness's leaf
        Ok(())
    });
    harness2.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(ENUM_PARENT_DROPS.load(Ordering::SeqCst), 2);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 2);
        Ok(())
    });
}

// =============================================================================
// #[jsg_traceable] on a plain struct
//
// Replaces manual GarbageCollected impl for PrivateData-style helpers.
// =============================================================================

static STRUCT_PARENT_DROPS: AtomicUsize = AtomicUsize::new(0);

/// Plain helper struct annotated with `#[jsg_traceable]` — no manual
/// `GarbageCollected` impl needed.
#[jsg_traceable]
struct Callbacks {
    pub on_data: jsg::Rc<Leaf>,
    pub on_error: Option<jsg::Rc<Leaf>>,
}

/// Resource that delegates tracing to the `#[jsg_traceable]` struct via `#[jsg_trace]`.
#[jsg_resource]
struct EventSource {
    #[jsg_trace]
    pub callbacks: Callbacks,
}

impl Drop for EventSource {
    fn drop(&mut self) {
        STRUCT_PARENT_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

#[jsg_resource]
impl EventSource {}

/// Children held inside a `#[jsg_traceable]` struct are kept alive.
#[test]
fn traceable_struct_children_kept_alive_through_gc() {
    LEAF_DROPS.store(0, Ordering::SeqCst);
    STRUCT_PARENT_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let on_data = jsg::Rc::new(Leaf { value: 30 });
        let on_error = jsg::Rc::new(Leaf { value: 31 });

        let parent = jsg::Rc::new(EventSource {
            callbacks: Callbacks {
                on_data: on_data.clone(),
                on_error: Some(on_error.clone()),
            },
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);

        std::mem::drop(on_data);
        std::mem::drop(on_error);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        assert_eq!(STRUCT_PARENT_DROPS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(STRUCT_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 2);
        Ok(())
    });
}

/// Optional field `None` — no crash, parent still collected.
#[test]
fn traceable_struct_optional_none_does_not_crash() {
    STRUCT_PARENT_DROPS.store(0, Ordering::SeqCst);
    LEAF_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let on_data = jsg::Rc::new(Leaf { value: 99 });

        let parent = jsg::Rc::new(EventSource {
            callbacks: Callbacks {
                on_data: on_data.clone(),
                on_error: None,
            },
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);
        std::mem::drop(on_data);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        assert_eq!(STRUCT_PARENT_DROPS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(STRUCT_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });
}

// =============================================================================
// Nested #[jsg_traceable] — enum inside a #[jsg_traceable] struct
// =============================================================================

static NESTED_PARENT_DROPS: AtomicUsize = AtomicUsize::new(0);

/// Inner enum used inside the outer `#[jsg_traceable]` struct.
#[jsg_traceable]
enum InnerState {
    // Intentionally unused in tests — exercises the unit-variant no-op arm.
    #[expect(
        dead_code,
        reason = "exercises the unit-variant no-op arm in jsg_traceable"
    )]
    Empty,
    Loaded(jsg::Rc<Leaf>),
}

/// Outer helper struct that holds an inner `#[jsg_traceable]` enum.
#[jsg_traceable]
struct OuterHelper {
    #[jsg_trace]
    pub inner: InnerState,
}

#[jsg_resource]
struct NestedParent {
    #[jsg_trace]
    pub helper: OuterHelper,
}

impl Drop for NestedParent {
    fn drop(&mut self) {
        NESTED_PARENT_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

#[jsg_resource]
impl NestedParent {}

/// Two levels of `#[jsg_trace]` delegation — child inside enum inside struct inside resource.
#[test]
fn nested_jsg_traceable_child_kept_alive_through_gc() {
    LEAF_DROPS.store(0, Ordering::SeqCst);
    NESTED_PARENT_DROPS.store(0, Ordering::SeqCst);

    let harness = crate::Harness::new();
    harness.run_in_context(|lock, ctx| {
        let leaf = jsg::Rc::new(Leaf { value: 42 });

        let parent = jsg::Rc::new(NestedParent {
            helper: OuterHelper {
                inner: InnerState::Loaded(leaf.clone()),
            },
        });
        let wrapped = parent.clone().to_js(lock);
        ctx.set_global("parent", wrapped);

        std::mem::drop(leaf);
        std::mem::drop(parent);

        crate::Harness::request_gc(lock);
        // Leaf is kept alive: resource → #[jsg_trace] OuterHelper →
        // #[jsg_trace] InnerState::Loaded → jsg::Rc<Leaf>
        assert_eq!(NESTED_PARENT_DROPS.load(Ordering::SeqCst), 0);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 0);
        Ok(())
    });

    harness.run_in_context(|lock, _ctx| {
        crate::Harness::request_gc(lock);
        assert_eq!(NESTED_PARENT_DROPS.load(Ordering::SeqCst), 1);
        assert_eq!(LEAF_DROPS.load(Ordering::SeqCst), 1);
        Ok(())
    });
}
