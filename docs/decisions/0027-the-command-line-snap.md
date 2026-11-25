# 0027: The command line as a strictly confined snap

Status: accepted for milestone 6

## Decision

- **Operators get `amafu` as the snap `amafu`**, on base core26, with strict confinement.
- **Dashboard machines keep the plain binary** (until 0035 moved the dashboard onto the nodes). `amafu dashboard enable` has them download it from a URL and check
  its digest, because the service reads its keys from `/etc/amafu`, which a strictly confined snap cannot read.
- **It asks for `home`, `network` and `network-bind`.** `network` reaches MAAS and the cluster, `network-bind` lets
  `amafu dashboard serve` listen, and `home` reads install descriptions, keys and manifests and writes the
  installer's state directory. Nothing else is needed: the command runs no other program, verifies TLS with
  rustls against the cluster's authority or the roots it carries, and embeds the dashboard's assets.
- **The command links no node code.** The node configuration the installer writes lives in
  `amafu-node-configuration`, so the snap does not carry the hypervisor, storage or WebAssembly runtime.
- **The snap builds the committed `HEAD`** from git, versioned as the workspace version and the short commit, and is
  `devel` grade until the first release.

## Consequences

- Files the command reads or writes must be under the user's home directory, outside hidden folders, or in the
  current directory when that is in the home directory. The guides use `~/amafu-<cluster>` for state directories.
- The installer's state directory, with the cluster's authority and admin token, is written with the user's own
  permissions; removing the snap does not remove it.
- Each release publishes the plain binary beside the snap, for `amafu dashboard enable --command-line-url`.
- Builds for amd64 and arm64 are declared; only amd64 has been packed and run.
