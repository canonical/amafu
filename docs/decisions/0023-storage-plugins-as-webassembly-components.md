# 0023: Storage plugins as WebAssembly components with a host that holds every capability

Status: accepted for milestone 6

## Decision

- **The contract is one WIT package, `amafu:storage@1.0.0`.** A plugin exports `provider` (check, create, delete,
  export to a node, and the three fencing calls) and imports only `http`, `secrets` and `logging`. Every call takes
  the backend's configuration as a string, so plugins keep no state between calls and any node can make any call.
- **The package defines its own `http` interface instead of importing `wasi:http`.** The host answers one request
  at a time with a whole body, which is what storage management APIs need, and the plugin cannot open sockets,
  stream or reach anything the host has not checked. `wasi:http` can replace it in a later major version.
- **The host holds every capability.** It sends a request only to a URL under one of the backend's declared
  `endpoints`, hands out only that backend's `secrets`, caps response bodies at 4 MiB, and checks what the plugin
  returns (identifiers, sizes, what an export may be) before it reaches a controller.
- **Each call is bounded:** 2 billion units of fuel, 64 MiB of linear memory with growth failures trapped, epoch
  interruption every 10 ms, and a 30-second wall clock enforced outside the guest as well. A trap, timeout or
  malformed answer is a retryable provider error; nothing a plugin does stops the node.
- **Modules are fetched by URL and pinned by SHA-256** (`module.url`, `module.digest` on the backend), up to 64 MiB,
  and cached by digest on each node. A module whose digest differs is refused.
- **The plugin kit** (`plugins/kit`) wraps the generated bindings and re-exports the contract's types, so a vendor
  writes a plain Rust implementation of `StorageProvider`. The bindings contain `unsafe`; the workspace denies
  `unsafe` and allows it only in the kit and in each plugin's `abi` module, which holds the single export macro.
- **The external reference plugin is the SPDK JSON-RPC plugin**, which mirrors the built-in NVMe/TCP provider over
  the host's `http`. The contract suite runs against the built-in local, NFS and NVMe/TCP providers and this
  plugin; a hostile plugin checks the limits (endless loop, memory growth, undeclared endpoint, other backends'
  secrets, oversized response, malformed answers, panics).

## Consequences

- Plugins run on every node that hosts or fences a volume, so the node binary carries the WebAssembly runtime and is
  now 35 MiB (10 MiB compressed); its effect on network boot time is not yet measured (0005).
- A vendor ships one `.wasm` file for every architecture and Amafu release that speaks `amafu:storage@1`.
- Secrets live in the backend specification, readable by anyone who may read storage backends (0005).
