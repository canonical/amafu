use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use amafu_domain::Sha256Digest;
use amafu_interfaces::{Installation, MachineName, MachineProvisioner, MachineState, ProvisioningError};
use base64::Engine;

use crate::certificates::CertificateAuthority;

const DEPLOY_POLL: Duration = Duration::from_secs(15);
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(3600);
const DASHBOARD_PORT: u16 = 443;
const FIRST_PRIORITY: u32 = 150;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementPlan {
    pub machines: Vec<MachineName>,
    pub virtual_address: String,
    pub interface: String,
    pub core_endpoints: Vec<String>,
    pub command_line_url: String,
    pub command_line_digest: Sha256Digest,
    pub session_key: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ManagementError {
    #[error(transparent)]
    Provisioning(#[from] ProvisioningError),
    #[error(transparent)]
    Issuance(#[from] crate::certificates::IssuanceError),
    #[error("{0} failed to deploy: {1}")]
    Failed(MachineName, String),
    #[error("{0} was not deployed within an hour")]
    TimedOut(MachineName),
    #[error("the virtual address {0} is not an IPv4 address with a prefix length")]
    MalformedAddress(String),
}

fn encoded(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn keepalived(plan: &ManagementPlan, priority: u32) -> String {
    format!(
        "vrrp_script dashboard {{\n  script \"/usr/bin/curl --silent --insecure --max-time 2 --output /dev/null https://127.0.0.1:{DASHBOARD_PORT}/\"\n  interval 2\n  fall 2\n  rise 1\n}}\nvrrp_instance dashboard {{\n  state BACKUP\n  interface {}\n  virtual_router_id 61\n  priority {priority}\n  advert_int 1\n  virtual_ipaddress {{\n    {}\n  }}\n  track_script {{\n    dashboard\n  }}\n}}\n",
        plan.interface, plan.virtual_address
    )
}

fn unit(plan: &ManagementPlan) -> String {
    let endpoints: Vec<String> =
        plan.core_endpoints.iter().map(|endpoint| format!(" --core-endpoint {endpoint}")).collect();
    let endpoints = endpoints.concat();
    format!(
        "[Unit]\nDescription=Amafu dashboard\nAfter=network-online.target\n[Service]\nExecStart=/usr/local/bin/amafu dashboard serve --listen 0.0.0.0:{DASHBOARD_PORT} --certificate-authority /etc/amafu/cluster-authority.pem --session-key-file /etc/amafu/session-key --certificate /etc/amafu/dashboard.pem --private-key /etc/amafu/dashboard-key.pem{endpoints}\nRestart=always\n[Install]\nWantedBy=multi-user.target\n"
    )
}

#[must_use]
pub fn management_script(
    plan: &ManagementPlan,
    priority: u32,
    authority_pem: &str,
    certificate_pem: &str,
    key_pem: &str,
) -> String {
    let files = [
        ("/etc/amafu/cluster-authority.pem", authority_pem.to_owned(), "0644"),
        ("/etc/amafu/dashboard.pem", certificate_pem.to_owned(), "0644"),
        ("/etc/amafu/dashboard-key.pem", key_pem.to_owned(), "0600"),
        ("/etc/amafu/session-key", plan.session_key.clone(), "0600"),
        ("/etc/systemd/system/amafu-dashboard.service", unit(plan), "0644"),
        ("/etc/keepalived/keepalived.conf", keepalived(plan, priority), "0644"),
    ];
    let writes: Vec<String> = files
        .iter()
        .map(|(path, contents, mode)| {
            format!("install -d \"$(dirname {path})\"\necho '{}' | base64 --decode > {path}\nchmod {mode} {path}\n", encoded(contents))
        })
        .collect();
    let writes = writes.concat();
    format!(
        "#!/bin/bash\nset -euo pipefail\napt-get update -q\napt-get install -y -q keepalived curl\ncurl --fail --silent --show-error --location --output /usr/local/bin/amafu '{}'\necho '{}  /usr/local/bin/amafu' | sha256sum --check --quiet\nchmod 0755 /usr/local/bin/amafu\n{writes}systemctl daemon-reload\nsystemctl enable --now amafu-dashboard keepalived\n",
        plan.command_line_url,
        plan.command_line_digest.hexadecimal()
    )
}

fn virtual_ip(plan: &ManagementPlan) -> Result<IpAddr, ManagementError> {
    let (address, prefix) = plan
        .virtual_address
        .split_once('/')
        .ok_or_else(|| ManagementError::MalformedAddress(plan.virtual_address.clone()))?;
    prefix.parse::<u8>().map_err(|_| ManagementError::MalformedAddress(plan.virtual_address.clone()))?;
    address.parse().map_err(|_| ManagementError::MalformedAddress(plan.virtual_address.clone()))
}

pub async fn enable_dashboard(
    plan: &ManagementPlan,
    authority: &CertificateAuthority,
    provisioner: Arc<dyn MachineProvisioner>,
) -> Result<(), ManagementError> {
    let address = virtual_ip(plan)?;
    let identity = authority.issue_server("amafu-dashboard", &[address])?;
    for (machine, priority) in plan.machines.iter().zip((0..).map(|index| FIRST_PRIORITY - index * 10)) {
        let user_data = management_script(
            plan,
            priority,
            &authority.certificate_pem,
            &identity.certificate_pem,
            &identity.private_key_pem,
        );
        if matches!(provisioner.state(machine).await?, MachineState::Ready | MachineState::Allocated) {
            provisioner.deploy(machine, &Installation { user_data, is_in_memory: false }).await?;
        }
    }
    for machine in &plan.machines {
        let started = std::time::Instant::now();
        loop {
            match provisioner.state(machine).await? {
                MachineState::Deployed => break,
                MachineState::Failed { reason } => {
                    return Err(ManagementError::Failed(machine.clone(), reason));
                }
                _ if started.elapsed() >= DEPLOY_TIMEOUT => {
                    return Err(ManagementError::TimedOut(machine.clone()));
                }
                _ => tokio::time::sleep(DEPLOY_POLL).await,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> ManagementPlan {
        ManagementPlan {
            machines: vec![MachineName(String::from("infra-0"))],
            virtual_address: String::from("10.0.0.50/24"),
            interface: String::from("eth0"),
            core_endpoints: vec![
                String::from("https://10.0.0.10:7443"),
                String::from("https://10.0.0.11:7443"),
            ],
            command_line_url: String::from("https://releases.example/amafu"),
            command_line_digest: "a".repeat(64).parse().unwrap(),
            session_key: "b".repeat(64),
        }
    }

    #[test]
    fn each_management_machine_serves_the_dashboard_behind_the_shared_address() {
        let plan = plan();
        assert_eq!(virtual_ip(&plan).unwrap().to_string(), "10.0.0.50");
        let script = management_script(&plan, 150, "authority", "certificate", "key");
        assert!(script.contains(&encoded(&keepalived(&plan, 150))));
        assert!(keepalived(&plan, 150).contains("10.0.0.50/24"));
        assert!(
            unit(&plan)
                .contains("--core-endpoint https://10.0.0.10:7443 --core-endpoint https://10.0.0.11:7443")
        );
        assert!(script.contains(&"a".repeat(64)));
        assert!(!script.contains(&"b".repeat(64)));
        let malformed = ManagementPlan { virtual_address: String::from("10.0.0.50"), ..plan };
        assert!(virtual_ip(&malformed).is_err());
    }
}
