use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use amafu_domain::ObjectId;
use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
};
use rustix::fs::{Gid, Mode, Uid, chmod, chown};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule,
    TargetArch,
};

use serde::{Deserialize, Serialize};

use crate::runtime::RuntimeDirectory;

pub const VM_GROUP_ID: u32 = 64_000;
pub const FIRST_VM_USER_ID: u32 = 200_000;
pub const MAXIMUM_VMS: u32 = 60_000;
const KVM_DEVICE: &str = "/dev/kvm";
const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const CGROUP_SLICE: &str = "amafu-vms";
const SERVICE_SLICE: &str = "amafu-services";
const CONTROLLERS: &str = "+cpu +memory +pids";
const CPU_PERIOD_MICROSECONDS: u64 = 100_000;
const MEMORY_OVERHEAD_MEBIBYTES: u64 = 256;
const MAXIMUM_PROCESSES: u32 = 4096;
const ADDRESS_FAMILY_UNIX: u64 = 1;
const ADDRESS_FAMILY_INTERNET: u64 = 2;
const NO_SUCH_SYSTEM_CALL: u32 = 38;
const LANDLOCK_ABI: ABI = ABI::V5;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("the filesystem sandbox cannot be applied: {0}")]
    Filesystem(String),
    #[error("the system call filter cannot be applied: {0}")]
    SystemCalls(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketFamilies {
    UnixOnly,
    UnixAndInternet,
}

impl SocketFamilies {
    fn allowed(self) -> &'static [u64] {
        match self {
            Self::UnixOnly => &[ADDRESS_FAMILY_UNIX],
            Self::UnixAndInternet => &[ADDRESS_FAMILY_UNIX, ADDRESS_FAMILY_INTERNET],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restrictions {
    pub readable: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
}

pub fn prepare_kvm_access() -> io::Result<()> {
    chown(KVM_DEVICE, Some(Uid::ROOT), Some(Gid::from_raw(VM_GROUP_ID)))?;
    chmod(KVM_DEVICE, Mode::from_raw_mode(0o660)).map_err(Into::into)
}

pub fn allocate_user_id(
    runtime: &RuntimeDirectory,
    vm_id: &ObjectId,
    previous: Option<u32>,
) -> io::Result<u32> {
    let mut taken: BTreeMap<u32, ObjectId> = BTreeMap::new();
    for (existing, directory) in runtime.vms()? {
        if let Some(user_id) = directory.user_id() {
            if existing == *vm_id {
                return Ok(user_id);
            }
            taken.insert(user_id, existing);
        }
    }
    previous
        .filter(|user_id| is_vm_identity(*user_id))
        .into_iter()
        .chain(std::iter::once(preferred_user_id(vm_id)))
        .chain(FIRST_VM_USER_ID..FIRST_VM_USER_ID + MAXIMUM_VMS)
        .find(|candidate| !taken.contains_key(candidate))
        .ok_or_else(|| io::Error::other("every VM identity is in use"))
}

#[must_use]
pub fn preferred_user_id(vm_id: &ObjectId) -> u32 {
    let spread = vm_id
        .to_string()
        .bytes()
        .fold(0_u32, |hash, byte| hash.wrapping_mul(31).wrapping_add(u32::from(byte)));
    FIRST_VM_USER_ID + spread % MAXIMUM_VMS
}

#[must_use]
pub fn is_vm_identity(user_id: u32) -> bool {
    (FIRST_VM_USER_ID..FIRST_VM_USER_ID + MAXIMUM_VMS).contains(&user_id)
}

#[must_use]
pub fn owner_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|metadata| metadata.uid())
}

pub fn give_to(path: &Path, user_id: u32) -> io::Result<()> {
    chown(path, Some(Uid::from_raw(user_id)), Some(Gid::from_raw(VM_GROUP_ID))).map_err(Into::into)
}

fn cgroup_slice() -> PathBuf {
    Path::new(CGROUP_ROOT).join(CGROUP_SLICE)
}

#[must_use]
pub fn cgroup_of(name: &str) -> PathBuf {
    cgroup_slice().join(name)
}

fn create_slice(slice: &Path) -> io::Result<()> {
    std::fs::write(Path::new(CGROUP_ROOT).join("cgroup.subtree_control"), CONTROLLERS)?;
    std::fs::create_dir_all(slice)?;
    std::fs::write(slice.join("cgroup.subtree_control"), CONTROLLERS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceLimits {
    pub cpu_count: u16,
    pub memory_mebibytes: u64,
    pub processes: u32,
}

pub fn create_service_cgroup(name: &str, limits: ServiceLimits) -> io::Result<PathBuf> {
    let slice = Path::new(CGROUP_ROOT).join(SERVICE_SLICE);
    create_slice(&slice)?;
    let cgroup = slice.join(name);
    std::fs::create_dir_all(&cgroup)?;
    let quota = u64::from(limits.cpu_count) * CPU_PERIOD_MICROSECONDS;
    std::fs::write(cgroup.join("cpu.max"), format!("{quota} {CPU_PERIOD_MICROSECONDS}"))?;
    std::fs::write(cgroup.join("memory.max"), (limits.memory_mebibytes * 1024 * 1024).to_string())?;
    std::fs::write(cgroup.join("pids.max"), limits.processes.to_string())?;
    Ok(cgroup)
}

pub fn create_cgroup(name: &str, cpu_count: u16, memory_mebibytes: u64) -> io::Result<PathBuf> {
    create_slice(&cgroup_slice())?;
    let cgroup = cgroup_of(name);
    std::fs::create_dir_all(&cgroup)?;
    let quota = u64::from(cpu_count) * CPU_PERIOD_MICROSECONDS;
    std::fs::write(cgroup.join("cpu.max"), format!("{quota} {CPU_PERIOD_MICROSECONDS}"))?;
    std::fs::write(
        cgroup.join("memory.max"),
        ((memory_mebibytes + MEMORY_OVERHEAD_MEBIBYTES) * 1024 * 1024).to_string(),
    )?;
    std::fs::write(cgroup.join("pids.max"), MAXIMUM_PROCESSES.to_string())?;
    match std::fs::write(cgroup.join("memory.swap.max"), "0") {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    Ok(cgroup)
}

pub fn place_in_cgroup(cgroup: &Path, process_id: u32) -> io::Result<()> {
    std::fs::write(cgroup.join("cgroup.procs"), process_id.to_string())
}

const SMALLEST_QUOTA_MICROSECONDS: u64 = 1_000;

pub fn limit_cpu_share(name: &str, cpu_count: u16, share_percent: u64) -> io::Result<()> {
    let quota = (u64::from(cpu_count) * CPU_PERIOD_MICROSECONDS * share_percent / 100)
        .max(SMALLEST_QUOTA_MICROSECONDS);
    std::fs::write(cgroup_of(name).join("cpu.max"), format!("{quota} {CPU_PERIOD_MICROSECONDS}"))
}

pub fn remove_cgroup(name: &str) -> io::Result<()> {
    match std::fs::remove_dir(cgroup_of(name)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn restrict_filesystem(restrictions: &Restrictions) -> Result<(), SandboxError> {
    let failure = |error: landlock::RulesetError| SandboxError::Filesystem(error.to_string());
    let existing =
        |paths: &[PathBuf]| -> Vec<PathBuf> { paths.iter().filter(|path| path.exists()).cloned().collect() };
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(LANDLOCK_ABI))
        .map_err(failure)?
        .create()
        .map_err(failure)?
        .add_rules(path_beneath_rules(existing(&restrictions.readable), AccessFs::from_read(LANDLOCK_ABI)))
        .map_err(failure)?
        .add_rules(path_beneath_rules(existing(&restrictions.writable), AccessFs::from_all(LANDLOCK_ABI)))
        .map_err(failure)?
        .restrict_self()
        .map_err(failure)?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(SandboxError::Filesystem(String::from("this kernel does not enforce Landlock")));
    }
    Ok(())
}

const FORBIDDEN_SYSTEM_CALLS: &[i64] = &[
    numbers::PTRACE,
    numbers::PROCESS_VM_READV,
    numbers::PROCESS_VM_WRITEV,
    numbers::MOUNT,
    numbers::UMOUNT2,
    numbers::PIVOT_ROOT,
    numbers::CHROOT,
    numbers::INIT_MODULE,
    numbers::FINIT_MODULE,
    numbers::DELETE_MODULE,
    numbers::KEXEC_LOAD,
    numbers::KEXEC_FILE_LOAD,
    numbers::BPF,
    numbers::UNSHARE,
    numbers::SETNS,
    numbers::KEYCTL,
    numbers::ADD_KEY,
    numbers::REQUEST_KEY,
    numbers::PERF_EVENT_OPEN,
    numbers::OPEN_BY_HANDLE_AT,
    numbers::REBOOT,
    numbers::SWAPON,
    numbers::SWAPOFF,
];

mod numbers {
    pub const SOCKET: i64 = 41;
    pub const PTRACE: i64 = 101;
    pub const PROCESS_VM_READV: i64 = 310;
    pub const PROCESS_VM_WRITEV: i64 = 311;
    pub const MOUNT: i64 = 165;
    pub const UMOUNT2: i64 = 166;
    pub const PIVOT_ROOT: i64 = 155;
    pub const CHROOT: i64 = 161;
    pub const INIT_MODULE: i64 = 175;
    pub const FINIT_MODULE: i64 = 313;
    pub const DELETE_MODULE: i64 = 176;
    pub const KEXEC_LOAD: i64 = 246;
    pub const KEXEC_FILE_LOAD: i64 = 320;
    pub const BPF: i64 = 321;
    pub const UNSHARE: i64 = 272;
    pub const SETNS: i64 = 308;
    pub const KEYCTL: i64 = 250;
    pub const ADD_KEY: i64 = 248;
    pub const REQUEST_KEY: i64 = 249;
    pub const PERF_EVENT_OPEN: i64 = 298;
    pub const OPEN_BY_HANDLE_AT: i64 = 304;
    pub const REBOOT: i64 = 169;
    pub const SWAPON: i64 = 167;
    pub const SWAPOFF: i64 = 168;
    pub const IO_URING_SETUP: i64 = 425;
    pub const IO_URING_ENTER: i64 = 426;
    pub const IO_URING_REGISTER: i64 = 427;
}

const UNAVAILABLE_SYSTEM_CALLS: &[i64] =
    &[numbers::IO_URING_SETUP, numbers::IO_URING_ENTER, numbers::IO_URING_REGISTER];

fn backend_failure(error: impl std::fmt::Display) -> SandboxError {
    SandboxError::SystemCalls(error.to_string())
}

fn compile(
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    action: SeccompAction,
) -> Result<BpfProgram, SandboxError> {
    let filter = SeccompFilter::new(rules, SeccompAction::Allow, action, TargetArch::x86_64)
        .map_err(backend_failure)?;
    BpfProgram::try_from(filter).map_err(backend_failure)
}

fn unconditional(numbers: &[i64]) -> BTreeMap<i64, Vec<SeccompRule>> {
    numbers.iter().map(|number| (*number, Vec::new())).collect()
}

pub fn forbidden_system_calls_filter(sockets: SocketFamilies) -> Result<BpfProgram, SandboxError> {
    let mut rules = unconditional(FORBIDDEN_SYSTEM_CALLS);
    let other_family = sockets
        .allowed()
        .iter()
        .map(|family| SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, *family))
        .collect::<Result<Vec<_>, _>>()
        .map_err(backend_failure)?;
    rules.insert(numbers::SOCKET, vec![SeccompRule::new(other_family).map_err(backend_failure)?]);
    compile(rules, SeccompAction::KillProcess)
}

pub fn unavailable_system_calls_filter() -> Result<BpfProgram, SandboxError> {
    compile(unconditional(UNAVAILABLE_SYSTEM_CALLS), SeccompAction::Errno(NO_SUCH_SYSTEM_CALL))
}

pub fn enter(restrictions: &Restrictions) -> Result<(), SandboxError> {
    enter_with(restrictions, SocketFamilies::UnixOnly)
}

pub fn enter_with(restrictions: &Restrictions, sockets: SocketFamilies) -> Result<(), SandboxError> {
    restrict_filesystem(restrictions)?;
    for program in [unavailable_system_calls_filter()?, forbidden_system_calls_filter(sockets)?] {
        seccompiler::apply_filter(&program).map_err(|error| SandboxError::SystemCalls(error.to_string()))?;
    }
    Ok(())
}

#[must_use]
pub fn distinct_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().collect::<BTreeSet<PathBuf>>().into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_call_filters_compile() {
        assert!(forbidden_system_calls_filter(SocketFamilies::UnixOnly).is_ok());
        assert!(forbidden_system_calls_filter(SocketFamilies::UnixAndInternet).is_ok());
        assert!(unavailable_system_calls_filter().is_ok());
    }

    #[test]
    fn user_identities_are_allocated_lowest_first_and_reused_for_the_same_vm() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let first = ObjectId::from_uuid(uuid::Uuid::from_u128(1));
        let preferred = preferred_user_id(&first);
        assert_eq!(allocate_user_id(&runtime, &first, None).unwrap(), preferred);
        let directory = runtime.vm(&first);
        std::fs::create_dir_all(directory.path()).unwrap();
        directory.record_user_id(preferred).unwrap();
        assert_eq!(allocate_user_id(&runtime, &first, None).unwrap(), preferred);
        let second = ObjectId::from_uuid(uuid::Uuid::from_u128(2));
        assert_eq!(allocate_user_id(&runtime, &second, None).unwrap(), preferred_user_id(&second));
    }

    #[test]
    fn every_node_prefers_the_same_identity_for_a_vm_and_vms_spread_out() {
        let identities: std::collections::BTreeSet<u32> = (0..200_u128)
            .map(|id| preferred_user_id(&ObjectId::from_uuid(uuid::Uuid::from_u128(id))))
            .collect();
        assert!(identities.iter().all(|user_id| is_vm_identity(*user_id)));
        assert!(identities.len() > 190);
    }

    #[test]
    fn a_vm_keeps_the_identity_that_owns_its_disks_on_another_node() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let arriving = ObjectId::from_uuid(uuid::Uuid::from_u128(7));
        assert_eq!(
            allocate_user_id(&runtime, &arriving, Some(FIRST_VM_USER_ID + 41)).unwrap(),
            FIRST_VM_USER_ID + 41
        );
    }

    #[test]
    fn an_identity_in_use_here_is_not_shared_and_outsiders_are_ignored() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let resident = ObjectId::from_uuid(uuid::Uuid::from_u128(1));
        let directory = runtime.vm(&resident);
        std::fs::create_dir_all(directory.path()).unwrap();
        directory.record_user_id(FIRST_VM_USER_ID).unwrap();
        let arriving = ObjectId::from_uuid(uuid::Uuid::from_u128(2));
        let preferred = preferred_user_id(&arriving);
        assert_eq!(allocate_user_id(&runtime, &arriving, Some(FIRST_VM_USER_ID)).unwrap(), preferred);
        assert_eq!(allocate_user_id(&runtime, &arriving, Some(0)).unwrap(), preferred);
    }
}
