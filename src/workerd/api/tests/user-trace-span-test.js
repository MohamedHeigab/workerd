// Copyright (c) 2025 Cloudflare, Inc.
// Licensed under the Apache 2.0 license found in the LICENSE file or at:
//     https://opensource.org/licenses/Apache-2.0

// This test exercises user trace span nesting via getCurrentUserTraceSpan().
//
// startSpan() pushes the new span's user SpanParent into userTraceAsyncContextKey in the
// async context frame.  While that span is active, any nested makeUserTraceSpan() call
// (e.g. from a fetch subrequest) picks up the pushed value as its parent, producing
// proper span nesting.
//
// The test creates an explicit user span via withSpan('outer-op', ...) and makes a fetch
// subrequest INSIDE that span's lifetime.  The streaming tail worker verifies that the
// fetch span is a CHILD of outer-op (not a sibling).

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
