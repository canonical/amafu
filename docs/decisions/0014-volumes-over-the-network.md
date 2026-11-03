# 0014: Volumes over the network, local volumes on their node, images on every node

Status: accepted for milestone 3 (extends 0009)

## Decision

- **NVMe/TCP volumes live on an SPDK target**, provisioned through its JSON-RPC API (behind SPDK's HTTP proxy) as
  thin logical volumes, one subsystem per volume, named `nqn.2026-11.io.amafu:volume:<id>`. Exporting a volume to a
  node allows that node's host name (`nqn.2026-11.io.amafu:node:<node>`) on the subsystem; revoking removes it. The
  attacher writes to `/dev/nvme-fabrics` once per target address and finds the namespace through sysfs; kernel
  multipath joins the paths. SPDK supports reservations, which fencing needs in milestone 4.
- **Ceph RBD volumes are created through the Ceph dashboard's REST API** and mapped with the kernel client through
  `/sys/bus/rbd/add_single_major`. The client key is passed in the map request; the kernel keyring would need
  `unsafe` system calls, which Amafu forbids.
- **Block volumes start empty.** Cloning a disk image into an NVMe or RBD volume needs a qcow2 reader on the node and
  is refused with a clear reason for now.
- **Local-disk volumes are created by the node that runs their first VM** (the local-volume controller), which
  records itself in the volume's provisioned state; the scheduler then pins later VMs to that node, and the same node
  frees the storage when the volume is deleted.
- **Every node caches every image** and adds itself to the image's `cached_on` list with a status update computed
  from the current object, so concurrent nodes never overwrite each other. Only core members, which see every user,
  remove a deleted image once no node caches it.
- **A volume is attached on at most one node** stays a global invariant and is checked while block volumes follow
  their VM between nodes.

## Consequences

- Backend credentials (the SPDK proxy's password, the Ceph client key and dashboard password) are stored in the
  storage backend's specification, which nodes receive. Secrets handed out by reference arrive with the secret
  store; this is a stand-in recorded in 0005.
