use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use amafu_domain::kinds::juju_controller::{JujuVersion, SignInMode};
use amafu_interfaces::kubernetes::KUBERNETES_API_PORT;
use serde_json::{Value, json};

pub const BOOTSTRAP_KEY_COMMENT: &str = "amafu-bootstrap";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Machine { address: Ipv4Addr, ssh_private_key: String },
    Cluster { kubeconfig: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPlan {
    pub controller: String,
    pub version: JujuVersion,
    pub sign_in: SignInMode,
    pub certificate: String,
    pub private_key: String,
    pub token_refresh_url: String,
    pub target: Target,
}

impl BootstrapPlan {
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::from([
            (String::from("JUJU_CONTROLLER_NAME"), self.controller.clone()),
            (String::from("JUJU_VERSION"), self.version.to_string()),
            (String::from("JUJU_CA_CERT"), self.certificate.clone()),
            (String::from("JUJU_CA_PRIVATE_KEY"), self.private_key.clone()),
            (String::from("JUJU_LOGIN_TOKEN_REFRESH_URL"), self.token_refresh_url.clone()),
            (
                String::from("JUJU_SIGN_IN"),
                String::from(match self.sign_in {
                    SignInMode::Amafu => "amafu",
                    SignInMode::Jaas => "jaas",
                }),
            ),
        ]);
        match &self.target {
            Target::Machine { address, ssh_private_key } => {
                environment.insert(String::from("JUJU_SUBSTRATE"), String::from("machine"));
                environment.insert(String::from("JUJU_MACHINE_ADDRESS"), address.to_string());
                environment.insert(String::from("JUJU_SSH_PRIVATE_KEY"), ssh_private_key.clone());
            }
            Target::Cluster { kubeconfig } => {
                environment.insert(String::from("JUJU_SUBSTRATE"), String::from("kubernetes"));
                environment.insert(String::from("JUJU_KUBECONFIG"), kubeconfig.clone());
            }
        }
        environment
    }
}

#[must_use]
pub fn controller_user_data(ssh_public_key: &str, amafu_certificate_authority: &str) -> String {
    let mut document = json!({ "ssh_authorized_keys": [ssh_public_key] });
    if !amafu_certificate_authority.is_empty() {
        document["ca_certs"] = json!({ "trusted": [amafu_certificate_authority] });
    }
    format!("#cloud-config\n{}", serde_yaml_ng::to_string(&document).unwrap_or_default())
}

#[must_use]
pub fn kubeconfig(server: Ipv4Addr, certificate_authority: &str, user: &str, token: &str) -> String {
    use base64::Engine;
    let authority = base64::engine::general_purpose::STANDARD.encode(certificate_authority);
    let document: Value = json!({
        "apiVersion": "v1",
        "kind": "Config",
        "clusters": [{"name": "host", "cluster": {
            "server": format!("https://{server}:{KUBERNETES_API_PORT}"),
            "certificate-authority-data": authority,
        }}],
        "users": [{"name": user, "user": {"token": token}}],
        "contexts": [{"name": "host", "context": {"cluster": "host", "user": user}}],
        "current-context": "host",
    });
    serde_yaml_ng::to_string(&document).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(target: Target) -> BootstrapPlan {
        BootstrapPlan {
            controller: String::from("observability"),
            version: JujuVersion::new(4, 2, 1),
            sign_in: SignInMode::Amafu,
            certificate: String::from("ca"),
            private_key: String::from("key"),
            token_refresh_url: String::from("https://amafu.example/.well-known/jwks.json"),
            target,
        }
    }

    #[test]
    fn the_runner_learns_its_target_from_its_environment() {
        let machine = plan(Target::Machine {
            address: Ipv4Addr::new(10, 1, 0, 4),
            ssh_private_key: String::from("ssh"),
        })
        .environment();
        assert_eq!(machine["JUJU_SUBSTRATE"], "machine");
        assert_eq!(machine["JUJU_MACHINE_ADDRESS"], "10.1.0.4");
        assert_eq!(machine["JUJU_LOGIN_TOKEN_REFRESH_URL"], "https://amafu.example/.well-known/jwks.json");
        let cluster = plan(Target::Cluster { kubeconfig: String::from("config") }).environment();
        assert_eq!(cluster["JUJU_SUBSTRATE"], "kubernetes");
        assert!(!cluster.contains_key("JUJU_SSH_PRIVATE_KEY"));
    }

    #[test]
    fn controller_machines_trust_the_bootstrap_key_and_amafu() {
        let user_data = controller_user_data("ssh-ed25519 AAAA amafu-bootstrap", "amafu-ca");
        let document: Value =
            serde_yaml_ng::from_str(user_data.strip_prefix("#cloud-config\n").unwrap()).unwrap();
        assert_eq!(document["ssh_authorized_keys"][0], "ssh-ed25519 AAAA amafu-bootstrap");
        assert_eq!(document["ca_certs"]["trusted"][0], "amafu-ca");
    }

    #[test]
    fn a_kubeconfig_holds_the_service_account_token() {
        let config: Value = serde_yaml_ng::from_str(&kubeconfig(
            Ipv4Addr::new(203, 0, 113, 7),
            "ca",
            "juju-observability",
            "token",
        ))
        .unwrap();
        assert_eq!(config["clusters"][0]["cluster"]["server"], "https://203.0.113.7:6443");
        assert_eq!(config["users"][0]["user"]["token"], "token");
    }
}
