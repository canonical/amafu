# Developing Amafu

## Layout

| Path | Contents |
|---|---|
| `crates/domain` | Resource kinds, names, the generation and status rules, the kind registry, roles and verbs |
| `crates/interfaces` | Traits for every side effect, typed store helpers, error classification, contract suites |
| `crates/reconciler` | The controller shape, keyed work queue, backoff and run loop |
| `crates/controllers` | Placement, compute and node controllers |
| `crates/state` | The `Store` and audit log on a single-voter Quorium node |
| `crates/hypervisor` | The per-VM process embedding Cloud Hypervisor, and the node-side adapter |
| `crates/api` | Routes, authentication, authorization, admission, audit, the TLS server and the client |
| `crates/system` | What PID 1 needs: mounts, kernel modules, boot media, firmware facts |
| `crates/network` | The node's underlay address over netlink |
| `crates/node` | `amafu-node`: PID 1, role wiring, the test-only diagnostic channel |
| `crates/command-line` | `amafu` |
| `test-rig/*` | The `Cluster` trait, scenarios, checkers, the simulated world, the LXD lab, and `cargo rig` |
| `image` | The node image build |
| `docs/decisions` | Decision records |

## Everyday checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --features amafu-node/diagnostics
cargo test --workspace
cargo rig simulate --seeds 200 --crash-roles
```

A failing seed is replayed with `cargo rig replay <seed> [--scenario <part of its name>] [--crash-roles]`.

## Fuzzing

```sh
cd crates/api/fuzz
cargo +nightly fuzz run create_body -- -max_total_time=60
cargo +nightly fuzz run update_body -- -max_total_time=60
```

## The node image

`image/build test` builds the test image in `image/output/test`, and `image/build release` the release image. It
needs `apt-get download` access to the Ubuntu archive, plus `cpio`, `zstd` and `depmod`. Assembling a lab disk with
`image/assemble-disk` also needs `sfdisk`, `mkfs.vfat`, `mtools`, `mkfs.ext4` and `qemu-img`.

## The virtual lab

The lab runs on an LXD server reachable as the `amafu-lab` remote of the snap's `lxc` client (its client
certificate and the pinned server certificate are read from `~/snap/lxd/common/config`). The defaults in
`test-rig/lxd-lab/src/topology.rs` describe that server's address, bridge and storage pool.

```sh
image/build test
cargo rig lab up          # creates the amafu-lab LXD project and boots node-0
cargo rig lab contract    # the hypervisor contract against the real Cloud Hypervisor adapter
cargo rig lab run         # every scenario at the virtual-lab level
cargo rig lab instances   # VM processes on each node, with their console output
cargo rig lab down        # deletes the project
```

`target/rig/lab.json` holds the lab's CA and tokens. To use the command line against the lab:

```sh
export AMAFU_ENDPOINT=https://10.0.10.13:17443 AMAFU_PROJECT=project-a
export AMAFU_CERTIFICATE_AUTHORITY=<the certificate_authority from lab.json, saved to a file>
export AMAFU_TOKEN=<a token from lab.json, for example operator@project-a>
amafu vm create web-1 --cpus 1 --memory 256 --image test-guest --wait
amafu vm list
amafu vm delete web-1 --wait
```
