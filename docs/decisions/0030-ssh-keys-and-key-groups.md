# 0030: SSH keys and key groups, granted at first placement

Status: accepted for milestone 7

## Context

People reach a VM over SSH with a key the guest accepts. Typing a key into every VM's cloud-init user data is
error-prone and does not share well across a team. A team wants to add a key once, from a paste, a fresh key pair
or a GitHub or Launchpad account, and to grant a group of keys to an instance in one step.

## Decision

- `SshKey` is a project kind holding one public key and where it came from: pasted, generated, or imported from a
  GitHub or Launchpad account. The key is parsed when it is admitted: one line, a supported type (Ed25519, ECDSA,
  their security-key forms, or RSA of at least 2048 bits) and data that names the same type. A private key is
  refused with a reason. The key cannot be changed after it is added.
- Amafu never holds a private key. Generating happens where the person is: the dashboard makes an Ed25519 pair
  with the browser's WebCrypto and downloads the private key, the command line writes it next to its `.pub` with
  mode 0600. Only the public key is sent.
- Importing is a snapshot. The API looks up the keys an account publishes, at
  `https://github.com/USER.keys` or `https://launchpad.net/~USER/+sshkeys`, and only returns them; the caller
  creates the keys it wants. Importing again adds new keys and names the ones the account withdrew, which
  `--prune` deletes. A key never follows an account by itself, so whoever controls that account cannot change who
  reaches an instance.
- `SshKeyGroup` is a project kind listing keys by name. A group holds references, so a key fixed once is fixed
  in every group.
- A VM, and so a container, names keys and groups in `ssh_keys`. Before it is placed, the authorized-keys
  controller resolves them, removes repeats by fingerprint and freezes the result in `status.authorized_keys`.
  The scheduler places a VM only once its keys are frozen, and the VM says which key or group is missing until
  then.
- A VM receives its frozen keys as `public-keys` in its NoCloud meta-data, kept apart from the user data so they
  never clash with a person's own cloud-config. A container's guest agent writes them to the workload user's
  `~/.ssh/authorized_keys` before the workload starts.
- Admission refuses a VM that names a key or group the project does not have, and refuses deleting a key or group
  that a group or a VM still waiting for its keys depends on.

## Consequences

- Keys apply at first boot. Changing or deleting a key or a group later does not reach instances that already
  have their keys, and the dashboard and the command line say so. Pushing key changes into running VMs would need
  an agent in every disk VM, which Amafu does not have.
- Deleting a key is safe for running instances: they keep the keys they were given, and their status records which.
- A container's keys matter only if its image runs an SSH server. A shell into any container is a separate
  feature, through the guest agent and the person's Amafu identity.
- Any project member can make the API fetch a GitHub or Launchpad page; the account name is checked so it cannot
  change the address, responses are capped at 256 KiB and ten seconds.
