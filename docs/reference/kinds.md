# Kinds

Generated from the kind registry by `cargo rig reference`. Do not edit it by hand.

| Kind | Path | Version | Scope | Who may change it |
|---|---|---|---|---|
| Node | `/v1/nodes` | 1 | cluster | nodes, and operators for their specification |
| NodeLease | `/v1/node-leases` | 1 | cluster | admins |
| StorageBackend | `/v1/storage-backends` | 1 | cluster | operators |
| ExternalNetwork | `/v1/external-networks` | 1 | cluster | operators |
| CertificateRevocation | `/v1/certificate-revocations` | 1 | cluster | admins |
| ClusterVersion | `/v1/cluster-versions` | 1 | cluster | admins |
| AuditSink | `/v1/audit-sinks` | 1 | cluster | admins |
| Operation | `/v1/operations` | 1 | cluster | operators |
| Vm | `/v1/vms` | 1 | project | members of the project |
| Image | `/v1/images` | 1 | project | members of the project |
| Volume | `/v1/volumes` | 1 | project | members of the project |
| Network | `/v1/networks` | 1 | project | members of the project |
| SecurityGroup | `/v1/security-groups` | 1 | project | members of the project |
| NetworkEndpoint | `/v1/network-endpoints` | 1 | project | nodes |
| FloatingIp | `/v1/floating-ips` | 1 | project | members of the project |
| ServiceAccount | `/v1/service-accounts` | 1 | project | members of the project |
| SigningKey | `/v1/signing-keys` | 1 | cluster | admins |
| SshKey | `/v1/ssh-keys` | 1 | project | members of the project |
| SshKeyGroup | `/v1/ssh-key-groups` | 1 | project | members of the project |
| EnvironmentSecret | `/v1/environment-secrets` | 1 | cluster | admins |
| KubernetesCluster | `/v1/kubernetes-clusters` | 1 | project | nodes |
| KubernetesClusterChange | `/v1/kubernetes-cluster-changes` | 1 | project | members of the project |
| JujuController | `/v1/juju-controllers` | 1 | project | nodes |
| JujuControllerChange | `/v1/juju-controller-changes` | 1 | project | members of the project |
