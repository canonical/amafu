# Amafu ☁️

**Just a kernel and one binary. Your servers become one cloud, and any VM or container runs on any node.**

*Amafu* is Zulu for "clouds".

> **Status: design phase.** Nothing here runs yet. Read the design doc and open an issue to discuss it.

## Design goals

1. ⚡ **Fast installation:** two `amafu` commands deploy a whole cluster through MAAS.
2. 🪶 **Tiny host footprint:** a kernel and one binary.
3. 🔌 **Hardware-agnostic workloads:** virtio devices and a shared CPU baseline per pool.
4. 🚀 **Fast live migration:** shared storage, compressed multi-stream transfer.
5. 🧩 **Clear, modular code:** a small set of mechanisms, simulation-tested.

## The problem

Running your own cloud today means stacking a full Linux distribution, a hypervisor, a storage stack, a network stack and a control plane on **every host**, then keeping them all patched.

## The idea

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/948394e9-a829-4d16-aef6-191ea8732e04">
     <img alt="image" src="https://github.com/user-attachments/assets/23030190-d582-40e9-84d7-3cfc8c4f0bfa" />
  </picture>
</p>

- **No host to manage.** No shell, no SSH, no packages. One signed image, configured through an API.
- **Every workload is a VM.** VMs, system containers and OCI containers all run in (micro)VMs, so all of them can live-migrate consistently.
- **The cluster is the OS.** Nodes are interchangeable; state lives in the cluster, not on any one machine.

## How it fits together

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/67c49605-fc78-44ec-80c4-9af59429442c">
     <img alt="image" src="https://github.com/user-attachments/assets/8b5b5875-4a9d-456f-96aa-295c536cb79e" />
  </picture>
</p>

| Piece | Built on |
| --- | --- |
| Hypervisor | [Cloud Hypervisor](https://github.com/cloud-hypervisor/cloud-hypervisor) / rust-vmm |
| Cluster state | [Quorium](https://github.com/canonical/quorium) (Raft) |
| Storage plugins | WebAssembly components ([Wasmtime](https://wasmtime.dev)), loaded at runtime |
| Networking | Overlay with per-node DHCP, DNS and gateway |
| Provisioning | [MAAS](https://maas.io), driven by the `amafu` CLI |
| Everything else | Rust |

## Install with MAAS (planned)

Four steps. MAAS already knows your machines, their zones, disks and networks, so `amafu` reads everything from MAAS and asks you for almost nothing.

```sh
# 1. Install MAAS on an infra node  (see maas.io)
sudo snap install maas && sudo maas init ...

# 2. Commission compute nodes in MAAS, then tag the ones Amafu should use
maas admin tags create name=amafu
maas admin tag update-nodes amafu add=<system_id> ...      # or use the MAAS UI

# 3. Install the amafu CLI on the infra node
sudo snap install amafu

# 4. Connect to MAAS and deploy
amafu maas connect http://localhost:5240/MAAS --api-key "$(sudo maas apikey --username admin)"
amafu cluster create prod-east --tag amafu
```

`amafu cluster create` does the rest:

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/26e1202c-9fe7-427a-8202-8a9080233b74">
     <img alt="image" src="https://github.com/user-attachments/assets/3fc9f73a-2bc6-4b7a-9afe-1fb772a69331" />
  </picture>
</p>

```
$ amafu cluster create prod-east --tag amafu
Checking 12 tagged machines ........................ ok
Uploading image custom/amafu-26.04.................. ok
Creating cluster CA ................................ ok
Voters: node-1 (zone-a), node-2 (zone-b), node-3 (zone-c)
Deploying voters ................................... 3/3 ready
Deploying remaining nodes .......................... 9/9 ready
Cluster prod-east is ready: 12 nodes, 3 zones. Credentials saved for `amafu`.
```

What `amafu` takes from MAAS, so you don't have to configure it:

| Amafu needs | Taken from MAAS |
| --- | --- |
| Which machines | Tag `amafu` (or `--pool <resource-pool>`) |
| Zones for spreading voters and VMs | MAAS availability zones |
| Host networking (interfaces, bonds, VLANs, IPs) | The machine's MAAS network config |
| Which network carries what traffic | MAAS spaces named `amafu-storage`, `amafu-migration`, `amafu-overlay` (optional; default: the boot interface) |
| Seed addresses | MAAS DNS names |
| Install disk | The machine's MAAS boot disk |

Optional tag: `amafu-voter` pins voter placement.

Day 2:

```sh
amafu node add --tag amafu          # deploy newly tagged machines
amafu node remove node-7            # drain by live migration, then release in MAAS
amafu cluster status
```

How it works under the hood: the image carries a [curtin hook](packaging/maas/curtin-hooks) that hands MAAS's credentials and network config to Amafu during deployment, so nothing needs installing on the MAAS server.

## Using Amafu (planned)

Everything (storage backends, networks, floating IPs, volumes, VMs and containers) is a resource in one cluster API. The `amafu` CLI and the dashboard are two clients of that API, so anything you click can also be scripted.

### CLI

```sh
amafu storage add ceph-prod --plugin ceph --config ceph.yaml
amafu network create app --subnet 10.10.0.0/24
amafu vm create web-1 --image ubuntu-24.04 --cpu 4 --memory 8G --disk 50G --network app
amafu container create api   --image ghcr.io/acme/api:1.4 --network app --port 8080   # OCI
amafu container create build --image debian-13 --network app                         # system
amafu fip create --network public --attach web-1
amafu vm migrate web-1 --to node-7
amafu get vms -o yaml > vms.yaml && amafu apply -f vms.yaml
```

Containers need no type flag: an OCI reference (`ghcr.io/...`, `nginx:1.27`) runs as an application container, and a system image from Amafu's catalog (`debian-13`) runs as a system container. `--oci` and `--system` override the guess if ever needed.

### Dashboard

The dashboard ships in the `amafu` snap and runs on the infra node next to MAAS, not on the compute nodes, so hypervisor hosts stay minimal and the dashboard keeps working during a cluster incident.

<img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/82d2560c-2be3-45b4-ab08-839e78b83cf7" />

<table>
  <tr>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/89dff31c-0eb4-4f5e-8860-99b45b97196d" /></td>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/b3314a1e-c53c-46f4-8775-d30260094b2a" /></td>
  </tr>
  <tr>
    <td align="center">Virtual machines</td>
    <td align="center">Containers, with the create panel</td>
  </tr>
  <tr>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/ae8f8226-3cdd-4864-84af-0da292354af2" /></td>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/73f3a9c0-e2fe-41e4-91b3-7ab05526daf0" /></td>
  </tr>
  <tr>
    <td align="center">Storage backends</td>
    <td align="center">Networks</td>
  </tr>
  <tr>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/b563b0ba-c8e4-4960-994e-8f9c6e85e53e" /></td>
    <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/4b883403-f499-4f4d-a55b-cf83aa77c255" /></td>
  </tr>
  <tr>
    <td align="center">Floating IPs</td>
    <td align="center">Security groups</td>
  </tr>
  <tr>
     <td><img width="1440" height="1042" alt="image" src="https://github.com/user-attachments/assets/23aca3de-3f32-42b1-a2f3-bfe8d68dea9a" /></td>
     <td><img width="1440" height="960" alt="image" src="https://github.com/user-attachments/assets/27fb680f-1ec4-4cfb-9ca2-b390689598b4" /></td>
  </tr>
  <tr>
     <td align="center">Nodes</td>
     <td align="center">Events</td>
  </tr>
</table>

## Upgrade (planned)

Two steps. Nodes upgrade in place one by one while their VMs live-migrate out of the way, so workloads never stop.

```sh
# 1. Refresh the CLI to the new release
sudo snap refresh amafu --channel=26.10/stable

# 2. Upload the new image to MAAS and roll it out
amafu upgrade
```

```
$ amafu upgrade
Cluster prod-east: 26.04 -> 26.10, 12 nodes
Pre-flight: health, spare capacity, version skew ................ ok
Backing up cluster state to this machine ........................ ok
Uploading image custom/amafu-26.10 to MAAS ...................... ok
Staging 26.10 on every node (inactive slot) ..................... 12/12
Canary node-4 ................................................... healthy
Non-voting nodes, one zone at a time ............................ 8/8
Voters, one at a time ........................................... 3/3
All nodes on 26.10. Soaking for 1h, then finalizing.
Roll back any time before then with: amafu upgrade rollback
```

Each node goes through the same cycle. The new version is only made permanent after it proves healthy:

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/10d935e9-c126-420f-9903-12a96cc58be5">
     <img alt="image" src="https://github.com/user-attachments/assets/8b6da558-54b9-40a9-b2e0-5ff33930637a" />
  </picture>
</p>

| Risk | Safeguard |
| --- | --- |
| VM downtime | VMs live-migrate off each node first, onto nodes already upgraded, so each VM moves once |
| A bad release | One canary node goes first; any failed health check pauses the whole upgrade |
| A node that doesn't come back | Each node has two image slots (A/B). The old image stays the default until the new one passes its checks, so a crash or watchdog reboot lands back on the old version |
| Losing quorum | Voters go last, one at a time, only while every other voter is healthy |
| Running out of capacity | One node drains at a time per zone, and only if the rest of the cluster can hold its VMs |
| Mixed versions mid-upgrade | Each release works with the previous one. New features and state formats stay off until the upgrade is finalized |

### Rollback

A node that fails its health checks rolls itself back automatically. To roll back the whole cluster, for example after a regression found during the soak, use `amafu upgrade rollback` before the upgrade is finalized: every node still has the old version in its other slot and no state has changed format, so it is the same rolling process in reverse.

```sh
amafu upgrade status          # progress, per-node health, why it paused
amafu upgrade pause           # stop after the current node
amafu upgrade resume
amafu upgrade rollback        # roll every node back to 26.04
amafu upgrade finalize        # skip the soak and finalize now
```

During a cluster rollback each VM restarts once, one at a time in restart-priority order, because a VM can't live-migrate from a newer hypervisor to an older one. Releases that pass a backward-migration test in CI are marked *live rollback*, and for those VMs move back without restarting.

Finalizing turns on the new release's features and state formats and makes MAAS deploy 26.10 for new machines. After that, problems are fixed forward with a patch release through the same `amafu upgrade`; restoring the pre-upgrade state backup is the last resort.

Upgrades go one release at a time (26.04 to 26.10, not 26.04 to 27.04); `amafu upgrade` refuses to skip a release.
