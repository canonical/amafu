# Developing Amafu

## Layout

| Path | Contents |
|---|---|
| `crates/domain` | Resource kinds, names, the generation and status rules, the kind registry, roles and verbs |
| `crates/interfaces` | Traits for every side effect, typed store helpers, error classification, contract suites |
| `crates/reconciler` | The controller shape, keyed work queue, backoff and run loop |
| `crates/controllers` | Placement with address allocation, compute, node, image, volume, storage backend, network, security group and node network controllers |
| `crates/state` | The `Store` and audit log on a single-voter Quorium node |
| `crates/hypervisor` | The sandboxed per-VM process embedding Cloud Hypervisor, the node-side adapter and the guest agent client |
| `crates/storage` | Local and NFS volumes as qcow2 files, and the image cache with its OCI registry client |
| `crates/guest-agent` | The init of container VMs, and the protocol the node speaks to it |
| `crates/api` | Routes, authentication, authorization, admission, audit, the TLS server and the client |
| `crates/system` | What PID 1 needs: mounts, kernel modules, boot media, firmware facts |
| `crates/network` | The underlay, bridges and taps, the nftables ruleset, and the DHCP, DNS and metadata services |
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

Some tests need root and are ignored by default: the kernel datapath contract (`crates/network/tests`), the VM
process sandbox (`crates/node/tests/sandbox.rs`), and a real registry pull (`crates/storage/tests`). Build them with
`cargo test --no-run` and run the test binary with `sudo ... --ignored`.

## Fuzzing

| Crate | Targets |
|---|---|
| `crates/api/fuzz` | `create_body`, `update_body` |
| `crates/network/fuzz` | `dhcp_message`, `dns_query`, `metadata_request` |
| `crates/guest-agent/fuzz` | `agent_messages` |

```sh
cd crates/network/fuzz
cargo +nightly fuzz run dhcp_message -- -max_total_time=60
```

## The node image

`image/build test` builds the test image in `image/output/test`, and `image/build release` the release image. It
needs `apt-get download` access to the Ubuntu archive and GitHub (for the pinned VM firmware), plus `cpio`, `zstd`
and `depmod`, and the build machine's `nftables` and `erofs-utils` packages, which it copies onto the boot partition
(decision 0010). Assembling a lab disk with
`image/assemble-disk` also needs `sfdisk`, `mkfs.vfat`, `mtools`, `mkfs.ext4` and `qemu-img`.

## The virtual lab

The lab runs on an LXD server reachable as the `amafu-lab` remote of the snap's `lxc` client (its client
certificate and the pinned server certificate are read from `~/snap/lxd/common/config`). The defaults in
`test-rig/lxd-lab/src/topology.rs` describe that server's address, bridge and storage pool.

```sh
image/build test
cargo rig lab up          # creates the amafu-lab LXD project, the NFS server VM, and boots node-0
cargo rig lab contract    # the hypervisor contract against the real Cloud Hypervisor adapter
cargo rig lab run         # every scenario at the virtual-lab level
cargo rig lab instances   # VM processes on each node, with their console and VM process log
cargo rig lab inspect network      # ports and the nftables ruleset on each node
cargo rig lab inspect attachments  # volumes attached on each node
cargo rig lab inspect sandbox      # run the VM process sandbox probes
cargo rig lab down        # deletes the project
```

`target/rig/lab.json` holds the lab's CA and tokens. To use the command line against the lab:

```sh
export AMAFU_ENDPOINT=https://10.0.10.13:17443 AMAFU_PROJECT=project-a
export AMAFU_CERTIFICATE_AUTHORITY=<the certificate_authority from lab.json, saved to a file>
export AMAFU_TOKEN=<a token from lab.json, for example operator@project-a>
amafu vm create web-1 --cpus 1 --memory 256 --kernel test-guest --wait
amafu vm list
amafu vm delete web-1 --wait
```

Milestone 2 kinds, from the command line (storage backends need an operator token):

```sh
amafu network create private --subnet 10.70.0.0/24
amafu security-group create web --rule ingress,tcp,80,any --rule ingress,tcp,22,10.0.0.0/8
amafu vm create web-2 --kernel test-guest --network private --security-group web --user-data script.sh --wait

amafu image create busybox --container docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec
amafu container run box --image busybox --network private --env GREETING=hello --wait -- sh -c 'echo $GREETING; sleep 3600'

amafu storage-backend create local --local-disk
amafu image create noble --disk-url https://cloud-images.ubuntu.com/minimal/releases/noble/release/ubuntu-24.04-minimal-cloudimg-amd64.img \
    --sha256 3839f148c5a3b4a736e7dbb0af75594743fa5ebee2a202424ba1a32e5d3b5d2c
amafu volume create noble-root --size 4 --backend local --from-image noble
amafu vm create ubuntu --from-disk --volume noble-root --memory 1024 --network private --user-data cloud.yaml --wait
```

The lab's NFS export is `10.215.2.40:/srv/amafu` (`amafu storage-backend create shared --nfs 10.215.2.40:/srv/amafu`).
