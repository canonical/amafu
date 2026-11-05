use std::path::Path;

#[test]
fn the_kind_reference_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/kinds.md");
    let committed = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(committed, amafu_domain::registry::kind_reference(), "run cargo rig reference");
}
