# 0009: Images and volumes

Status: accepted for milestone 2

## Decision

- **An Image is fetched by the node and cached.** A disk image is a URL with a required SHA-256; a container image is
  an OCI reference pinned by digest. Nothing is trusted by name. Disk images are kept as downloaded (raw or qcow2);
  container images are converted to erofs (decision 0006).
- **Volumes cloned from an image are qcow2 overlays** whose backing file is the cached image, so a clone costs no
  copy and many volumes share one image. Empty volumes are qcow2 files of their size.
- **Storage backends** are cluster-wide objects managed by operators. `local-disk` keeps volumes on the data
  partition of the node that runs the VM. `nfs` keeps them on an export the node mounts with NFSv4.2; the export must
  allow root (`no_root_squash`), because the node hands each volume to its VM's own user.
- **A volume is reserved for one VM at a time.** The reservation is the volume's `attached_to`, written before the
  VM starts, and the compute controller attaches only what is reserved for its VM. The invariant "a volume is
  attached on at most one node" is checked during the scenarios.
- **Boot from disk** uses Cloud Hypervisor's EDK2 firmware (`CLOUDHV.fd`, release `ch-97eeb7b09`, pinned by
  digest). The NoCloud seed is passed in the SMBIOS system serial number (`ds=nocloud;s=http://169.254.169.254/`),
  which cloud-init in stock Ubuntu cloud images reads without configuration.

## Consequences

- A `local-disk` volume can only be used on the node that holds it; moving it is milestone 3.
- An image being deleted keeps its cache, and so stays, while a volume was cloned from it or a container VM boots from it: the cache is the volumes' backing file and the containers' root.
