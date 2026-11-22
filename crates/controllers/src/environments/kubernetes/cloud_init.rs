use std::net::Ipv4Addr;

use amafu_domain::kinds::kubernetes_cluster::KubernetesVersion;
use amafu_domain::platform_access::SERVICE_SUBJECT;
use serde_json::{Value, json};

use crate::environments::CertificateAuthority;

const CONFIGURATION_FILE: &str = "/root/k8s-configuration.yaml";
const CONTROLLER_BINDING: &str = "amafu-controller";
const API_WAIT_SECONDS: u32 = 900;
const RETRY_SECONDS: u32 = 5;
const AUTHENTICATION_FILE: &str = "authentication.yaml";
const ARGUMENTS_DIRECTORY: &str = "/var/snap/k8s/common/args/conf.d";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuerTrust {
    pub issuer: String,
    pub discovery_url: String,
    pub certificate_authority: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPlane<'a> {
    pub api_address: Ipv4Addr,
    pub audience: &'a str,
    pub trust: &'a IssuerTrust,
}

impl ControlPlane<'_> {
    fn authentication(&self) -> String {
        let configuration = json!({
            "apiVersion": "apiserver.config.k8s.io/v1",
            "kind": "AuthenticationConfiguration",
            "jwt": [{
                "issuer": {
                    "url": self.trust.issuer,
                    "discoveryURL": self.trust.discovery_url,
                    "certificateAuthority": self.trust.certificate_authority,
                    "audiences": [self.audience],
                },
                "claimMappings": {
                    "username": {"claim": "sub", "prefix": ""},
                    "groups": {"claim": "groups", "prefix": ""},
                },
            }],
        });
        yaml(&configuration)
    }

    fn settings(&self) -> serde_json::Map<String, Value> {
        let mut settings = serde_json::Map::new();
        settings.insert(String::from("extra-sans"), json!([self.api_address.to_string()]));
        settings.insert(
            String::from("extra-node-config-files"),
            json!({ AUTHENTICATION_FILE: self.authentication() }),
        );
        settings.insert(
            String::from("extra-node-kube-apiserver-args"),
            json!({ "--authentication-config": format!("{ARGUMENTS_DIRECTORY}/{AUTHENTICATION_FILE}") }),
        );
        settings
    }
}

#[must_use]
pub fn bootstrap(
    version: &KubernetesVersion,
    control_plane: &ControlPlane<'_>,
    authority: &CertificateAuthority,
    k8sd_token: &str,
) -> String {
    let mut settings = control_plane.settings();
    settings.insert(
        String::from("cluster-config"),
        json!({
            "network": {"enabled": true},
            "dns": {"enabled": true},
            "local-storage": {"enabled": true, "default": true},
        }),
    );
    settings.insert(String::from("ca-crt"), json!(authority.certificate_pem));
    settings.insert(String::from("ca-key"), json!(authority.private_key_pem));
    user_data(
        version,
        Some(&Value::Object(settings)),
        &[
            command(&["k8s", "bootstrap", "--file", CONFIGURATION_FILE]),
            command(&["k8s", "x-capi", "set-auth-token", k8sd_token]),
            until_it_succeeds(&format!(
                "k8s kubectl get clusterrolebinding {CONTROLLER_BINDING} || k8s kubectl create clusterrolebinding {CONTROLLER_BINDING} --clusterrole=cluster-admin --user={SERVICE_SUBJECT}"
            )),
        ],
    )
}

#[must_use]
pub fn join(version: &KubernetesVersion, token: &str, control_plane: Option<&ControlPlane<'_>>) -> String {
    let settings = control_plane.map(|control_plane| Value::Object(control_plane.settings()));
    let mut arguments = vec!["k8s", "join-cluster", token];
    if settings.is_some() {
        arguments.extend(["--file", CONFIGURATION_FILE]);
    }
    user_data(version, settings.as_ref(), &[command(&arguments)])
}

fn command(arguments: &[&str]) -> Value {
    json!(arguments)
}

fn until_it_succeeds(script: &str) -> Value {
    let attempts = API_WAIT_SECONDS / RETRY_SECONDS;
    command(&[
        "sh",
        "-c",
        &format!(
            "for attempt in $(seq {attempts}); do ({script}) && exit 0; sleep {RETRY_SECONDS}; done; exit 1"
        ),
    ])
}

fn user_data(version: &KubernetesVersion, settings: Option<&Value>, commands: &[Value]) -> String {
    let install =
        command(&["snap", "install", "k8s", "--classic", &format!("--channel={}", version.snap_channel())]);
    let runcmd: Vec<&Value> = std::iter::once(&install).chain(commands).collect();
    let mut document = json!({ "runcmd": runcmd });
    if let Some(settings) = settings {
        document["write_files"] =
            json!([{ "path": CONFIGURATION_FILE, "permissions": "0600", "content": yaml(settings) }]);
    }
    format!("#cloud-config\n{}", yaml(&document))
}

fn yaml(value: &Value) -> String {
    serde_yaml_ng::to_string(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust() -> IssuerTrust {
        IssuerTrust {
            issuer: String::from("https://amafu.internal"),
            discovery_url: String::from("https://10.20.0.10:8443/.well-known/openid-configuration"),
            certificate_authority: String::from("amafu-ca"),
        }
    }

    fn document(user_data: &str) -> Value {
        let body = user_data.strip_prefix("#cloud-config\n").unwrap();
        serde_yaml_ng::from_str(body).unwrap()
    }

    fn written_settings(document: &Value) -> Value {
        serde_yaml_ng::from_str(document["write_files"][0]["content"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn the_first_control_plane_node_bootstraps_with_amafu_as_a_trusted_issuer() {
        let trust = trust();
        let control_plane = ControlPlane {
            api_address: Ipv4Addr::new(203, 0, 113, 7),
            audience: "cluster-uid",
            trust: &trust,
        };
        let authority = CertificateAuthority {
            certificate_pem: String::from("ca"),
            private_key_pem: String::from("key"),
        };
        let document =
            document(&bootstrap(&KubernetesVersion::new(1, 34), &control_plane, &authority, "secret"));
        let commands = document["runcmd"].as_array().unwrap();
        assert_eq!(commands[0][4], "--channel=1.34-classic/stable");
        assert_eq!(commands[1][1], "bootstrap");
        assert_eq!(commands[2], json!(["k8s", "x-capi", "set-auth-token", "secret"]));
        let binding = commands[3][2].as_str().unwrap();
        assert_eq!((&commands[3][0], &commands[3][1]), (&json!("sh"), &json!("-c")));
        assert!(binding.starts_with("for attempt in $(seq 180); do (k8s kubectl get clusterrolebinding amafu-controller || k8s kubectl create clusterrolebinding amafu-controller --clusterrole=cluster-admin --user=amafu@amafu) && exit 0; sleep 5; done"));
        let settings = written_settings(&document);
        assert_eq!(settings["extra-sans"], json!(["203.0.113.7"]));
        assert_eq!(settings["ca-crt"], "ca");
        let authentication: Value = serde_yaml_ng::from_str(
            settings["extra-node-config-files"][AUTHENTICATION_FILE].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(authentication["jwt"][0]["issuer"]["audiences"], json!(["cluster-uid"]));
        assert_eq!(authentication["jwt"][0]["claimMappings"]["groups"]["claim"], "groups");
    }

    #[test]
    fn workers_join_with_their_token_only() {
        let document = document(&join(&KubernetesVersion::new(1, 34), "join-token", None));
        assert_eq!(document["runcmd"][1], json!(["k8s", "join-cluster", "join-token"]));
        assert!(document.get("write_files").is_none());
    }

    #[test]
    fn control_plane_nodes_join_with_the_issuer_configuration() {
        let trust = trust();
        let control_plane = ControlPlane {
            api_address: Ipv4Addr::new(203, 0, 113, 7),
            audience: "cluster-uid",
            trust: &trust,
        };
        let document = document(&join(&KubernetesVersion::new(1, 34), "join-token", Some(&control_plane)));
        assert_eq!(document["runcmd"][1][3], "--file");
        assert_eq!(written_settings(&document)["extra-sans"], json!(["203.0.113.7"]));
    }
}
