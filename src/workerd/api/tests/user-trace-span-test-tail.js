// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

// Streaming tail worker for user-trace-span-test.
//
// Verifies that user trace spans produced by makeUserTraceSpan() → getCurrentUserTraceSpan()
// have the correct parent-child relationships.
//
// The source worker creates an explicit user span ("outer-op") via withSpan() and makes a
// fetch subrequest INSIDE that span's lifetime.  Since enterContext() is not yet implemented,
// getCurrentUserTraceSpan() always falls back to the IncomingRequest root user span, meaning
// BOTH the "outer-op" span and the "fetch" span should be flat siblings under the root.
//
// When enterContext() is implemented (Phase 0b), the "fetch" span should become a child of
// "outer-op" instead.  Update this test accordingly when that lands.

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

    // KEY ASSERTION: Both spans should be children of the TOP-LEVEL span (flat / siblings).
    // This is because enterContext() is not implemented yet, so getCurrentUserTraceSpan()
    // always falls back to the IncomingRequest root user span — every user span created in
    // the same request is a sibling under the root.
    //
    // Once enterContext() is implemented (Phase 0b), the fetch span should become a child of
    // outer-op, and this assertion should change to:
    //   assert.strictEqual(fetchSpan.parentSpanId, outerSpanId);
    assert.strictEqual(
      outerSpan.parentSpanId,
      target.topLevelSpanId,
      '"outer-op" should be a child of the root span'
    );
    assert.strictEqual(
      fetchSpan.parentSpanId,
      target.topLevelSpanId,
      '"fetch" should be a child of the root span (not nested under "outer-op"), ' +
        'because enterContext() is not yet implemented'
    );

    // Verify the outcome event was received.
    assert.strictEqual(
      target.hasOutcome,
      true,
      'should have received outcome event'
    );
  },
};
