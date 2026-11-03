#![no_main]

use std::collections::BTreeSet;

use amafu_api::node_wire::NodeSwapRequest;
use amafu_domain::kinds::node::NodeRole;
use amafu_domain::registry::kind_named;
use amafu_domain::slice::NodeSlice;
use amafu_domain::NodeName;
use libfuzzer_sys::fuzz_target;
use serde_json::json;

fuzz_target!(|body: &[u8]| {
    let Ok(request) = serde_json::from_slice::<NodeSwapRequest>(body) else {
        return;
    };
    let Ok(node) = NodeName::new("node-3") else {
        return;
    };
    let mut slice = NodeSlice::new(node);
    let hosted = json!({"metadata": {"name": "web-1", "project": "project-a"}, "specification": {"volumes": ["data"]}, "status": {"node": "node-3"}});
    slice.learn_vms([&hosted]);
    let roles = BTreeSet::from([NodeRole::Compute]);
    let kind = request.reference.kind.as_str();
    if let Some(replacement) = &request.replacement
        && let Some(descriptor) = kind_named(kind)
    {
        let _ = (descriptor.check_object)(replacement);
    }
    let allowed = slice.may_write(&roles, kind, Some(&hosted), request.replacement.as_ref());
    if allowed {
        assert!(matches!(kind, "Vm" | "Node" | "Volume" | "Image" | "NetworkEndpoint"), "{kind} was writable");
    }
    let _ = slice.includes(kind, request.replacement.as_ref().unwrap_or(&hosted));
});
