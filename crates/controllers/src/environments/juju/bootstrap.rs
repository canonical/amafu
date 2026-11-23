use std::collections::BTreeMap;
use std::fmt::Write;
use std::net::Ipv4Addr;

use amafu_domain::kinds::juju_controller::{JujuVersion, SignInMode};
use amafu_interfaces::kubernetes::KUBERNETES_API_PORT;
use serde_json::{Value, json};

pub const BOOTSTRAP_KEY_COMMENT: &str = "amafu-bootstrap";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Itself { ssh_private_key: String, admin_password: String },
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
            Target::Itself { ssh_private_key, admin_password } => {
                environment.insert(String::from("JUJU_SUBSTRATE"), String::from("machine"));
                environment.insert(String::from("JUJU_SSH_PRIVATE_KEY"), ssh_private_key.clone());
                environment.insert(String::from("JUJU_ADMIN_PASSWORD"), admin_password.clone());
            }
            Target::Cluster { kubeconfig } => {
                environment.insert(String::from("JUJU_SUBSTRATE"), String::from("kubernetes"));
                environment.insert(String::from("JUJU_KUBECONFIG"), kubeconfig.clone());
            }
        }
        environment
    }
}

const BOOTSTRAP_SCRIPT: &str = include_str!("../../../../../image/juju-runner/bootstrap");
const BOOTSTRAP_DIRECTORY: &str = "/root/amafu-juju";
pub const BOOTSTRAP_LOG: &str = "/root/amafu-juju/bootstrap.log";

fn shell_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

impl BootstrapPlan {
    #[must_use]
    pub fn controller_user_data(&self, ssh_public_key: &str, amafu_certificate_authority: &str) -> String {
        let environment = self.environment().iter().fold(String::new(), |mut lines, (name, value)| {
            let _ = writeln!(lines, "export {name}={}", shell_quoted(value));
            lines
        });
        let install = format!(
            "cd {BOOTSTRAP_DIRECTORY} && snap download juju --channel={} --basename=juju && unsquashfs -f -d client juju.snap bin/juju && install -m 0755 client/bin/juju /usr/local/bin/juju",
            self.version.snap_channel()
        );
        let bootstrap = format!(
            "cd {BOOTSTRAP_DIRECTORY} && mkdir -p work && . ./environment && JUJU_MACHINE_ADDRESS=$(hostname -I | cut -d ' ' -f 1) HOME=/root TMPDIR={BOOTSTRAP_DIRECTORY}/work ./bootstrap 2>&1 | tee {BOOTSTRAP_LOG}"
        );
        let mut document = machine_document(ssh_public_key, amafu_certificate_authority);
        document["packages"] = json!(["squashfs-tools"]);
        document["write_files"] = json!([
            {"path": format!("{BOOTSTRAP_DIRECTORY}/bootstrap"), "permissions": "0700", "content": BOOTSTRAP_SCRIPT},
            {"path": format!("{BOOTSTRAP_DIRECTORY}/environment"), "permissions": "0600", "content": environment},
        ]);
        document["runcmd"] = json!([["sh", "-c", install], ["sh", "-c", bootstrap]]);
        cloud_config(&document)
    }
}

fn machine_document(ssh_public_key: &str, amafu_certificate_authority: &str) -> Value {
    let mut document = json!({ "ssh_authorized_keys": [ssh_public_key] });
    if !amafu_certificate_authority.is_empty() {
        document["ca_certs"] = json!({ "trusted": [amafu_certificate_authority] });
    }
    document
}

fn cloud_config(document: &Value) -> String {
    format!("#cloud-config\n{}", serde_yaml_ng::to_string(document).unwrap_or_default())
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

    fn itself() -> Target {
        Target::Itself { ssh_private_key: String::from("ssh"), admin_password: String::from("password") }
    }

    #[test]
    fn the_bootstrap_learns_its_target_from_its_environment() {
        let machine = plan(itself()).environment();
        assert_eq!(machine["JUJU_SUBSTRATE"], "machine");
        assert_eq!(machine["JUJU_ADMIN_PASSWORD"], "password");
        assert!(!machine.contains_key("JUJU_MACHINE_ADDRESS"));
        assert_eq!(machine["JUJU_LOGIN_TOKEN_REFRESH_URL"], "https://amafu.example/.well-known/jwks.json");
        let cluster = plan(Target::Cluster { kubeconfig: String::from("config") }).environment();
        assert_eq!(cluster["JUJU_SUBSTRATE"], "kubernetes");
        assert!(!cluster.contains_key("JUJU_SSH_PRIVATE_KEY"));
    }

    #[test]
    fn the_first_controller_installs_the_client_and_bootstraps_itself_from_roots_home() {
        let mut machine = plan(itself());
        machine.private_key = String::from("it's secret");
        let user_data = machine.controller_user_data("ssh-ed25519 AAAA amafu-bootstrap", "amafu-ca");
        let document: Value =
            serde_yaml_ng::from_str(user_data.strip_prefix("#cloud-config\n").unwrap()).unwrap();
        assert_eq!(document["ssh_authorized_keys"][0], "ssh-ed25519 AAAA amafu-bootstrap");
        assert_eq!(document["ca_certs"]["trusted"][0], "amafu-ca");
        let install = document["runcmd"][0][2].as_str().unwrap();
        assert!(install.contains("snap download juju --channel=4.2/stable"));
        assert!(install.ends_with("install -m 0755 client/bin/juju /usr/local/bin/juju"));
        let bootstrap = document["runcmd"][1][2].as_str().unwrap();
        assert!(bootstrap.starts_with("cd /root/amafu-juju && "));
        assert!(bootstrap.contains("JUJU_MACHINE_ADDRESS=$(hostname -I | cut -d ' ' -f 1) HOME=/root"));
        assert!(bootstrap.ends_with("| tee /root/amafu-juju/bootstrap.log"));
        assert_eq!(document["write_files"][0]["content"], BOOTSTRAP_SCRIPT);
        let environment = document["write_files"][1]["content"].as_str().unwrap();
        assert!(environment.contains(r"export JUJU_CA_PRIVATE_KEY='it'\''s secret'"));
        assert_eq!(document["write_files"][1]["permissions"], "0600");
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
