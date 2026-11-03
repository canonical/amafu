use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use amafu_domain::NodeName;
use amafu_interfaces::{Fence, HeldLease, Leases, StoreError};
use futures::FutureExt;
use futures::channel::oneshot;

use crate::lock::lock;

type Grant = (u64, oneshot::Receiver<()>);

struct Holding {
    holder: NodeName,
    token: u64,
    _lost: oneshot::Sender<()>,
}

struct Waiter {
    holder: NodeName,
    grant: oneshot::Sender<Grant>,
}

#[derive(Default)]
struct LeaseTable {
    next_token: u64,
    held: BTreeMap<String, Holding>,
    waiting: BTreeMap<String, VecDeque<Waiter>>,
    isolated: BTreeSet<NodeName>,
    grants: Vec<(String, NodeName, u64)>,
}

impl LeaseTable {
    fn grant_next(&mut self, name: &str) {
        if self.held.contains_key(name) {
            return;
        }
        let Some(queue) = self.waiting.get_mut(name) else {
            return;
        };
        let mut skipped = VecDeque::new();
        while let Some(waiter) = queue.pop_front() {
            if self.isolated.contains(&waiter.holder) {
                skipped.push_back(waiter);
                continue;
            }
            self.next_token += 1;
            let token = self.next_token;
            let (lost_sender, lost_receiver) = oneshot::channel();
            if waiter.grant.send((token, lost_receiver)).is_ok() {
                self.grants.push((name.to_owned(), waiter.holder.clone(), token));
                self.held
                    .insert(name.to_owned(), Holding { holder: waiter.holder, token, _lost: lost_sender });
                break;
            }
        }
        skipped.extend(queue.drain(..));
        *queue = skipped;
    }

    fn release(&mut self, name: &str, token: u64) {
        if self.held.get(name).is_some_and(|holding| holding.token == token) {
            self.held.remove(name);
            self.grant_next(name);
        }
    }
}

#[derive(Default)]
pub struct FakeLeases {
    table: Mutex<LeaseTable>,
}

impl FakeLeases {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn for_node(self: &Arc<Self>, holder: NodeName) -> NodeLeases {
        NodeLeases { table: Arc::clone(self), holder }
    }

    pub fn isolate(&self, node: &NodeName) {
        let mut table = lock(&self.table);
        table.isolated.insert(node.clone());
        let lost: Vec<String> = table
            .held
            .iter()
            .filter(|(_, holding)| holding.holder == *node)
            .map(|(name, _)| name.clone())
            .collect();
        for name in lost {
            table.held.remove(&name);
            table.grant_next(&name);
        }
    }

    pub fn reconnect(&self, node: &NodeName) {
        let mut table = lock(&self.table);
        table.isolated.remove(node);
        let names: Vec<String> = table.waiting.keys().cloned().collect();
        for name in names {
            table.grant_next(&name);
        }
    }

    #[must_use]
    pub fn grants(&self) -> Vec<(String, NodeName, u64)> {
        lock(&self.table).grants.clone()
    }
}

struct Release {
    table: Arc<FakeLeases>,
    name: String,
    token: u64,
}

impl Drop for Release {
    fn drop(&mut self) {
        lock(&self.table.table).release(&self.name, self.token);
    }
}

pub struct NodeLeases {
    table: Arc<FakeLeases>,
    holder: NodeName,
}

#[async_trait::async_trait]
impl Leases for NodeLeases {
    async fn acquire(&self, name: &str) -> Result<HeldLease, StoreError> {
        let (grant, granted) = oneshot::channel();
        {
            let mut table = lock(&self.table.table);
            table
                .waiting
                .entry(name.to_owned())
                .or_default()
                .push_back(Waiter { holder: self.holder.clone(), grant });
            table.grant_next(name);
        }
        let (token, lost) =
            granted.await.map_err(|_| StoreError::Unavailable(String::from("the lease table stopped")))?;
        let release = Release { table: Arc::clone(&self.table), name: name.to_owned(), token };
        let lost = async move {
            let _release = release;
            let _ = lost.await;
        }
        .boxed();
        Ok(HeldLease::new(Fence { name: name.to_owned(), token }, lost))
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;

    use super::*;

    fn node(name: &str) -> NodeName {
        NodeName::new(name).unwrap()
    }

    #[test]
    fn one_holder_at_a_time_with_increasing_tokens() {
        let leases = FakeLeases::new();
        let first = block_on(leases.for_node(node("node-0")).acquire("scheduler")).unwrap();
        let other = leases.for_node(node("node-1"));
        let mut waiting = Box::pin(other.acquire("scheduler"));
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(first);
        let second = block_on(waiting).unwrap();
        assert!(second.fence.token > 1);
        assert_eq!(leases.grants().len(), 2);
    }

    #[test]
    fn an_isolated_holder_loses_its_lease_to_a_waiter() {
        let leases = FakeLeases::new();
        let first = block_on(leases.for_node(node("node-0")).acquire("scheduler")).unwrap();
        let other = leases.for_node(node("node-1"));
        let waiting = other.acquire("scheduler");
        leases.isolate(&node("node-0"));
        let (_, lost) = first.into_parts();
        block_on(lost);
        let second = block_on(waiting).unwrap();
        assert_eq!(second.fence.token, 2);
    }
}
