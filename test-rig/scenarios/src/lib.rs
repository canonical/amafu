pub mod containers;
pub mod networking;
pub mod overlay;
pub mod placement;
pub mod rogue_nodes;
pub mod sandbox;
pub mod security;
pub mod storage;
pub mod store_history;
pub mod support;
pub mod walking_skeleton;

use amafu_scenario::ScenarioDefinition;

#[must_use]
pub fn all() -> Vec<ScenarioDefinition> {
    vec![
        walking_skeleton::DEFINITION,
        walking_skeleton::RESTART_AFTER_POWER_CYCLE,
        walking_skeleton::MISSING_IMAGE_IS_REPORTED,
        security::AUTHORIZATION_MATRIX,
        security::CREDENTIALS_ARE_CHECKED,
        containers::CONTAINER_RUNS,
        containers::CONTAINER_EXIT_IS_REPORTED,
        storage::VM_BOOTS_FROM_A_CLONED_VOLUME,
        storage::NFS_VOLUME_IS_ATTACHED,
        storage::BLOCK_VOLUMES_FOLLOW_THEIR_VM,
        networking::ADDRESSES_ARE_DISTINCT,
        networking::SECURITY_GROUPS_DECIDE,
        sandbox::HOSTILE_GUEST_IS_CONFINED,
        placement::ANTI_AFFINITY_SPREADS_VMS,
        placement::CORDONED_NODE_IS_AVOIDED,
        placement::NODE_KEEPS_SERVING_WITHOUT_THE_CORE,
        rogue_nodes::ROGUE_NODES_ARE_REFUSED,
        rogue_nodes::NODES_WRITE_ONLY_THEIR_SHARE,
        overlay::OVERLAY_CONNECTS_NODES,
        overlay::FLOATING_IP_MOVES,
        store_history::STORE_HISTORY_IS_LINEARIZABLE,
    ]
}

#[must_use]
pub fn named(name: &str) -> Option<ScenarioDefinition> {
    all().into_iter().find(|definition| definition.name == name)
}
