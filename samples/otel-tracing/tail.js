// Streaming tail worker that receives OTel-generated span events.
//
// This worker receives SpanOpen, Attributes, SpanClose, and other events
// from the instrumented worker. It logs each event as a single JSON line
// to make the output easy to parse and inspect.

export default {
  tailStream(onsetEvent) {
    const invocationId = onsetEvent.invocationId;
    const scriptName = onsetEvent.event?.scriptName ?? 'unknown';

    console.log(JSON.stringify({ type: 'onset', invocationId, scriptName }));

    return (event) => {
      const spanId =
        event.event?.spanId ?? event.spanContext?.spanId ?? undefined;
      const ts = event.timestamp ?? undefined;
      const eventType = event.event?.type;

      switch (eventType) {
        case 'spanOpen':
          console.log(
            JSON.stringify({
              type: 'spanOpen',
              invocationId,
              spanId,
              name: event.event.name,
              ts,
            })
          );
          break;

        case 'attributes':
          if (event.event.info) {
            const attributes = {};
            for (const { name, value } of event.event.info) {
              attributes[name] = value;
            }
            console.log(
              JSON.stringify({
                type: 'attributes',
                invocationId,
                spanId,
                attributes,
                ts,
              })
            );
          }
          break;

        case 'spanClose':
          console.log(
            JSON.stringify({
              type: 'spanClose',
              invocationId,
              spanId,
              outcome: event.event.outcome ?? undefined,
              ts,
            })
          );
          break;

        case 'outcome':
          console.log(
            JSON.stringify({
              type: 'outcome',
              invocationId,
              outcome: event.event.outcome,
              cpuTime: event.event.cpuTime,
              wallTime: event.event.wallTime,
              ts,
            })
          );
          break;

        case 'log':
          console.log(
            JSON.stringify({
              type: 'log',
              invocationId,
              level: event.event.level,
              message: event.event.message,
              ts,
            })
          );
          break;

        case 'exception':
          console.log(
            JSON.stringify({
              type: 'exception',
              invocationId,
              name: event.event.name,
              message: event.event.message,
              ts,
            })
          );
          break;

        default:
          console.log(
            JSON.stringify({
              type: eventType ?? 'unknown',
              invocationId,
              event: event.event,
              ts,
            })
          );
      }
    };
  },
};
