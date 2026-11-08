# Developing Amafu

## Layout

| Path | Contents |
|---|---|
| `crates/domain` | Resource kinds, names, the generation and status rules, the kind registry, roles and verbs |
| `crates/interfaces` | Traits for every side effect, typed store helpers, error classification, contract suites |
| `crates/reconciler` | The controller shape, keyed work queue, backoff and run loop |
| `crates/scheduler` | Placement as a pure function: filters, scores and the placement benchmark |
| `crates/controllers` | The scheduler, compute, node, node health, image, volume, local volume, storage backend, network, security group, node network, floating IP and retirement controllers |
| `crates/state` | The `Store`, audit log and leases on Quorium voters, and the subscribed store with its local state cache |
| `crates/hypervisor` | The sandboxed per-VM process embedding Cloud Hypervisor, the node-side adapter and the guest agent client |
| `crates/storage` | Local and NFS volumes as qcow2 files, NVMe/TCP volumes on SPDK, Ceph RBD volumes, and the image cache with its OCI registry client |
| `crates/guest-agent` | The init of container VMs, and the protocol the node speaks to it |
| `crates/api` | Routes, authentication, authorization, admission, audit, the TLS server and the client, and the node API with its client |
| `crates/system` | What PID 1 needs: mounts, kernel modules, boot media, firmware facts |
| `crates/network` | The underlay, bridges, taps and VXLAN tunnels, floating addresses, the nftables ruleset, and the DHCP, DNS and metadata services |
| `crates/node` | `amafu-node`: PID 1, role wiring, the test-only diagnostic channel |
| `crates/command-line` | `amafu` |
| `crates/audit-export` | Delivering the audit log to syslog (RFC 5424 over TCP or TLS) and S3-compatible object storage |
| `crates/dashboard` | The dashboard server: sealed sessions, the proxy that forwards with each user's token, and the event stream |
| `crates/dashboard/frontend` | The dashboard's React and Vanilla sources; the built bundle is committed in `crates/dashboard/assets` |
| `wit`, `plugins` | The storage plugin contract, the plugin kit, the SPDK JSON-RPC plugin and the hostile test plugin |
| `test-rig/*` | The `Cluster` trait, scenarios, checkers, the simulated world, the LXD lab, the scale run, the demo cluster, and `cargo rig` |
| `image` | The node image build |
| `docs/decisions` | Decision records |

## Everyday checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --features amafu-node/diagnostics
cargo test --workspace
cargo rig simulate --seeds 200 --crash-roles
cargo bench -p amafu-scheduler
```

The simulated world runs the first three nodes of a scenario as core members holding leases and the rest as nodes
outside the core, on their subscribed share through the node API. `Fault::Partition` cuts a node off; a cut-off core
member loses its leases.

A failing seed is replayed with `cargo rig replay <seed> [--scenario <part of its name>] [--crash-roles]`.

Some tests need root and are ignored by default: the kernel datapath contract and the tunnel and floating address
test (`crates/network/tests`), the VM
process sandbox (`crates/node/tests/sandbox.rs`), and a real registry pull (`crates/storage/tests`). Build them with
`cargo test --no-run` and run the test binary with `sudo ... --ignored`.

`docs/reference/kinds.md` is generated from the kind registry with `cargo rig reference`; a test fails when it is
out of date.

## Storage plugins

Plugins are WebAssembly components built against `wit/storage-provider.wit` with the kit in `plugins/kit`. They
are a separate workspace because they target `wasm32-wasip2`:

```sh
rustup target add wasm32-wasip2
plugins/build
cargo test -p amafu-storage --test plugin_kit
```

The contract tests load the SPDK plugin and the hostile plugin from `plugins/target`. Generated bindings use
`unsafe`, so it is allowed only in the kit and in each plugin's `abi` module.

## Scale

```sh
cargo build --release -p amafu-node
cargo rig scale --nodes 1000 --vms 2000 --state-in-memory
```

The run starts three real core members on this machine and 1,000 simulated nodes, places the VMs, reads at the
requested rate and prints a report with the budgets it checked. Without `--state-in-memory` the core members keep
their state under `target/`; on a network block device that measures the disk rather than Amafu (0024). The run
needs about 2 GiB per core member and stops before the host runs out of memory.

## The dashboard

```sh
cd crates/dashboard/frontend
npm ci
npm run check
npm run build
cargo rig dashboard
```

`npm run build` writes the bundle into `crates/dashboard/assets`, which `amafu-dashboard` embeds, so a Rust build
never needs Node. Commit the rebuilt bundle with the sources. `cargo rig dashboard` serves it over a seeded cluster
on `http://127.0.0.1:8480` whose objects settle as a real cluster's would; sign in with `amafu-demo-admin`,
`amafu-demo-member` or `amafu-demo-viewer`. `npm run develop` serves the sources with hot reload, forwarding to
that cluster.

`cargo rig dashboard --browser-check` drives every page and the main workflows in headless Chromium and saves
screenshots in `target/dashboard-screenshots`. It needs Playwright's browser once:
`npx playwright install --with-deps chromium`.

## Fuzzing

| Crate | Targets |
|---|---|
| `crates/api/fuzz` | `create_body`, `update_body`, `node_swap`, `slice_change` |
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
(decision 0010). The first build also compiles the container guest kernel from the pinned kernel.org source in a
temporary directory (`make`, `gcc`, `flex`, `bison`, `bc` and `libelf-dev`, about a minute on 14 cores) and keeps
the result in `image/output/cache`. Assembling a lab disk with
`image/assemble-disk` also needs `sfdisk`, `mkfs.vfat`, `mtools`, `mkfs.ext4` and `qemu-img`.

The build leaves `esp/` (shim, GRUB and its configuration), `slot/` (what one boot slot holds), `node-image.tar` (the
slot as a bundle) and `boot-loader.tar` (the `esp/` files). `image/assemble-disk` installs the bundle into slot `a`
with its signed manifest (decision 0021). Images are signed with Ed25519:

```sh
amafu release create-key --private-key release.key        # prints the public key for upgrade.image_keys
amafu release sign --private-key release.key --bundle image/output/release/node-image.tar \
    --version 0.2.0 --feature-level 1 --url https://images.example/0.2.0/node-image.tar > node-image.json
```

## Upgrades

```sh
amafu upgrade start --image node-image.json --dry-run   # the waves and steps, without starting them
amafu upgrade start --image node-image.json --wait 7200
amafu upgrade status                                    # the image, slot and confirmation of every node
amafu upgrade rollback --wait 7200                       # the same waves backwards, to the previous image
amafu upgrade finalize                                  # enable the new features; rollback is refused after this
```

An upgrade is an operation, so `amafu operation show`, `pause`, `resume` and `cancel` apply to it.

## The virtual lab

The lab runs on an LXD server reachable as the `amafu-lab` remote of the snap's `lxc` client (its client
certificate and the pinned server certificate are read from `~/snap/lxd/common/config`). The defaults in
`test-rig/lxd-lab/src/topology.rs` describe that server's address, bridge and storage pool.

The lab (decision 0016) has four nodes, three of them core members, a lab switch VM that owns the underlay
`10.216.0.0/24`, an NFS server, an SPDK NVMe/TCP target and a MicroCeph VM. The first `lab up` builds the SPDK target
image, which takes a while and is kept for later labs.

```sh
image/build test
cargo rig lab up          # creates the project, the switch, the storage targets and boots the four nodes
cargo rig lab contract    # the hypervisor contract against the real Cloud Hypervisor adapter
cargo rig lab run         # every scenario at the virtual-lab level
cargo rig lab soak --forced-stops 50   # force nodes off again and again; each time their VMs must run elsewhere within 30 s
cargo rig lab instances   # VM processes on each node, with their console and VM process log
cargo rig lab inspect network      # ports and the nftables ruleset on each node
cargo rig lab inspect attachments  # volumes attached on each node
cargo rig lab inspect sandbox      # run the VM process sandbox probes
cargo rig lab down        # deletes the project
```

`lab up` signs three variants of the built bundle with a key it keeps in `target/rig/lab.json`: the installed image,
an upgrade that differs only in its release file, and a broken one whose initramfs is empty. The NFS VM serves them
on port 8080 for the upgrade scenarios.

`target/rig/lab.json` holds the lab's CA and tokens. To use the command line against the lab:

```sh
export AMAFU_ENDPOINT=https://10.0.10.13:17443 AMAFU_PROJECT=project-a
export AMAFU_CERTIFICATE_AUTHORITY=<the certificate_authority from lab.json, saved to a file>
export AMAFU_TOKEN=<a token from lab.json, for example operator@project-a>
amafu vm create web-1 --cpus 1 --memory 256 --kernel test-guest --wait
amafu vm migrate web-1 --wait          # live migration to a node the scheduler chooses
amafu node evacuate node-2 --wait      # cordon and move every VM off a node
amafu operation list                   # migrations, evacuations and recoveries with their progress
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
amafu image create noble --disk-url https://cloud-images.ubuntu.com/minimal/releases/noble/release-20261027/ubuntu-24.04-minimal-cloudimg-amd64.img \
    --sha256 3839f148c5a3b4a736e7dbb0af75594743fa5ebee2a202424ba1a32e5d3b5d2c
amafu volume create noble-root --size 4 --backend local --from-image noble
amafu vm create ubuntu --from-disk --volume noble-root --memory 1024 --network private --user-data cloud.yaml --wait
```

The lab's NFS export is `10.216.0.40:/srv/amafu` (`amafu storage-backend create shared --nfs 10.216.0.40:/srv/amafu`).

Milestone 3 kinds, from the command line:

```sh
amafu vm create web-1 --kernel test-guest --network private --anti-affinity-group web --priority 10 --wait
amafu external-network create lab --subnet 10.216.0.0/24 --first 10.216.0.100 --last 10.216.0.149
amafu floating-ip create public --external-network lab --vm web-1
amafu floating-ip move public --vm web-2
amafu storage-backend create fast --provider-file nvme.yaml    # an NvmeTcp or CephRbd provider, as in lab.json
amafu node list
```

`lab.json` holds the block backends the lab created (`block_backends`), with their credentials.

`cargo rig lab storage-contract` runs the storage provider contract on a lab node against every built-in provider
the lab has and against the SPDK plugin, built with `plugins/build`.

## The MAAS lab

The MAAS lab (decision 0022) installs nodes as customers will. It runs beside the virtual lab, on its own bridge
`amafu-maas0` (`10.217.0.0/24`) and in its own project: a MAAS VM, which also serves the images, and empty machines
that MAAS commissions through its LXD power driver. `amafu install` runs inside the MAAS VM, which reaches MAAS and
the nodes.

```sh
image/build test
cargo build --release -p amafu-command-line
cargo rig lab maas up --nodes 3   # MAAS, its DHCP, and three commissioned machines
cargo rig lab maas install        # amafu install, timed
cargo rig lab maas down
```

