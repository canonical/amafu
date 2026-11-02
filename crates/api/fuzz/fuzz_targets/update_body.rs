#![no_main]

use std::collections::BTreeMap;

use amafu_api::admission::admit_update;
use amafu_domain::registry::{UntypedChange, kinds};
use amafu_domain::{ObjectId, ObjectName, ProjectName};
use libfuzzer_sys::fuzz_target;
use serde_json::json;

fn existing(kind: &amafu_domain::registry::KindDescriptor) -> Option<serde_json::Value> {
    let specification = match kind.kind {
        "Vm" => json!({"cpu_count": 1, "memory_mebibytes": 256, "boot": {"type": "kernel", "bundle": "test-guest"}}),
        _ => json!({"roles": ["compute"]}),
    };
    let project = if kind.is_project_scoped() { ProjectName::new("project-a").ok() } else { None };
    let change = UntypedChange::Create {
        id: ObjectId::from_uuid(uuid::Uuid::nil()),
        name: ObjectName::new("existing").ok()?,
        project,
        labels: BTreeMap::new(),
        specification,
    };
    (kind.apply_change)(None, change).ok()
}

fuzz_target!(|body: &[u8]| {
    let Ok(change) = admit_update(body) else {
        return;
    };
    for kind in kinds() {
        let Some(current) = existing(&kind) else {
            continue;
        };
        if let Ok(updated) = (kind.apply_change)(Some(&current), change.clone()) {
            let before = current["metadata"]["generation"].as_u64().unwrap_or_default();
            let after = updated["metadata"]["generation"].as_u64().unwrap_or_default();
            assert!(after == before || after == before + 1, "generation moves by at most one");
            assert_eq!(updated["metadata"]["id"], current["metadata"]["id"]);
        }
    }
});
