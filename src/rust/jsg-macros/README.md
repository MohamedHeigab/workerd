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
> or `Vec<jsg::Rc<T>>`, but not `Option<Vec<jsg::Rc<T>>>`). For such cases use
> `#[jsg_trace]` delegation or `custom_trace` (see below).

### Delegating to a nested type with `#[jsg_trace]`

Annotate a field with `#[jsg_trace]` to delegate tracing to it. The macro emits
`jsg::GarbageCollected::trace(&self.field, visitor)` — the field's type must implement
`GarbageCollected`, enforced at compile time. `#[jsg_trace]` is stripped from the
emitted struct definition so the compiler never sees it as an unknown attribute.

The field type can implement `GarbageCollected` in two ways:

**Manually** (for complex logic):

```rust
struct EventHandlers {
    on_message: Option<jsg::v8::Global<jsg::v8::Value>>,
    on_error:   Option<jsg::v8::Global<jsg::v8::Value>>,
}

impl jsg::GarbageCollected for EventHandlers {
    fn trace(&self, visitor: &mut jsg::GcVisitor) {
        if let Some(ref h) = self.on_message { visitor.visit_global(h); }
        if let Some(ref h) = self.on_error   { visitor.visit_global(h); }
    }
    fn memory_name(&self) -> &'static std::ffi::CStr { c"EventHandlers" }
}

#[jsg_resource]
pub struct MySocket {
    #[jsg_trace]
    handlers: EventHandlers,
    name: String,
}
```

**Via `#[jsg_traceable]`** (auto-generated, see below):

```rust
#[jsg_traceable]
struct EventHandlers {
    on_message: Option<jsg::v8::Global<jsg::v8::Value>>,
    on_error:   Option<jsg::v8::Global<jsg::v8::Value>>,
}

#[jsg_resource]
pub struct MySocket {
    #[jsg_trace]
    handlers: EventHandlers,
    name: String,
}
```

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

### Custom tracing with `custom_trace`

For cases where `#[jsg_trace]` delegation is not enough, use
`#[jsg_resource(custom_trace)]` to suppress the generated `GarbageCollected` impl
and write your own. The macro still generates `jsg::Type`, `jsg::ToJS`, and
`jsg::FromJS` — only the GC impl is omitted.

```rust
#[jsg_resource(custom_trace)]
pub struct DynamicResource {
    slots: Vec<Option<jsg::Rc<Handler>>>,
}

impl jsg::GarbageCollected for DynamicResource {
    fn trace(&self, visitor: &mut jsg::GcVisitor) {
        for slot in &self.slots {
            if let Some(ref h) = slot {
                visitor.visit_rc(h);
            }
        }
    }
    fn memory_name(&self) -> &'static std::ffi::CStr { c"DynamicResource" }
}
```

`custom_trace` can be combined with `name`: `#[jsg_resource(name = "MyName", custom_trace)]`.

---

## `#[jsg_traceable]`

Generates `GarbageCollected` for a plain struct or enum that is not itself a
JavaScript resource but holds GC-visible handles. The type can then be used as a
`#[jsg_trace]` field inside a `#[jsg_resource]`.

### Plain struct

Equivalent to writing `GarbageCollected` by hand, but using the same automatic
field classifier as `#[jsg_resource]`. All field shapes from the
[Supported field shapes](#supported-field-shapes) table are recognised.

```rust
#[jsg_traceable]
struct Callbacks {
    pub on_data:  jsg::Rc<DataHandler>,
    pub on_error: Option<jsg::Rc<ErrorHandler>>,
}

#[jsg_resource]
pub struct EventSource {
    #[jsg_trace]
    callbacks: Callbacks,
}
```

### Enum — the `kj::OneOf` state-machine pattern

Each variant gets one `match` arm. Fields within the arm are classified with the
same cascade as struct fields:

| Variant kind | Generated arm |
|---|---|
| Unit (`Closed`) | `Self::Closed => {}` — no-op |
| Named fields (`Errored { reason: jsg::Rc<T> }`) | binds traceable fields by name, traces each |
| Tuple (`Readable(jsg::Rc<T>)`) | binds positionally as `_f0`, `_f1`, …, traces each |

```rust
#[jsg_traceable]
enum StreamState {
    Closed,                                     // unit — no-op arm
    Errored { reason: jsg::Rc<ErrorObject> },   // traces reason
    Readable(jsg::Rc<ReadableImpl>),            // traces _f0
}

#[jsg_resource]
pub struct ReadableStream {
    #[jsg_trace]
    state: StreamState,
    name: String,
}
```

### Nested `#[jsg_traceable]` types

`#[jsg_trace]` works inside `#[jsg_traceable]` too, enabling multi-level
delegation:

```rust
#[jsg_traceable]
enum InnerState { Empty, Loaded(jsg::Rc<Data>) }

#[jsg_traceable]
struct Outer {
    #[jsg_trace]  // delegates to InnerState::trace
    inner: InnerState,
}

#[jsg_resource]
pub struct Controller {
    #[jsg_trace]  // delegates to Outer::trace → InnerState::trace → visit_rc
    helper: Outer,
}
```

Override `memory_name()` with `#[jsg_traceable(name = "CustomName")]`.
