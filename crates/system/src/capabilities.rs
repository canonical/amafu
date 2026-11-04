use rustix::io::Errno;
pub use rustix::thread::CapabilitySet;
use rustix::thread::{CapabilitySets, remove_capability_from_bounding_set, set_capabilities};

pub fn keep_only(kept: CapabilitySet) -> std::io::Result<()> {
    for dropped in (CapabilitySet::all() - kept).iter() {
        match remove_capability_from_bounding_set(dropped) {
            Ok(()) | Err(Errno::INVAL) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let sets = CapabilitySets { effective: kept, permitted: kept, inheritable: CapabilitySet::empty() };
    set_capabilities(None, sets)?;
    Ok(())
}
