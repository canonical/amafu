use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use amafu_domain::NodeName;
use serde::{Deserialize, Serialize};

pub const SILENCE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advertisement {
    pub node: NodeName,
    pub is_eligible: bool,
    pub holds: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    Hold,
    Release,
}

#[derive(Debug)]
struct Heard {
    advertisement: Advertisement,
    at: Instant,
}

#[derive(Debug)]
pub struct Election {
    me: NodeName,
    is_eligible: bool,
    holds: bool,
    listening_since: Instant,
    heard: BTreeMap<NodeName, Heard>,
}

impl Election {
    #[must_use]
    pub fn new(me: NodeName, now: Instant) -> Self {
        Self { me, is_eligible: false, holds: false, listening_since: now, heard: BTreeMap::new() }
    }

    pub fn set_eligible(&mut self, is_eligible: bool) {
        self.is_eligible = is_eligible;
    }

    pub fn hear(&mut self, advertisement: Advertisement, now: Instant) {
        if advertisement.node != self.me {
            self.heard.insert(advertisement.node.clone(), Heard { advertisement, at: now });
        }
    }

    #[must_use]
    pub fn holds(&self) -> bool {
        self.holds
    }

    #[must_use]
    pub fn is_held_by_a_peer(&self, now: Instant) -> bool {
        self.peers_heard_within_silence(now).any(|peer| peer.holds)
    }

    #[must_use]
    pub fn advertisement(&self) -> Advertisement {
        Advertisement { node: self.me.clone(), is_eligible: self.is_eligible, holds: self.holds }
    }

    pub fn decide(&mut self, now: Instant) -> Claim {
        self.holds = self.should_hold(now);
        if self.holds { Claim::Hold } else { Claim::Release }
    }

    fn should_hold(&self, now: Instant) -> bool {
        if !self.is_eligible {
            return false;
        }
        let peers: Vec<&Advertisement> = self.peers_heard_within_silence(now).collect();
        if self.holds {
            return !peers.iter().any(|peer| peer.holds && peer.node < self.me);
        }
        let is_vacant = !peers.iter().any(|peer| peer.holds);
        let is_first_in_line = !peers.iter().any(|peer| peer.is_eligible && peer.node < self.me);
        is_vacant && is_first_in_line && self.has_listened_long_enough(now)
    }

    fn peers_heard_within_silence(&self, now: Instant) -> impl Iterator<Item = &Advertisement> {
        self.heard
            .values()
            .filter(move |heard| now.saturating_duration_since(heard.at) < SILENCE)
            .map(|heard| &heard.advertisement)
    }

    fn has_listened_long_enough(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.listening_since) >= SILENCE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_secs(1);

    fn name(text: &str) -> NodeName {
        NodeName::new(text).unwrap()
    }

    fn eligible(text: &str, start: Instant) -> Election {
        let mut election = Election::new(name(text), start);
        election.set_eligible(true);
        election
    }

    struct Cluster {
        nodes: Vec<Election>,
        now: Instant,
        reachable: fn(usize, usize) -> bool,
    }

    impl Cluster {
        fn of(names: &[&str]) -> Self {
            let now = Instant::now();
            Self {
                nodes: names.iter().map(|text| eligible(text, now)).collect(),
                now,
                reachable: |_, _| true,
            }
        }

        fn tick(&mut self) {
            self.now += TICK;
            for index in 0..self.nodes.len() {
                self.nodes[index].decide(self.now);
            }
            let advertisements: Vec<Advertisement> = self.nodes.iter().map(Election::advertisement).collect();
            for (from, advertisement) in advertisements.iter().enumerate() {
                for to in 0..self.nodes.len() {
                    if from != to && (self.reachable)(from, to) {
                        self.nodes[to].hear(advertisement.clone(), self.now);
                    }
                }
            }
        }

        fn ticks(&mut self, count: usize) {
            for _ in 0..count {
                self.tick();
            }
        }

        fn holders(&self) -> Vec<String> {
            self.nodes.iter().filter(|node| node.holds()).map(|node| node.me.to_string()).collect()
        }
    }

    #[test]
    fn a_new_node_listens_for_a_while_before_it_claims_the_address() {
        let start = Instant::now();
        let mut alone = eligible("node-1", start);
        assert_eq!(alone.decide(start + TICK), Claim::Release);
        assert_eq!(alone.decide(start + SILENCE), Claim::Hold);
    }

    #[test]
    fn an_ineligible_node_never_holds_the_address() {
        let start = Instant::now();
        let mut node = Election::new(name("node-1"), start);
        assert_eq!(node.decide(start + SILENCE * 2), Claim::Release);
    }

    #[test]
    fn exactly_one_node_holds_the_address_and_it_is_the_first_in_line() {
        let mut cluster = Cluster::of(&["node-2", "node-1", "node-3"]);
        cluster.ticks(6);
        assert_eq!(cluster.holders(), vec![String::from("node-1")]);
    }

    #[test]
    fn a_healthy_holder_keeps_the_address_when_a_node_earlier_in_line_joins() {
        let mut cluster = Cluster::of(&["node-2", "node-3"]);
        cluster.ticks(6);
        assert_eq!(cluster.holders(), vec![String::from("node-2")]);
        cluster.nodes.push(eligible("node-1", cluster.now));
        cluster.ticks(10);
        assert_eq!(cluster.holders(), vec![String::from("node-2")]);
    }

    #[test]
    fn when_the_holder_falls_silent_the_next_in_line_takes_over_after_the_silence() {
        let mut cluster = Cluster::of(&["node-1", "node-2", "node-3"]);
        cluster.ticks(6);
        cluster.nodes.remove(0);
        cluster.ticks(2);
        assert_eq!(cluster.holders(), Vec::<String>::new());
        cluster.ticks(2);
        assert_eq!(cluster.holders(), vec![String::from("node-2")]);
    }

    #[test]
    fn a_holder_that_steps_aside_hands_the_address_on_at_the_next_tick() {
        let mut cluster = Cluster::of(&["node-1", "node-2", "node-3"]);
        cluster.ticks(6);
        cluster.nodes[0].set_eligible(false);
        cluster.ticks(2);
        assert_eq!(cluster.holders(), vec![String::from("node-2")]);
        assert!(cluster.nodes[0].is_held_by_a_peer(cluster.now));
    }

    #[test]
    fn after_a_partition_heals_only_the_first_in_line_of_the_holders_keeps_the_address() {
        let mut cluster = Cluster::of(&["node-1", "node-2", "node-3"]);
        cluster.reachable = |from, to| (from == 0) == (to == 0);
        cluster.ticks(8);
        assert_eq!(cluster.holders(), vec![String::from("node-1"), String::from("node-2")]);
        cluster.reachable = |_, _| true;
        cluster.ticks(2);
        assert_eq!(cluster.holders(), vec![String::from("node-1")]);
    }
}
