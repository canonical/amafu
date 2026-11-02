use super::{ContractViolation, ensure, succeed};
use crate::network::{Datapath, NodeNetworkState};

pub async fn datapath_contract(
    datapath: &dyn Datapath,
    desired: &NodeNetworkState,
) -> Result<(), ContractViolation> {
    let realized = succeed(datapath.converge(desired).await, "converging works")?;
    ensure(
        desired.segments.iter().all(|segment| realized.segments.contains(&segment.key)),
        "every desired segment is realized",
        &realized,
    )?;
    ensure(
        desired.ports.iter().all(|port| realized.ports.contains(&port.key)),
        "every desired port is realized",
        &realized,
    )?;
    let again = succeed(datapath.converge(desired).await, "converging is idempotent")?;
    ensure(again == realized, "converging twice gives the same result", (&realized, &again))?;
    let current = succeed(datapath.current().await, "reading the datapath works")?;
    ensure(current == realized, "the datapath reports what it realized", (&realized, &current))?;

    let emptied =
        succeed(datapath.converge(&NodeNetworkState::default()).await, "converging to nothing works")?;
    ensure(
        emptied.ports.is_empty() && emptied.segments.is_empty(),
        "converging to nothing removes everything",
        &emptied,
    )
}
