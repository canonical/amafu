use std::fmt::Write;

use amafu_interfaces::network::bridge_name;
use amafu_interfaces::{NodeNetworkState, Segment};

const MARK: &str = "0x414d4150";
const CHAIN: &str = "platform_api";
const RATE_SET: &str = "platform_api_rate";
const CONNECTION_SET: &str = "platform_api_connections";
pub const NEW_CONNECTIONS_PER_SECOND: u32 = 20;
pub const NEW_CONNECTION_BURST: u32 = 40;
pub const CONNECTIONS_PER_GUEST: u32 = 64;
const RATE_MEMORY_SECONDS: u32 = 60;

pub struct PlatformApiRules<'state> {
    state: &'state NodeNetworkState,
}

impl<'state> PlatformApiRules<'state> {
    #[must_use]
    pub fn new(state: &'state NodeNetworkState) -> Self {
        Self { state }
    }

    fn segments(&self) -> impl Iterator<Item = &'state Segment> {
        let has_addresses = !self.state.platform_api.is_empty();
        self.state.segments.iter().filter(move |segment| has_addresses && segment.is_platform_api_allowed)
    }

    fn is_enabled(&self) -> bool {
        self.segments().next().is_some()
    }

    fn destinations(&self) -> String {
        let addresses: Vec<String> = self
            .state
            .platform_api
            .iter()
            .map(|address| format!("{} . {}", address.ip(), address.port()))
            .collect();
        format!("{{ {} }}", addresses.join(", "))
    }

    fn admissions(&self, output: &mut String) {
        let destinations = self.destinations();
        for segment in self.segments() {
            let bridge = bridge_name(&segment.key);
            let _ =
                writeln!(output, "\t\tiifname \"{bridge}\" ip daddr . tcp dport {destinations} jump {CHAIN}");
        }
    }

    pub fn declare(&self, output: &mut String) {
        if !self.is_enabled() {
            return;
        }
        let _ = writeln!(
            output,
            "\tset {RATE_SET} {{\n\t\ttype ipv4_addr\n\t\tflags dynamic\n\t\ttimeout {RATE_MEMORY_SECONDS}s\n\t}}"
        );
        let _ = writeln!(output, "\tset {CONNECTION_SET} {{\n\t\ttype ipv4_addr\n\t\tflags dynamic\n\t}}");
        let _ = writeln!(output, "\tchain {CHAIN} {{");
        let _ = writeln!(
            output,
            "\t\tct state new add @{CONNECTION_SET} {{ ip saddr ct count over {CONNECTIONS_PER_GUEST} }} reject with tcp reset"
        );
        let _ = writeln!(
            output,
            "\t\tct state new update @{RATE_SET} {{ ip saddr limit rate over {NEW_CONNECTIONS_PER_SECOND}/second burst {NEW_CONNECTION_BURST} packets }} drop"
        );
        let _ = writeln!(output, "\t\tmeta mark set {MARK}\n\t}}");
    }

    pub fn admit_to_node(&self, output: &mut String, bridges: &str) {
        if !self.is_enabled() {
            return;
        }
        self.admissions(output);
        let _ = writeln!(output, "\t\tiifname \"{bridges}\" meta mark {MARK} accept");
    }

    pub fn admit_through_node(&self, output: &mut String) {
        self.admissions(output);
    }

    #[must_use]
    pub fn infrastructure_exemption(&self) -> String {
        if self.is_enabled() { format!("meta mark != {MARK} ") } else { String::new() }
    }

    pub fn accept_through_node(&self, output: &mut String, bridges: &str) {
        if self.is_enabled() {
            let _ = writeln!(output, "\t\tiifname \"{bridges}\" meta mark {MARK} accept");
        }
    }

    pub fn masquerade(&self, output: &mut String, bridges: &str) {
        for segment in self.segments().filter(|segment| !segment.is_outbound_access_allowed) {
            let _ = writeln!(
                output,
                "\t\tip saddr {} meta mark {MARK} oifname != \"{bridges}\" masquerade",
                segment.subnet
            );
        }
    }
}
