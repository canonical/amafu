#![no_main]

use amafu_api::admission::admit_create;
use amafu_domain::registry::kinds;
use amafu_domain::{ObjectId, ProjectName};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|body: &[u8]| {
    let project = ProjectName::new("project-a").ok();
    for kind in kinds() {
        let Ok(change) = admit_create(&kind, project.clone(), body, ObjectId::from_uuid(uuid::Uuid::nil())) else {
            continue;
        };
        if let Ok(created) = (kind.apply_change)(None, change) {
            let encoded = serde_json::to_vec(&created).unwrap_or_default();
            let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap_or_default();
            assert_eq!(decoded, created, "a created {} must survive a round trip", kind.kind);
            assert_eq!(created["metadata"]["generation"], 1);
        }
    }
});
