use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use amafu_hypervisor::{
    Attempt, BootSources, CloudHypervisor, CloudHypervisorSettings, ProbeOutcome, RuntimeDirectory,
};

const SIGNAL_KILL: i32 = 9;
const SIGNAL_BAD_SYSTEM_CALL: i32 = 31;

struct Fixture {
    _root: tempfile::TempDir,
    hypervisor: CloudHypervisor,
    shared_file: PathBuf,
    private_file: PathBuf,
}

fn open_to_everyone(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

fn fixture() -> std::io::Result<Fixture> {
    let root = tempfile::tempdir()?;
    open_to_everyone(root.path(), 0o755)?;
    let shared = root.path().join("image-cache");
    let runtime = root.path().join("vms");
    for directory in [&shared, &runtime] {
        std::fs::create_dir_all(directory)?;
        open_to_everyone(directory, 0o755)?;
    }
    let shared_file = shared.join("image");
    std::fs::write(&shared_file, b"image")?;
    open_to_everyone(&shared_file, 0o644)?;
    let private_file = root.path().join("node-state");
    std::fs::write(&private_file, b"secret")?;
    open_to_everyone(&private_file, 0o644)?;
    let executable = root.path().join("amafu-node");
    std::fs::copy(env!("CARGO_BIN_EXE_amafu-node"), &executable)?;
    open_to_everyone(&executable, 0o755)?;
    let hypervisor = CloudHypervisor::new(CloudHypervisorSettings {
        runtime: RuntimeDirectory::new(runtime),
        boot: BootSources {
            bundles_directory: root.path().join("bundles"),
            firmware: root.path().join("CLOUDHV.fd"),
            container_kernel: root.path().join("vmlinuz"),
            container_initramfs: root.path().join("initramfs"),
        },
        executable,
        shared_readable: vec![shared],
    });
    Ok(Fixture { _root: root, hypervisor, shared_file, private_file })
}

#[tokio::test]
#[ignore = "needs root, cgroup v2 and Landlock"]
async fn a_vm_process_is_confined_to_what_its_vm_needs() {
    let fixture = fixture().unwrap();
    let probe = |attempt: Attempt| fixture.hypervisor.probe(attempt);
    assert_eq!(
        probe(Attempt::ReadFile { path: fixture.shared_file.clone() }).await.unwrap(),
        ProbeOutcome::Permitted
    );
    for forbidden in
        [fixture.private_file.clone(), PathBuf::from("/etc/shadow"), PathBuf::from("/etc/hostname")]
    {
        let outcome = probe(Attempt::ReadFile { path: forbidden.clone() }).await.unwrap();
        assert!(
            matches!(outcome, ProbeOutcome::Denied { .. }),
            "reading {} gave {outcome:?}",
            forbidden.display()
        );
    }
    let outcome = probe(Attempt::WriteFile { path: PathBuf::from("/tmp/escaped") }).await.unwrap();
    assert!(matches!(outcome, ProbeOutcome::Denied { .. }), "writing outside gave {outcome:?}");
    for attempt in
        [Attempt::OpenInternetSocket, Attempt::OpenNetlinkSocket, Attempt::Mount, Attempt::LoadModule]
    {
        assert_eq!(
            probe(attempt.clone()).await.unwrap(),
            ProbeOutcome::Killed { signal: SIGNAL_BAD_SYSTEM_CALL },
            "{attempt:?}"
        );
    }
    assert_eq!(
        probe(Attempt::AllocateMemory { mebibytes: 1024 }).await.unwrap(),
        ProbeOutcome::Killed { signal: SIGNAL_KILL }
    );
}
