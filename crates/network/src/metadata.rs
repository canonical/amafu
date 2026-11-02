use std::net::Ipv4Addr;

use amafu_interfaces::NodeNetworkState;

pub const METADATA_ADDRESS: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const EMPTY_CLOUD_CONFIG: &str = "#cloud-config\n{}\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub content_type: &'static str,
    pub body: String,
}

fn text(body: String) -> Document {
    Document { content_type: "text/plain", body }
}

#[must_use]
pub fn answer(state: &NodeNetworkState, source: Ipv4Addr, path: &str) -> Option<Document> {
    let segment = state.segments.iter().find(|segment| segment.subnet.contains(source))?;
    let port = state.port_with_address(&segment.key, source)?;
    match path.trim_end_matches('/') {
        "" => Some(text(String::from("meta-data\nuser-data\nvendor-data\n"))),
        "/meta-data" => {
            Some(text(format!("instance-id: {}\nlocal-hostname: {}\n", port.key.vm_id, port.hostname)))
        }
        "/user-data" => Some(text(port.user_data.clone().unwrap_or_else(|| EMPTY_CLOUD_CONFIG.to_owned()))),
        "/vendor-data" => Some(text(String::new())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dhcp::tests::state;

    #[test]
    fn a_vm_gets_its_own_documents() {
        let state = state();
        let meta = answer(&state, Ipv4Addr::new(10, 1, 0, 2), "/meta-data").unwrap();
        assert!(meta.body.contains("local-hostname: web-1"));
        let user = answer(&state, Ipv4Addr::new(10, 1, 0, 2), "/user-data/").unwrap();
        assert_eq!(user.body, EMPTY_CLOUD_CONFIG);
        assert!(answer(&state, Ipv4Addr::new(10, 1, 0, 2), "/").unwrap().body.contains("user-data"));
    }

    #[test]
    fn strangers_and_unknown_paths_get_nothing() {
        let state = state();
        assert_eq!(answer(&state, Ipv4Addr::new(10, 1, 0, 9), "/meta-data"), None);
        assert_eq!(answer(&state, Ipv4Addr::new(10, 9, 0, 2), "/meta-data"), None);
        assert_eq!(answer(&state, Ipv4Addr::new(10, 1, 0, 2), "/../etc/passwd"), None);
    }
}
