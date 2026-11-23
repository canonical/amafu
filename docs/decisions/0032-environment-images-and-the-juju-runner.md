# 0032: Environment images are cluster settings; Juju on VMs bootstraps from its own machine

Status: accepted for milestone 7; amends 0028

## Context

The Ubuntu image environments boot from and the Juju runner container were set in each node's configuration, which the installer writes once and leaves empty, so no installed cluster could set them. The runner container also had to be built and published to a registry before any Juju controller could exist.

## Decision

- One cluster-scoped `EnvironmentSettings` object named `cluster`, changed only by admins, holds the Ubuntu disk image and the Juju runner image. `amafu environment-settings set` takes the Ubuntu image as a catalog ID; the Settings page offers the same.
- A Juju controller on VMs bootstraps itself: cloud-init on its first machine extracts the `juju` client from the snap for the controller's version (so it runs unconfined, as root from `/root`), bootstraps the machine over SSH to its own address with the one-time bootstrap key, raises Juju's bootstrap timeout to an hour, skips the package upgrade, and sets the admin password Amafu generated. No runner VM, no SSH between machines.
- Amafu logs in with that admin password until it knows the controller's UUID, which Juju chooses at bootstrap, and with its own signed tokens afterwards. Cleaning up the bootstrap removes the key from the controller and from Amafu's secret.
- The bootstrap is done when the controller answers. If none answers within an hour, the change fails and names the first controller machine, whose `/root/amafu-juju/bootstrap.log` holds the client's output.
- Juju on Kubernetes keeps the runner container, so only it needs the runner image.
- Platform signing keys are served at `/.well-known/jwks.json`, because Juju derives its discharge URL by removing that suffix from `login-token-refresh-url`.

## Consequences

- Juju on VMs needs no registry and no extra VM; each bootstrap still downloads the client, the agent and the database snap, which a baked image per Juju version would remove.
- The snap gives the newest patch release of the controller's minor version as the client; `--agent-version` still pins the controller.
- The first controller's user data carries the controller's CA key, the bootstrap SSH key and the admin password, as the container's environment did.
