# Writing a storage plugin

A storage plugin lets Amafu keep volumes on storage it has no built-in provider for. It is a WebAssembly component
that implements the contract in [`wit/storage-provider.wit`](../../wit/storage-provider.wit) (decision 0023). One
`.wasm` file works on every node and every Amafu release that speaks `amafu:storage@1`.

## What a plugin does

| Call | Must |
|---|---|
| `check` | Succeed when the storage is reachable with this configuration |
| `create-volume` | Create a volume of at least the requested size for `volume-id`, and succeed again if it already exists |
| `delete-volume` | Delete it, and succeed if it is already gone |
| `export-to-node` | Let the named node reach the volume, and return how it attaches |
| `revoke-node` | Stop the node reaching any volume, so a lost node cannot write after its VMs restart elsewhere |
| `has-revoked-node` | Report whether the node is revoked |
| `readmit-node` | Undo a revocation |

Every call may be retried, on any node, after a crash at any point: each must be idempotent. Every call receives the
backend's `configuration` as a JSON string; keep no state between calls.

`export-to-node` returns the attach target as JSON, one of:

```json
{"type": "nvme-tcp", "qualified_name": "nqn....", "addresses": ["10.0.4.10:4420"]}
{"type": "rbd", "monitors": ["10.0.1.11:6789"], "pool": "amafu", "image": "volume-1", "user": "amafu", "key": "..."}
{"type": "nfs-file", "server": "10.0.3.10", "export": "/srv/amafu", "file": "volume-1.qcow2"}
```

Errors are `retryable` (try again later), `invalid` (the configuration or request is wrong; a person must act),
`not-found` or `unsupported`.

## What a plugin may use

- **`http.send`**, for one request with a whole body, to a URL that starts with one of the backend's `endpoints`.
  Anything else is refused with `denied`. Responses are limited to 4 MiB.
- **`secrets.get`**, for the backend's own `secrets`. Put credentials there, not in the configuration.
- **`logging.log`**, which goes to the node's log, labelled with the module's digest.

Each call may use 2 billion units of fuel, 64 MiB of memory and 30 seconds. A plugin that exceeds them is stopped and
the call is retried later. WASI's clocks and random numbers are available; files, sockets and environment
variables are not.

## Build one

The plugin kit wraps the generated bindings. A plugin is a `cdylib`:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
amafu-storage-plugin-kit = { path = "../kit" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

```rust
use amafu_storage_plugin_kit::StorageProvider;
use amafu_storage_plugin_kit::http::{self, Request};
use amafu_storage_plugin_kit::secrets;
use amafu_storage_plugin_kit::types::{FencedNode, ProviderError, ProvisionedVolume, VolumeRequest};

struct ArrayPlugin;

impl StorageProvider for ArrayPlugin {
    fn check(configuration: String) -> Result<(), ProviderError> { /* ... */ }
    fn create_volume(configuration: String, request: VolumeRequest) -> Result<ProvisionedVolume, ProviderError> { /* ... */ }
    // ... the other five calls
}

#[allow(unsafe_code)]
mod abi {
    use super::ArrayPlugin;
    amafu_storage_plugin_kit::export_storage_plugin!(ArrayPlugin);
}
```

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
sha256sum target/wasm32-wasip2/release/array_plugin.wasm
```

`plugins/spdk-rpc` is a complete plugin for SPDK targets managed over JSON-RPC; start from it.

## Test it

Amafu's provider contract is the test a plugin must pass: it creates, exports, revokes, readmits and deletes volumes,
and repeats each call to check idempotence. `crates/storage/tests/plugin_kit.rs` runs it against the SPDK plugin and
a fake target; copy that test, point it at your module and a test system, and run it with `cargo test`. In the
virtual lab, `cargo rig lab storage-contract` runs it on a real node.

## Install it

Publish the `.wasm` file where the nodes can download it, then declare a backend:

```yaml
type: plugin
module:
  url: https://plugins.example/array-plugin-1.2.0.wasm
  digest: sha256:...
configuration:
  management: https://array.example:8443
  pool: amafu
endpoints:
  - https://array.example:8443/
secrets:
  user: amafu
  password: ...
```

```sh
amafu storage-backend create array --provider-file array.yaml
```

Nodes refuse a module whose digest differs. To upgrade a plugin, publish the new file under a new URL and update the
backend with its URL and digest.
