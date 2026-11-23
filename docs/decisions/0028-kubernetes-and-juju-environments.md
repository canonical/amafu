# 0028: Kubernetes clusters and Juju controllers as environments run by step plans

Status: accepted for milestone 7

## Decision

- **An environment is a kind Amafu manages; a change to it is a kind people create.** `KubernetesCluster` and
  `JujuController` are written only by core members. `KubernetesClusterChange` and `JujuControllerChange` are
  requests from project members, immutable once created, whose steps are derived from the request (a change sent
  without steps takes those of its request).
- **Changes run on the operations runner of decision 0017.** The runner is generic over any kind whose
  specification is a step plan; each change kind names its runner singleton (`kubernetes-changes`,
  `juju-changes`). Every step answers `is_complete`, `run` and optionally `undo`, and writes with the runner's
  fencing token.
- **One change per environment at a time.** Admission refuses a change while another for the same environment is
  unfinished, refuses creating an environment that exists and changing one that does not, refuses deleting a
  cluster that hosts a Juju controller, and refuses machine changes to a controller that runs on Kubernetes.
- **Objects an environment owns carry its label** (`amafu.io/kubernetes-cluster`, `amafu.io/juju-controller`, plus
  `amafu.io/juju-model`, `amafu.io/juju-machine` and `amafu.io/juju-application` on Juju machines). People cannot
  create, change or delete them directly; deletion of an environment removes everything with its label.
- **Steps never run commands inside existing VMs.** Kubernetes nodes bootstrap and join from cloud-init and are
  driven through k8sd's CAPI endpoints and the Kubernetes API (`amafu-kubernetes-client`). Juju controllers are
  bootstrapped from their own first machine on VMs and by a short-lived `image/juju-runner` container on Kubernetes (decision 0032) and driven over the Juju API
  (`amafu-juju-client`). Both clients sit behind ports in `amafu-interfaces` with fakes in the simulated world.
- **Secrets an environment needs live in `EnvironmentSecret`**, cluster scoped and readable only by admins, until a
  secret store exists (decision 0005): the k8sd token, the Kubernetes and Juju certificate authorities, the one-time
  bootstrap SSH key and the name of the Juju service account token.
- **Amafu signs platform tokens.** `SigningKey` objects hold ES256 keys rotated monthly and published a day ahead
  at `/v1/jwks`. Clusters trust Amafu through a structured authentication configuration; controllers through
  `login-token-refresh-url`. The API mints five-minute tokens for people (`/tokens` on each environment) and the
  controllers mint their own service tokens. The same keyring signs fifteen-minute API tokens for service accounts,
  bought with a client secret or a trusted workload token, and the API accepts them beside static tokens.
- **Background loops** move a cluster's API address off a stopped control-plane VM, mark a Juju controller degraded
  while its host cluster is not ready, and rotate a Kubernetes-hosted controller's cloud credential monthly.

## Consequences

- Core members need a route to the external networks floating IPs come from, and environment VMs need a route to
  the Amafu API address for discovery and keys (`environments.api_address` in the node configuration).
- k8sd's certificate is self-signed and not pinned: the CAPI token is sent to whatever answers on the API address.
- The cloud controller manager, the CSI driver, the Terraform provider and the Cluster API provider are separate Go
  repositories; until the CCM exists, `LoadBalancer` services get no address and Juju on Kubernetes waits for its
  controller service.
- Juju port sync, user sync, registrations for the juju CLI and image clean-up are not built yet.
- Everything is proven in the simulated world against fake platforms; no Kubernetes cluster or Juju controller has
  been created in the lab yet.
