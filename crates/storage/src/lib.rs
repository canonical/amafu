pub mod built_in;
pub mod ceph;
pub mod images;
pub mod local_disk;
pub mod nfs;
pub mod nvme_tcp;
pub mod qcow2;

pub use built_in::BuiltInStorage;
pub use images::CachedImages;
