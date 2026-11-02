use std::str::FromStr;

use amafu_api::ApiClient;
use amafu_domain::kinds::security_group::{
    Direction, PortRange, Protocol, Remote, SecurityGroup, SecurityGroupRule, SecurityGroupSpecification,
};
use amafu_domain::{Object, ObjectName};
use clap::Subcommand;

use crate::output::OutputFormat;
use crate::resources::{CommonCommand, Tabulated, create, kebab_name, run_common};

const RULE_FORMAT: &str =
    "DIRECTION,PROTOCOL,PORTS,REMOTE such as ingress,tcp,80,any or egress,udp,53,10.0.0.0/8";
const GROUP_PREFIX: &str = "group:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleArgument(SecurityGroupRule);

fn parse_direction(text: &str) -> Option<Direction> {
    match text {
        "ingress" => Some(Direction::Ingress),
        "egress" => Some(Direction::Egress),
        _ => None,
    }
}

fn parse_protocol(text: &str) -> Option<Protocol> {
    match text {
        "any" => Some(Protocol::Any),
        "tcp" => Some(Protocol::Tcp),
        "udp" => Some(Protocol::Udp),
        "icmp" => Some(Protocol::Icmp),
        _ => None,
    }
}

fn parse_ports(text: &str) -> Result<Option<PortRange>, std::num::ParseIntError> {
    if text.is_empty() {
        return Ok(None);
    }
    let (first, last) = text.split_once('-').unwrap_or((text, text));
    Ok(Some(PortRange { first: first.parse()?, last: last.parse()? }))
}

fn parse_remote(text: &str) -> Option<Remote> {
    if text == "any" {
        return Some(Remote::Any);
    }
    if let Some(group) = text.strip_prefix(GROUP_PREFIX) {
        return Some(Remote::SecurityGroup { name: group.parse().ok()? });
    }
    Some(Remote::Addresses { network: text.parse().ok()? })
}

impl FromStr for RuleArgument {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("{text:?} is not a rule; write {RULE_FORMAT}");
        let fields: Vec<&str> = text.split(',').collect();
        let [direction, protocol, ports, remote] = fields.as_slice() else {
            return Err(invalid());
        };
        Ok(Self(SecurityGroupRule {
            direction: parse_direction(direction).ok_or_else(invalid)?,
            protocol: parse_protocol(protocol).ok_or_else(invalid)?,
            ports: parse_ports(ports).map_err(|_| invalid())?,
            remote: parse_remote(remote).ok_or_else(invalid)?,
        }))
    }
}

#[derive(Subcommand)]
pub enum SecurityGroupCommand {
    #[command(about = "Create a security group")]
    Create {
        name: ObjectName,
        #[arg(long = "rule", value_name = "RULE", help = RULE_FORMAT)]
        rules: Vec<RuleArgument>,
    },
    #[command(flatten)]
    Common(CommonCommand),
}

fn describe(rule: &SecurityGroupRule) -> String {
    let ports = rule.ports.map(|ports| format!(" {}-{}", ports.first, ports.last)).unwrap_or_default();
    let remote = match &rule.remote {
        Remote::Any => String::from("any"),
        Remote::Addresses { network } => network.to_string(),
        Remote::SecurityGroup { name } => format!("{GROUP_PREFIX}{name}"),
    };
    format!("{} {}{ports} {remote}", kebab_name(rule.direction), kebab_name(rule.protocol))
}

impl Tabulated for SecurityGroup {
    const HEADINGS: &'static [&'static str] = &["NAME", "RULES"];

    fn row(group: &Object<Self>) -> Vec<String> {
        let rules: Vec<String> = group.specification.rules.iter().map(describe).collect();
        vec![group.metadata.name.to_string(), rules.join("; ")]
    }
}

pub async fn run(
    client: &ApiClient,
    command: SecurityGroupCommand,
    format: OutputFormat,
) -> anyhow::Result<()> {
    match command {
        SecurityGroupCommand::Create { name, rules } => {
            let specification = SecurityGroupSpecification {
                rules: rules.into_iter().map(|RuleArgument(rule)| rule).collect(),
            };
            create::<SecurityGroup>(client, &name, &specification, format).await
        }
        SecurityGroupCommand::Common(command) => run_common::<SecurityGroup>(client, command, format).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_read_from_four_comma_separated_fields() {
        let RuleArgument(rule) = "ingress,tcp,8000-8100,group:web".parse().unwrap();
        assert_eq!(rule.direction, Direction::Ingress);
        assert_eq!(rule.ports, Some(PortRange { first: 8000, last: 8100 }));
        assert_eq!(rule.remote, Remote::SecurityGroup { name: ObjectName::new("web").unwrap() });
        let RuleArgument(rule) = "egress,any,,10.0.0.0/8".parse().unwrap();
        assert_eq!(rule.ports, None);
        assert_eq!(rule.remote, Remote::Addresses { network: "10.0.0.0/8".parse().unwrap() });
        assert!("ingress,tcp,80".parse::<RuleArgument>().is_err());
        assert!("sideways,tcp,80,any".parse::<RuleArgument>().is_err());
    }
}
