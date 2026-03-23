// Example: Using @opentelemetry/api with workerd
//
// This worker demonstrates that the standard @opentelemetry/api npm package
// works out of the box with workerd. The bootstrap preamble registers workerd's
// trace providers on the well-known global symbol before this module runs, so
// the npm package discovers them automatically.
//
// User-created spans flow to the attached streaming tail worker as SpanOpen,
// Attributes, and SpanClose events.

import { trace, SpanStatusCode } from '@opentelemetry/api';

const tracer = trace.getTracer('my-app', '1.0.0');

export default {
  async fetch(request) {
    return tracer.startActiveSpan('handle-request', async (rootSpan) => {
      rootSpan.setAttribute('http.method', request.method);
      rootSpan.setAttribute('http.url', request.url);

      // Simulate some work with nested spans
      const userId = await tracer.startActiveSpan(
        'auth.verify',
        async (span) => {
          span.setAttribute('auth.method', 'token');
          // Simulate async auth check
          await sleep(5);
          span.end();
          return 'user-123';
        }
      );

      const data = await tracer.startActiveSpan('db.query', async (span) => {
        span.setAttribute('db.system', 'd1');
        span.setAttribute('db.statement', 'SELECT * FROM users WHERE id = ?');
        span.setAttribute('db.user_id', userId);
        // Simulate DB query
        await sleep(10);
        span.end();
        return { name: 'Alice', id: userId };
      });

      // Demonstrate error handling
      try {
        await tracer.startActiveSpan('validate', async (span) => {
          span.setAttribute('validation.strict', true);
          if (request.url.includes('fail')) {
            span.setStatus({
              code: SpanStatusCode.ERROR,
              message: 'Validation failed',
            });
            span.end();
            throw new Error('Validation failed');
          }
          span.setStatus({ code: SpanStatusCode.OK });
          span.end();
        });
      } catch (e) {
        // Error was recorded on the span, continue with response
      }

      rootSpan.setAttribute('user.id', data.id);
      rootSpan.setAttribute('user.name', data.name);
      rootSpan.setStatus({ code: SpanStatusCode.OK });
      rootSpan.end();

      return new Response(
        JSON.stringify({
          message: 'Hello from OTel-instrumented worker!',
          user: data,
        }),
        {
          headers: { 'content-type': 'application/json' },
        }
      );
    });
  },
};

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
