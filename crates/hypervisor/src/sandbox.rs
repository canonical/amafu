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
const CONTROLLERS: &str = "+cpu +memory +pids";
const CPU_PERIOD_MICROSECONDS: u64 = 100_000;
const MEMORY_OVERHEAD_MEBIBYTES: u64 = 256;
const MAXIMUM_PROCESSES: u32 = 4096;
const ADDRESS_FAMILY_UNIX: u64 = 1;
const NO_SUCH_SYSTEM_CALL: u32 = 38;
const LANDLOCK_ABI: ABI = ABI::V5;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("the filesystem sandbox cannot be applied: {0}")]
    Filesystem(String),
    #[error("the system call filter cannot be applied: {0}")]
    SystemCalls(String),
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

pub fn allocate_user_id(runtime: &RuntimeDirectory, vm_id: &ObjectId) -> io::Result<u32> {
    let mut taken: BTreeMap<u32, ObjectId> = BTreeMap::new();
    for (existing, directory) in runtime.vms()? {
        if let Some(user_id) = directory.user_id() {
            if existing == *vm_id {
                return Ok(user_id);
            }
            taken.insert(user_id, existing);
        }
    }
    (FIRST_VM_USER_ID..FIRST_VM_USER_ID + MAXIMUM_VMS)
        .find(|candidate| !taken.contains_key(candidate))
        .ok_or_else(|| io::Error::other("every VM identity is in use"))
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

pub fn create_cgroup(name: &str, cpu_count: u16, memory_mebibytes: u64) -> io::Result<PathBuf> {
    std::fs::write(Path::new(CGROUP_ROOT).join("cgroup.subtree_control"), CONTROLLERS)?;
    std::fs::create_dir_all(cgroup_slice())?;
    std::fs::write(cgroup_slice().join("cgroup.subtree_control"), CONTROLLERS)?;
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

pub fn forbidden_system_calls_filter() -> Result<BpfProgram, SandboxError> {
    let mut rules = unconditional(FORBIDDEN_SYSTEM_CALLS);
    let other_family =
        SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, ADDRESS_FAMILY_UNIX)
            .map_err(backend_failure)?;
    rules.insert(numbers::SOCKET, vec![SeccompRule::new(vec![other_family]).map_err(backend_failure)?]);
    compile(rules, SeccompAction::KillProcess)
}

pub fn unavailable_system_calls_filter() -> Result<BpfProgram, SandboxError> {
    compile(unconditional(UNAVAILABLE_SYSTEM_CALLS), SeccompAction::Errno(NO_SUCH_SYSTEM_CALL))
}

pub fn enter(restrictions: &Restrictions) -> Result<(), SandboxError> {
    restrict_filesystem(restrictions)?;
    for program in [unavailable_system_calls_filter()?, forbidden_system_calls_filter()?] {
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
        assert!(forbidden_system_calls_filter().is_ok());
        assert!(unavailable_system_calls_filter().is_ok());
    }

    #[test]
    fn user_identities_are_allocated_lowest_first_and_reused_for_the_same_vm() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimeDirectory::new(root.path());
        let first = ObjectId::from_uuid(uuid::Uuid::from_u128(1));
        assert_eq!(allocate_user_id(&runtime, &first).unwrap(), FIRST_VM_USER_ID);
        let directory = runtime.vm(&first);
        std::fs::create_dir_all(directory.path()).unwrap();
        directory.record_user_id(FIRST_VM_USER_ID).unwrap();
        assert_eq!(allocate_user_id(&runtime, &first).unwrap(), FIRST_VM_USER_ID);
        let second = ObjectId::from_uuid(uuid::Uuid::from_u128(2));
        assert_eq!(allocate_user_id(&runtime, &second).unwrap(), FIRST_VM_USER_ID + 1);
    }
}
