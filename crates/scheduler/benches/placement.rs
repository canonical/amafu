use std::hint::black_box;

use amafu_domain::{NodeName, ObjectId, ZoneName};
use amafu_scheduler::{CandidateNode, ClusterCapacity, PendingWorkload, Scheduler, place};
use criterion::{Criterion, criterion_group, criterion_main};

const NODE_COUNT: usize = 1_000;
const WORKLOAD_COUNT: u128 = 10_000;
const ZONE_COUNT: usize = 3;

fn cluster() -> ClusterCapacity {
    let nodes = (0..NODE_COUNT)
        .map(|index| {
            let name = NodeName::new(format!("node-{index:04}")).unwrap_or_else(|_| unreachable!());
            let mut node = CandidateNode::new(name, 256, 1_048_576);
            node.zone = ZoneName::new(format!("zone-{}", index % ZONE_COUNT)).ok();
            node
        })
        .collect();
    ClusterCapacity::new(nodes)
}

fn pending() -> Vec<PendingWorkload> {
    (0..WORKLOAD_COUNT)
        .map(|index| {
            let mut workload =
                PendingWorkload::new(ObjectId::from_uuid(uuid::Uuid::from_u128(index)), 2, 4096);
            workload.priority = i32::try_from(index % 7).unwrap_or_default();
            workload.spread_group = format!("project-{}", index % 20);
            if index % 10 == 0 {
                workload.anti_affinity_group = Some(format!("group-{}", index % 50));
            }
            workload
        })
        .collect()
}

fn ten_thousand_workloads_on_a_thousand_nodes(criterion: &mut Criterion) {
    let scheduler = Scheduler::standard();
    let (cluster, pending) = (cluster(), pending());
    let mut group = criterion.benchmark_group("placement");
    group.sample_size(10);
    group.bench_function("10000 workloads on 1000 nodes", |bencher| {
        bencher.iter(|| place(&scheduler, black_box(cluster.clone()), black_box(pending.clone())));
    });
    group.finish();
}

criterion_group!(benches, ten_thousand_workloads_on_a_thousand_nodes);
criterion_main!(benches);
