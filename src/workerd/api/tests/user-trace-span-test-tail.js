// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

// Streaming tail worker for user-trace-span-test.
//
// Verifies that user trace spans produced by makeUserTraceSpan() → getCurrentUserTraceSpan()
// have correct parent-child nesting.
//
// The source worker creates an explicit user span ("outer-op") via withSpan() and makes a
// fetch subrequest INSIDE that span's lifetime.  startSpan() pushes the user span into
// userTraceAsyncContextKey, so getCurrentUserTraceSpan() returns it as the parent for any
// nested spans.  The fetch span should therefore be a CHILD of outer-op, not a sibling.

import assert from 'node:assert';

// Collect all events per trace (keyed by traceId).
const eventsByTrace = new Map();

export default {
  tailStream(event) {
    const traceId = event.spanContext.traceId;
    const topLevelSpanId = event.event.spanId;

    if (!eventsByTrace.has(traceId)) {
      eventsByTrace.set(traceId, {
        topLevelSpanId,
        onset: event.event,
        spans: new Map(),
        hasOutcome: false,
      });
    }

    return (event) => {
      const trace = eventsByTrace.get(event.spanContext.traceId);
      if (!trace) return;

      switch (event.event.type) {
        case 'spanOpen':
          trace.spans.set(event.event.spanId, {
            name: event.event.name,
            parentSpanId: event.spanContext.spanId,
          });
          break;
        case 'attributes': {
          const span = trace.spans.get(event.spanContext.spanId);
          if (span) {
            for (const { name, value } of event.event.info) {
              span[name] = value;
            }
          }
          break;
        }
        case 'spanClose': {
          const span = trace.spans.get(event.spanContext.spanId);
          if (span) span.closed = true;
          break;
        }
        case 'outcome':
          trace.hasOutcome = true;
          break;
      }
    };
  },
};

export const test = {
  async test() {
    // Wait for streaming tail events to arrive.
    await scheduler.wait(50);

    // Find the invocation that has both an "outer-op" span and a "fetch" span.
    let target = null;
    for (const [, trace] of eventsByTrace) {
      const names = Array.from(trace.spans.values()).map((s) => s.name);
      if (names.includes('outer-op') && names.includes('fetch')) {
        target = trace;
        break;
      }
    }

    assert(
      target,
      'Should find a trace with both "outer-op" and "fetch" spans. ' +
        `Got ${eventsByTrace.size} traces: ` +
        JSON.stringify(
          Array.from(eventsByTrace.values()).map((t) => ({
            onset: t.onset.info?.type,
            spans: Array.from(t.spans.values()).map((s) => s.name),
          }))
        )
    );

    const spans = Array.from(target.spans.values());
    const outerSpan = spans.find((s) => s.name === 'outer-op');
    const fetchSpan = spans.find((s) => s.name === 'fetch');

    assert(outerSpan, '"outer-op" span must exist');
    assert(fetchSpan, '"fetch" span must exist');

    // Both spans should be closed.
    assert.strictEqual(
      outerSpan.closed,
      true,
      '"outer-op" span should be closed'
    );
    assert.strictEqual(fetchSpan.closed, true, '"fetch" span should be closed');

    // The explicit span should have the attribute we set.
    assert.strictEqual(
      outerSpan.test,
      'nesting',
      '"outer-op" should have test=nesting attribute'
    );

    // The "outer-op" span should be a direct child of the top-level (root) span.
    assert.strictEqual(
      outerSpan.parentSpanId,
      target.topLevelSpanId,
      '"outer-op" should be a child of the root span'
    );

    // KEY ASSERTION: The "fetch" span should be a child of "outer-op", NOT a sibling.
    // startSpan() pushes the new span's SpanParent into userTraceAsyncContextKey via a
    // StorageScope.  While that scope is active, getCurrentUserTraceSpan() returns the
    // pushed span, so any nested makeUserTraceSpan() call (like the fetch subrequest)
    // picks it up as its parent.
    const outerSpanId = Array.from(target.spans.entries()).find(
      ([, s]) => s.name === 'outer-op'
    )[0];
    assert.strictEqual(
      fetchSpan.parentSpanId,
      outerSpanId,
      `"fetch" should be nested under "outer-op" (expected parent=${outerSpanId}, ` +
        `got parent=${fetchSpan.parentSpanId})`
    );

    // Verify the outcome event was received.
    assert.strictEqual(
      target.hasOutcome,
      true,
      'should have received outcome event'
    );
  },
};
