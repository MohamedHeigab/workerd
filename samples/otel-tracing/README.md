# OpenTelemetry Tracing Example

Demonstrates `@opentelemetry/api` working with workerd. The worker uses the
standard npm package to create spans, and a streaming tail worker receives
the span events.

## How it works

Before any user code runs, workerd's bootstrap preamble registers trace
providers on `Symbol.for('opentelemetry.js.api.1')`. When the npm package
loads, it discovers these providers and uses them instead of its built-in
no-ops. User-created spans flow through the existing tracing pipeline to
the streaming tail worker.

## Running

Build workerd and run:

```sh
bazel build //src/workerd/server:workerd
bazel-bin/src/workerd/server/workerd serve samples/otel-tracing/config.capnp
```

Then send a request:

```sh
curl http://localhost:8080/
```

The tail worker logs span events to stderr. You should see SpanOpen,
Attributes, and SpanClose events for `handle-request`, `auth.verify`,
`db.query`, and `validate`.

To trigger the error path:

```sh
curl http://localhost:8080/fail
```

## Files

- `worker.js` — Main worker using `@opentelemetry/api`
- `tail.js` — Streaming tail worker that logs span events
- `opentelemetry-api.mjs` — Vendored bundle of `@opentelemetry/api` v1.9.0
- `config.capnp` — workerd configuration wiring both workers
