# JSG Macros

Procedural macros for the JSG (JavaScript Glue) Rust bindings. These macros eliminate
boilerplate when implementing the JSG type system for Rust-backed JavaScript APIs.

## Crate layout

| File          | Contents                                                                       |
|---------------|--------------------------------------------------------------------------------|
| `lib.rs`      | Public macro entry points — thin dispatchers only                              |
| `resource.rs` | Code generation for `#[jsg_resource]` on structs and impl blocks               |
| `trace.rs`    | GC trace code generation — field classification and `trace()` body emission    |
| `utils.rs`    | Shared helpers: `extract_named_fields`, `snake_to_camel`, `is_lock_ref`, etc.  |

---

## `#[jsg_struct]`

Generates `jsg::Struct`, `jsg::Type`, `jsg::ToJS`, and `jsg::FromJS` for a plain data
struct. Only `pub` fields are projected into the JavaScript object. Use
`#[jsg_struct(name = "MyName")]` to override the JavaScript class name.

```rust
#[jsg_struct]
pub struct CaaRecord {
    pub critical: f64,
    pub tag: String,
    pub value: String,
}

#[jsg_struct(name = "CustomName")]
pub struct MyRecord {
    pub value: String,
}
```

---

## `#[jsg_method]`

Generates a V8 `FunctionCallback` for a method on a `#[jsg_resource]` type.

- **Instance methods** (`&self` / `&mut self`) are placed on the prototype.
- **Static methods** (no receiver) are placed on the constructor.
- Return types of `Result<T, E>` automatically throw a JavaScript exception on `Err`.
- The Rust `snake_case` name is converted to `camelCase` for JavaScript; override with
  `#[jsg_method(name = "jsName")]`.
- The first typed parameter may be `&mut Lock` / `&mut jsg::Lock` to receive the
  isolate lock directly — it is not exposed as a JavaScript argument.

```rust
#[jsg_resource]
impl DnsUtil {
    // Instance — obj.parseCaaRecord(…)
    #[jsg_method]
    pub fn parse_caa_record(&self, record: String) -> Result<CaaRecord, jsg::Error> { … }

    // Instance — obj.getName()
    #[jsg_method]
    pub fn get_name(&self) -> String { … }

    // Static — DnsUtil.create(…)
    #[jsg_method]
    pub fn create(name: String) -> Result<jsg::Rc<Self>, jsg::Error> { … }
}
```

---

## `#[jsg_resource]`

Generates JSG boilerplate for a resource type and its impl block.

**On a struct** — emits `jsg::Type`, `jsg::ToJS`, `jsg::FromJS`, and
`jsg::GarbageCollected`. The `trace()` body is synthesised automatically for every
field whose type is or contains a traceable JSG handle (see [Garbage Collection](#garbage-collection) below).
Use `#[jsg_resource(name = "JSName")]` to override the JavaScript class name.

**On an impl block** — emits `jsg::Resource::members()`, registering every
`#[jsg_method]`, `#[jsg_constructor]`, and `#[jsg_static_constant]` item.

```rust
#[jsg_resource]
pub struct DnsUtil {
    cache: HashMap<String, jsg::Rc<CacheEntry>>, // traced automatically
    name: String,                                // plain data, ignored by tracer
}

#[jsg_resource]
impl DnsUtil {
    #[jsg_method]
    pub fn lookup(&self, host: String) -> Result<String, jsg::Error> { … }

    #[jsg_method]
    pub fn create() -> Self { … }
}
```

---

## `#[jsg_static_constant]`

Exposes a Rust `const` as a read-only JavaScript property on both the constructor
and its prototype (equivalent to `JSG_STATIC_CONSTANT` in C++ JSG). The name is
used as-is (no camelCase). Only numeric types are supported.

```rust
#[jsg_resource]
impl WebSocket {
    #[jsg_static_constant]
    pub const CONNECTING: i32 = 0;
    #[jsg_static_constant]
    pub const OPEN: i32 = 1;
}
// JS: WebSocket.CONNECTING === 0 / instance.OPEN === 1
```

---

## `#[jsg_constructor]`

Marks a **static** method (no `self` receiver, returns `Self`) as the JavaScript
constructor. Only one per impl block is allowed. Without it, `new MyClass()` throws
`Illegal constructor`. An optional first parameter of `&mut Lock` is passed the
isolate lock and is not counted as a JavaScript argument.

```rust
#[jsg_resource]
impl Greeting {
    #[jsg_constructor]
    fn constructor(message: String) -> Self {
        Self { message }
    }
}
// JS: let g = new Greeting("hello");
```

---

## `#[jsg_oneof]`

Generates `jsg::Type` and `jsg::FromJS` for a union enum — the Rust equivalent of
`kj::OneOf<…>`. Each variant must be a single-field tuple whose inner type implements
`jsg::Type` + `jsg::FromJS`. Variants are tried in declaration order using
exact-type matching; if none matches, a `TypeError` is thrown listing all expected
types.

```rust
#[jsg_oneof]
#[derive(Debug, Clone)]
enum StringOrNumber {
    String(String),
    Number(jsg::Number),
}

#[jsg_resource]
impl MyResource {
    #[jsg_method]
    pub fn process(&self, value: StringOrNumber) -> String {
        match value {
            StringOrNumber::String(s) => format!("string: {s}"),
            StringOrNumber::Number(n) => format!("number: {}", n.value()),
        }
    }
}
```

---

## Garbage Collection

`#[jsg_resource]` on a struct synthesises a `GarbageCollected::trace` body that
automatically visits every field whose type is — or contains — a traceable JSG handle.
No manual implementation is needed for any of the supported shapes.

### Supported field shapes

| Field type | Trace behaviour |
|---|---|
| `jsg::Rc<T>` | Strong GC edge — `visitor.visit_rc` |
| `jsg::v8::Global<T>` | Dual strong/traced — `visitor.visit_global` (enables cycle collection) |
| `jsg::Weak<T>` | **Not traced** — does not keep the target alive |
| `Option<jsg::Rc<T>>` | Traced when `Some` |
| `Option<jsg::v8::Global<T>>` | Traced when `Some` |
| `jsg::Nullable<jsg::Rc<T>>` | Traced when `Some` |
| `jsg::Nullable<jsg::v8::Global<T>>` | Traced when `Some` |
| `Vec<jsg::Rc<T>>` | Each element visited — `visitor.visit_rc` |
| `Vec<jsg::v8::Global<T>>` | Each element visited — `visitor.visit_global` |
| `HashMap<K, jsg::Rc<T>>` | Each value visited via `.values()` |
| `HashMap<K, jsg::v8::Global<T>>` | Each value visited via `.values()` |
| `BTreeMap<K, jsg::Rc<T>>` | Each value visited via `.values()` |
| `BTreeMap<K, jsg::v8::Global<T>>` | Each value visited via `.values()` |
| `HashSet<jsg::Rc<T>>` | Each element visited |
| `HashSet<jsg::v8::Global<T>>` | Each element visited |
| `BTreeSet<jsg::Rc<T>>` | Each element visited |
| `BTreeSet<jsg::v8::Global<T>>` | Each element visited |
| `Cell<T>` / `std::cell::Cell<T>` for any of the above | Same as above — `Cell::as_ptr()` read is safe because tracing is single-threaded and never re-entrant |
| Anything else | **Not traced** — plain data, ignored |

The `Cell<…>` variants are required whenever a traced field needs to be mutated
after construction, because `GarbageCollected::trace` receives `&self`.

> **Note:** Nesting is not recursive. `Option<Vec<jsg::Rc<T>>>` is **not** automatically
> traced — only one level of wrapping around the traceable is supported (e.g. `Option<jsg::Rc<T>>`
> or `Vec<jsg::Rc<T>>`, but not `Option<Vec<jsg::Rc<T>>>`). For such cases, implement
> `GarbageCollected` manually (see [Custom tracing](#custom-tracing)).

```rust
use std::cell::Cell;
use std::collections::HashMap;

#[jsg_resource]
pub struct EventRouter {
    // Strong edges — all children kept alive through GC.
    handlers: HashMap<String, jsg::Rc<Handler>>,

    // Conditionally traced.
    fallback: Option<jsg::Rc<Handler>>,

    // Interior-mutable callback set after construction; dual-mode Global enables
    // cycle collection if the callback closes over this resource's own JS wrapper.
    on_error: Cell<Option<jsg::v8::Global<jsg::v8::Value>>>,

    // Weak — does not keep target alive.
    parent: jsg::Weak<EventRouter>,

    // Plain data — not traced.
    name: String,
}
```

### `jsg::v8::Global<T>` and cycle collection

`jsg::v8::Global<T>` uses the same strong↔traced dual-mode as C++ `jsg::V8Ref<T>`.
While the parent resource holds at least one strong Rust `Rc`, the V8 handle stays
strong. Once all `Rc`s are dropped and only the JS wrapper keeps the resource alive,
`visit_global` downgrades the handle to a `v8::TracedReference` that cppgc can
follow — allowing back-reference cycles (e.g. a resource that stores a callback
which closes over its own JS wrapper) to be detected and collected on the next full GC.

### Custom tracing

If the generated `trace()` body is insufficient, implement `GarbageCollected`
manually instead of using `#[jsg_resource]` on the struct:

```rust
impl jsg::GarbageCollected for CustomResource {
    fn trace(&self, visitor: &mut jsg::GcVisitor) {
        // custom logic
        for item in &self.dynamic_children {
            visitor.visit_rc(item);
        }
    }

    fn memory_name(&self) -> &'static std::ffi::CStr {
        c"CustomResource"
    }
}
```
