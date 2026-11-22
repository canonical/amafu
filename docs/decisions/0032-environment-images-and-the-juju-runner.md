# 0032: Environment images are cluster settings; Juju on VMs bootstraps from an Ubuntu VM

Status: accepted for milestone 7; amends 0028

## Context

The Ubuntu image environments boot from and the Juju runner container were set in each node's configuration, which the installer writes once and leaves empty, so no installed cluster could set them. The runner container also had to be built and published to a registry before any Juju controller could exist.

## Decision

- One cluster-scoped `EnvironmentSettings` object named `cluster`, changed only by admins, holds the Ubuntu disk image and the Juju runner image. `amafu environment-settings set` takes the Ubuntu image as a catalog ID; the Settings page offers the same.
- A Juju controller on VMs is bootstrapped from a short-lived Ubuntu VM outside the controller's anti-affinity group. Cloud-init installs the `juju` snap for the controller's version and runs the same `bootstrap` script the container uses, from `/root` because the snap is strictly confined.
- The bootstrap is done when the controller answers. The runner does not power off: Amafu starts again any VM that should be running and whose guest stops. If no controller answers within 45 minutes, the change fails and names the runner, whose `/root/amafu-juju/bootstrap.log` holds the client's output.
- Juju on Kubernetes keeps the runner container, so only it needs the runner image.
- Platform signing keys are served at `/.well-known/jwks.json`, because Juju derives its discharge URL by removing that suffix from `login-token-refresh-url`.

## Consequences

- Juju on VMs needs no registry, at the cost of a VM boot and a snap install per bootstrap.
- The snap gives the newest patch release of the controller's minor version as the client; `--agent-version` still pins the controller.
- The runner VM's user data carries the controller's CA key and the bootstrap SSH key, as the container's environment did.
