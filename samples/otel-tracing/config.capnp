using Workerd = import "/workerd/workerd.capnp";

const otelTracingExample :Workerd.Config = (
  services = [
    (name = "main", worker = .mainWorker),
    (name = "log", worker = .logWorker),
  ],
  sockets = [ ( name = "http", address = "*:8080", http = (), service = "main" ) ],
);

const mainWorker :Workerd.Worker = (
  modules = [
    (name = "worker", esModule = embed "worker.js"),
    # Vendored bundle of @opentelemetry/api v1.9.0 (esbuild --bundle --format=esm)
    (name = "@opentelemetry/api", esModule = embed "opentelemetry-api.mjs"),
  ],
  compatibilityDate = "2024-10-14",
  streamingTails = ["log"],
);

const logWorker :Workerd.Worker = (
  modules = [
    (name = "worker", esModule = embed "tail.js")
  ],
  compatibilityDate = "2024-10-14",
);
