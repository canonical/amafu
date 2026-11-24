use std::collections::BTreeMap;
use std::fmt::Write;
use std::net::Ipv4Addr;

use amafu_domain::kinds::juju_controller::{JujuVersion, SignInMode};
use amafu_interfaces::kubernetes::KUBERNETES_API_PORT;
use serde_json::{Value, json};

pub const LXD_CHANNEL: &str = "6/stable";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Pods { pods: u8, admin_password: String, trusted_certificate_authority: String },
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
            Target::Pods { pods, admin_password, trusted_certificate_authority } => {
                environment.insert(String::from("JUJU_SUBSTRATE"), String::from("lxd"));
                environment.insert(String::from("JUJU_PODS"), pods.to_string());
                environment.insert(String::from("JUJU_ADMIN_PASSWORD"), admin_password.clone());
                if !trusted_certificate_authority.is_empty() {
                    environment
                        .insert(String::from("JUJU_TRUSTED_CA"), trusted_certificate_authority.clone());
                }
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
const POD_SETUP_SCRIPT: &str = include_str!("../../../../../image/juju-pod/setup");
const BOOTSTRAP_DIRECTORY: &str = "/root/amafu-juju";
const POD_DIRECTORY: &str = "/root/amafu-pod";
pub const BOOTSTRAP_LOG: &str = "/root/amafu-juju/bootstrap.log";

fn shell_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn exports<'v>(variables: impl IntoIterator<Item = (&'v str, &'v str)>) -> String {
    variables.into_iter().fold(String::new(), |mut lines, (name, value)| {
        let _ = writeln!(lines, "export {name}={}", shell_quoted(value));
        lines
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodRole<'a> {
    Founder { bootstrap: Option<&'a BootstrapPlan> },
    Member { join_token: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodSetup<'a> {
    pub name: &'a str,
    pub role: PodRole<'a>,
    pub trusted_certificate: &'a str,
    pub amafu_certificate_authority: &'a str,
}

impl PodSetup<'_> {
    #[must_use]
    pub fn user_data(&self) -> String {
        let certificate = format!("{POD_DIRECTORY}/amafu.crt");
        let (role, join_token, bootstrap) = match &self.role {
            PodRole::Founder { bootstrap } => ("founder", None, *bootstrap),
            PodRole::Member { join_token } => ("member", Some(*join_token), None),
        };
        let mut variables = vec![
            ("AMAFU_POD_ROLE", role),
            ("AMAFU_POD_NAME", self.name),
            ("AMAFU_LXD_CHANNEL", LXD_CHANNEL),
            ("AMAFU_TRUSTED_CERTIFICATE", certificate.as_str()),
        ];
        variables.extend(join_token.map(|token| ("AMAFU_JOIN_TOKEN", token)));
        let environment = exports(variables);
        let mut files = vec![
            json!({"path": format!("{POD_DIRECTORY}/setup"), "permissions": "0700", "content": POD_SETUP_SCRIPT}),
            json!({"path": format!("{POD_DIRECTORY}/environment"), "permissions": "0600", "content": environment}),
            json!({"path": certificate, "permissions": "0644", "content": self.trusted_certificate}),
        ];
        let mut commands = vec![json!([
            "sh",
            "-c",
            format!("cd {POD_DIRECTORY} && . ./environment && ./setup > {POD_DIRECTORY}/setup.log 2>&1")
        ])];
        let mut document = json!({});
        if !self.amafu_certificate_authority.is_empty() {
            document["ca_certs"] = json!({ "trusted": [self.amafu_certificate_authority] });
        }
        if let Some(plan) = bootstrap {
            document["packages"] = json!(["squashfs-tools"]);
            files.extend(plan.files());
            commands.extend(plan.commands());
        }
        document["write_files"] = Value::Array(files);
        document["runcmd"] = Value::Array(commands);
        format!("#cloud-config\n{}", serde_yaml_ng::to_string(&document).unwrap_or_default())
    }
}

impl BootstrapPlan {
    fn files(&self) -> [Value; 2] {
        let environment = self.environment();
        let environment = exports(environment.iter().map(|(name, value)| (name.as_str(), value.as_str())));
        [
            json!({"path": format!("{BOOTSTRAP_DIRECTORY}/bootstrap"), "permissions": "0700", "content": BOOTSTRAP_SCRIPT}),
            json!({"path": format!("{BOOTSTRAP_DIRECTORY}/environment"), "permissions": "0600", "content": environment}),
        ]
    }

    fn commands(&self) -> [Value; 2] {
        let install = format!(
            "cd {BOOTSTRAP_DIRECTORY} && snap download juju --channel={} --basename=juju && unsquashfs -f -d client juju.snap bin/juju && install -m 0755 client/bin/juju /usr/local/bin/juju",
            self.version.snap_channel()
        );
        let bootstrap = format!(
            "cd {BOOTSTRAP_DIRECTORY} && mkdir -p work && . ./environment && HOME=/root TMPDIR={BOOTSTRAP_DIRECTORY}/work ./bootstrap > {BOOTSTRAP_LOG} 2>&1"
        );
        [json!(["sh", "-c", install]), json!(["sh", "-c", bootstrap])]
    }
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

    fn pods() -> Target {
        Target::Pods {
            pods: 3,
            admin_password: String::from("password"),
            trusted_certificate_authority: String::from("amafu-ca"),
        }
    }

    fn document(user_data: &str) -> Value {
        serde_yaml_ng::from_str(user_data.strip_prefix("#cloud-config\n").unwrap()).unwrap()
    }

    #[test]
    fn the_bootstrap_learns_its_target_from_its_environment() {
        let on_pods = plan(pods()).environment();
        assert_eq!(on_pods["JUJU_SUBSTRATE"], "lxd");
        assert_eq!(on_pods["JUJU_PODS"], "3");
        assert_eq!(on_pods["JUJU_ADMIN_PASSWORD"], "password");
        assert_eq!(on_pods["JUJU_TRUSTED_CA"], "amafu-ca");
        assert_eq!(on_pods["JUJU_LOGIN_TOKEN_REFRESH_URL"], "https://amafu.example/.well-known/jwks.json");
        let cluster = plan(Target::Cluster { kubeconfig: String::from("config") }).environment();
        assert_eq!(cluster["JUJU_SUBSTRATE"], "kubernetes");
        assert!(!cluster.contains_key("JUJU_ADMIN_PASSWORD"));
    }

    #[test]
    fn every_pod_sets_up_lxd_trusting_amafu_and_only_the_founder_bootstraps() {
        let member = PodSetup {
            name: "ctl-pod-1",
            role: PodRole::Member { join_token: "token" },
            trusted_certificate: "amafu-lxd-certificate",
            amafu_certificate_authority: "amafu-ca",
        };
        let joined = document(&member.user_data());
        assert_eq!(joined["ca_certs"]["trusted"][0], "amafu-ca");
        assert_eq!(joined["write_files"][0]["content"], POD_SETUP_SCRIPT);
        let environment = joined["write_files"][1]["content"].as_str().unwrap();
        assert!(environment.contains("export AMAFU_POD_ROLE='member'"));
        assert!(environment.contains("export AMAFU_POD_NAME='ctl-pod-1'"));
        assert!(environment.contains("export AMAFU_JOIN_TOKEN='token'"));
        assert_eq!(joined["write_files"][2]["content"], "amafu-lxd-certificate");
        assert_eq!(joined["runcmd"].as_array().unwrap().len(), 1);
        assert!(joined.get("packages").is_none());

        let mut secret = plan(pods());
        secret.private_key = String::from("it's secret");
        let first =
            PodSetup { name: "ctl-pod-0", role: PodRole::Founder { bootstrap: Some(&secret) }, ..member };
        let founded = document(&first.user_data());
        assert!(
            founded["write_files"][1]["content"]
                .as_str()
                .unwrap()
                .contains("export AMAFU_POD_ROLE='founder'")
        );
        assert_eq!(founded["write_files"][3]["content"], BOOTSTRAP_SCRIPT);
        let environment = founded["write_files"][4]["content"].as_str().unwrap();
        assert!(environment.contains(r"export JUJU_CA_PRIVATE_KEY='it'\''s secret'"));
        assert_eq!(founded["write_files"][4]["permissions"], "0600");
        let install = founded["runcmd"][1][2].as_str().unwrap();
        assert!(install.contains("snap download juju --channel=4.2/stable"));
        let bootstrap = founded["runcmd"][2][2].as_str().unwrap();
        assert!(bootstrap.ends_with("./bootstrap > /root/amafu-juju/bootstrap.log 2>&1"));
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
