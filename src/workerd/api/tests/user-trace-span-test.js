// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

// This test exercises the getCurrentUserTraceSpan() fallback path introduced by the
// otel-async-context-frame changes.
//
// When the JS lock is held but userTraceAsyncContextKey has not been seeded in the async
// context frame (because enterContext() has not been implemented yet), makeUserTraceSpan()
// falls through to the IncomingRequest-level root user span.  The consequence is that ALL
// user spans are flat siblings under the root — there is no nesting.
//
// The test creates an explicit user span via withSpan() and makes a fetch subrequest
// INSIDE that span's lifetime.  Both the explicit span and the fetch span should appear as
// siblings under the root (flat), NOT with the fetch nested under the explicit span.
//
// When enterContext() is implemented (Phase 0b), the fetch span should become a child of
// the explicit span.  This test documents the current pre-enterContext() behavior and will
// need updating when Phase 0b lands.

import assert from 'node:assert';

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    if (url.pathname === '/target') {
      return new Response('hello from target');
    }

    if (url.pathname === '/nested-spans') {
      const { withSpan } = env.tracing;

      // Create an explicit user span via withSpan.  While this span is open, make a fetch
      // subrequest — the fetch internally calls makeUserTraceSpan("fetch") which calls
      // getCurrentUserTraceSpan().  Since enterContext() isn't implemented yet, the fetch
      // span will be a sibling of the outer span, not a child of it.
      const result = await withSpan('outer-op', async (span) => {
        span.setAttribute('test', 'nesting');

        const resp = await env.SELF.fetch('http://placeholder/target');
        assert.strictEqual(resp.status, 200);
        return await resp.text();
      });

      assert.strictEqual(result, 'hello from target');
      return new Response('done');
    }

    return new Response('not found', { status: 404 });
  },
};

export const test = {
  async test(ctrl, env) {
    // Trigger the handler that creates nested spans.
    const resp = await env.SELF.fetch('http://placeholder/nested-spans');
    assert.strictEqual(resp.status, 200);
    assert.strictEqual(await resp.text(), 'done');

    // Allow time for the streaming tail events to propagate.
    await scheduler.wait(50);
  },
};
