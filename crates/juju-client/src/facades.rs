use std::collections::BTreeMap;

use amafu_interfaces::{ActionError, JujuMachine, JujuModel};
use serde_json::{Value, json};

const KNOWN_VERSIONS: &[(&str, &[u32])] = &[
    ("Application", &[20, 19]),
    ("Client", &[8, 7, 6]),
    ("Cloud", &[7]),
    ("HighAvailability", &[3, 2]),
    ("KeyManager", &[1]),
    ("MachineManager", &[11, 10, 9]),
    ("ModelManager", &[10, 9]),
    ("ModelUpgrader", &[1]),
];

const STARTED: &str = "started";
const UNAUTHORIZED: &str = "unauthorized access";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoginResult {
    pub controller_uuid: String,
    pub server_version: String,
}

#[derive(Debug, Clone, Default)]
pub struct Facades {
    login: LoginResult,
    offered: BTreeMap<String, Vec<u32>>,
}

impl Facades {
    #[must_use]
    pub fn from_login(response: &Value) -> Self {
        let offered = response["facades"]
            .as_array()
            .map(|facades| {
                facades
                    .iter()
                    .filter_map(|facade| {
                        let versions = facade["versions"]
                            .as_array()?
                            .iter()
                            .filter_map(|version| u32::try_from(version.as_u64()?).ok())
                            .collect();
                        Some((facade["name"].as_str()?.to_owned(), versions))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let controller_uuid = response["controller-tag"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("controller-")
            .to_owned();
        let server_version = response["server-version"].as_str().unwrap_or_default().to_owned();
        Self { login: LoginResult { controller_uuid, server_version }, offered }
    }

    #[must_use]
    pub fn login(&self) -> &LoginResult {
        &self.login
    }

    pub fn version(&self, facade: &str) -> Result<u32, ActionError> {
        let offered = self
            .offered
            .get(facade)
            .ok_or_else(|| ActionError::fatal(format!("the Juju controller offers no {facade} facade")))?;
        let known = KNOWN_VERSIONS
            .iter()
            .find(|(name, _)| *name == facade)
            .map_or(&[][..], |(_, versions)| *versions);
        known
            .iter()
            .find(|version| offered.contains(version))
            .or_else(|| offered.iter().max())
            .copied()
            .ok_or_else(|| ActionError::fatal(format!("the Juju controller offers no version of {facade}")))
    }
}

pub fn reply_result(facade: &str, request: &str, reply: &Value) -> Result<Value, ActionError> {
    if let Some(message) = reply["error"].as_str() {
        let code = reply["error-code"].as_str().unwrap_or_default();
        let error = format!("{facade}.{request} failed: {message}");
        return Err(if code == UNAUTHORIZED {
            ActionError::fatal(error)
        } else {
            ActionError::retryable(error)
        });
    }
    let response = reply.get("response").cloned().unwrap_or(Value::Null);
    if let Some(message) = first_refusal(&response) {
        return Err(ActionError::invalid(format!("{facade}.{request} refused the request: {message}")));
    }
    if let Some(message) = first_error(&response) {
        return Err(ActionError::retryable(format!("{facade}.{request} failed: {message}")));
    }
    Ok(response)
}

fn first_error(response: &Value) -> Option<String> {
    let results = response["results"].as_array().or_else(|| response["machines"].as_array())?;
    results.iter().find_map(|result| result["error"]["message"].as_str().map(str::to_owned))
}

fn first_refusal(response: &Value) -> Option<String> {
    let results = response["Results"].as_array()?;
    results.iter().find_map(|result| result["Errors"][0]["message"].as_str().map(str::to_owned))
}

#[must_use]
pub fn models(listing: &Value, details: &Value) -> Vec<JujuModel> {
    let versions: BTreeMap<String, String> = details["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    let model = &result["result"];
                    Some((model["uuid"].as_str()?.to_owned(), model["agent-version"].as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    listing["user-models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|entry| {
                    let model = &entry["model"];
                    let uuid = model["uuid"].as_str()?.to_owned();
                    Some(JujuModel {
                        name: model["name"].as_str()?.to_owned(),
                        version: versions.get(&uuid).cloned().unwrap_or_default(),
                        uuid,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn model_tags(listing: &Value) -> Value {
    let entities: Vec<Value> = listing["user-models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|entry| entry["model"]["uuid"].as_str())
                .map(|uuid| json!({"tag": format!("model-{uuid}")}))
                .collect()
        })
        .unwrap_or_default();
    json!({ "entities": entities })
}

#[must_use]
pub fn machines(status: &Value) -> Vec<JujuMachine> {
    let mut units: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(applications) = status["applications"].as_object() {
        for application in applications.values() {
            for (unit, details) in application["units"].as_object().into_iter().flatten() {
                if let Some(machine) = details["machine"].as_str() {
                    units.entry(machine.to_owned()).or_default().push(unit.clone());
                }
            }
        }
    }
    status["machines"]
        .as_object()
        .map(|machines| {
            machines
                .iter()
                .map(|(id, machine)| JujuMachine {
                    id: id.clone(),
                    instance_id: machine["instance-id"].as_str().unwrap_or_default().to_owned(),
                    is_started: machine["agent-status"]["status"] == STARTED,
                    units: units.remove(id).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn unit_tag(unit: &str) -> String {
    format!("unit-{}", unit.replace('/', "-"))
}

#[must_use]
pub fn placements(machines: &[String]) -> Vec<Value> {
    machines.iter().map(|machine| json!({"scope": "#", "directive": machine})).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_known_version_the_controller_offers_is_used() {
        let facades = Facades::from_login(&json!({
            "controller-tag": "controller-1234",
            "server-version": "4.2.1",
            "facades": [{"name": "Application", "versions": [19, 20, 21]}, {"name": "Brand", "versions": [5]}],
        }));
        assert_eq!(facades.login().controller_uuid, "1234");
        assert_eq!(facades.version("Application").unwrap(), 20);
        assert_eq!(facades.version("Brand").unwrap(), 5);
        assert!(facades.version("Missing").is_err());
    }

    #[test]
    fn errors_in_replies_and_bulk_results_are_reported() {
        let refused = json!({"request-id": 1, "error": "permission denied", "error-code": UNAUTHORIZED});
        assert!(matches!(reply_result("Client", "FullStatus", &refused), Err(ActionError::Fatal(_))));
        let bulk =
            json!({"request-id": 2, "response": {"results": [{"error": {"message": "no such unit"}}]}});
        assert!(matches!(reply_result("Application", "DestroyUnit", &bulk), Err(ActionError::Retryable(_))));
        let deployed =
            json!({"request-id": 4, "response": {"Results": [{"Errors": [{"message": "charm not found"}]}]}});
        assert!(matches!(
            reply_result("Application", "DeployFromRepository", &deployed),
            Err(ActionError::Invalid(_))
        ));
        let fine = json!({"request-id": 3, "response": {"results": [{}]}});
        assert!(reply_result("Application", "DestroyUnit", &fine).is_ok());
    }

    #[test]
    fn units_are_attached_to_the_machines_that_host_them() {
        let status = json!({
            "machines": {
                "0": {"instance-id": "manual:a", "agent-status": {"status": "started"}},
                "1": {"instance-id": "manual:b", "agent-status": {"status": "pending"}},
            },
            "applications": {"postgresql": {"units": {"postgresql/0": {"machine": "0"}}}},
        });
        let machines = machines(&status);
        assert_eq!(machines[0].units, ["postgresql/0"]);
        assert!(machines[0].is_started);
        assert!(!machines[1].is_started);
        assert_eq!(unit_tag("postgresql/0"), "unit-postgresql-0");
    }

    #[test]
    fn model_versions_come_from_model_info() {
        let listing = json!({"user-models": [{"model": {"name": "default", "uuid": "u1"}}]});
        let details = json!({"results": [{"result": {"uuid": "u1", "agent-version": "4.2.1"}}]});
        assert_eq!(model_tags(&listing), json!({"entities": [{"tag": "model-u1"}]}));
        assert_eq!(
            models(&listing, &details),
            [JujuModel {
                name: String::from("default"),
                uuid: String::from("u1"),
                version: String::from("4.2.1")
            }]
        );
    }
}
