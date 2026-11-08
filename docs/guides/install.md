# Installing a cluster

Amafu installs from MAAS. The installer runs on a machine that reaches both MAAS and the nodes' addresses, writes
each machine's disk through an in-memory Ubuntu, and becomes the cluster's first certificate authority (decision
0022).

## Before you start

- MAAS, with the machines commissioned and `Ready`, and an API key for a user who may deploy them.
- At least one machine with the `core` role; three core members, one per zone, survive the loss of any one.
- A signed node image: the bundle the image build produced, its manifest from `amafu release sign`, and the public
  half of the key that signed it. Put the bundle and the boot loader bundle where the machines can download them.
- Local NVMe on core members. Quorium writes on every change; on network storage the cluster is bound by the disk
  (decision 0024).

```sh
amafu release create-key --private-key release.key          # prints the public key
amafu release sign --private-key release.key --bundle node-image.tar \
    --version 26.04.1 --feature-level 3 --url https://images.example/node-image.tar > manifest.json
```

## The install description

```yaml
version: 1
cluster:
  name: prod-east
  image: { ... }                # the manifest amafu release sign printed
  boot_loader:
    url: https://images.example/boot-loader.tar
    digest: sha256:...
  image_keys: ["<public key>"]
  resolvers: [10.0.0.2]
maas:
  url: http://maas.example:5240/MAAS
  api_key: consumer:token:secret
machines:
  - hostname: node-1             # the machine's name in MAAS
    node: node-1                 # its name in Amafu
    roles: [core, compute]
    zone: zone-a
    underlay:
      interface: eno1
      address: 10.0.0.11/24
      gateway: 10.0.0.1
  - hostname: node-4
    node: node-4
    roles: [compute]
    zone: zone-a
    underlay: { interface: eno1, address: 10.0.0.14/24, gateway: 10.0.0.1 }
```

A machine may also have a `management` address that the installer reaches it on, when the underlay is not routed to
the installer.

## Install

```sh
amafu install --description cluster.yaml --state-directory ~/amafu-prod-east
```

The installer deploys core members first, then the other nodes, waits until each runs the image and confirms it,
and records the image as the cluster's. It keeps a journal in the state directory: if it stops, run the same command
again and it continues from the last finished step.

The state directory holds `cluster-credentials.json`, with the cluster's certificate authority and an admin token.
Keep it safe and readable only by you; anyone holding it administers the cluster.

## Connect

```sh
jq -r .authority.certificate_pem ~/amafu-prod-east/cluster-credentials.json > prod-east-ca.pem
export AMAFU_ENDPOINT=https://10.0.0.11:7443
export AMAFU_CERTIFICATE_AUTHORITY=prod-east-ca.pem
export AMAFU_TOKEN=$(jq -r .admin_token ~/amafu-prod-east/cluster-credentials.json)
amafu node list
amafu upgrade status
```

## The dashboard

The dashboard runs on management machines, not on the nodes, behind one virtual address (decision 0026).

```sh
amafu dashboard create-session-key --file dashboard-session.key
amafu dashboard enable --description cluster.yaml --state-directory ~/amafu-prod-east \
    --machine mgmt-1 --machine mgmt-2 --virtual-address 10.0.0.50/24 \
    --command-line-url https://images.example/amafu --command-line-digest sha256:... \
    --session-key-file dashboard-session.key
```

Users sign in with their own token; the dashboard can do only what they may.

## Storage and audit export

```sh
amafu storage-backend create local --local-disk
amafu storage-backend create archive --nfs 10.0.3.10:/srv/amafu
amafu storage-backend create flash --provider-file flash.yaml      # NVMe/TCP, Ceph RBD or a plugin
amafu audit-sink create siem --file syslog.yaml
```

See [storage plugins](storage-plugins.md) for backends provided by a vendor's plugin.
