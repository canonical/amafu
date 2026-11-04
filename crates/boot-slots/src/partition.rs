use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use amafu_domain::kinds::node::{BootReport, BootSlot, RefusedImage};
use amafu_domain::node_image::NodeImage;
use amafu_interfaces::{BootSlotError, BootSlots, TrustedImageKeys};

use crate::bundle::{download, extract, http_client};
use crate::environment::GrubEnvironment;
use crate::history::{BootHistory, Trial};
use crate::layout::EspLayout;

const DOWNLOAD_FILE: &str = "node-image.tar.partial";

pub trait NodeRestart: Send + Sync {
    fn restart(&self) -> io::Result<()>;
}

struct SlotState {
    environment: GrubEnvironment,
    history: BootHistory,
    first: Option<NodeImage>,
    second: Option<NodeImage>,
}

impl SlotState {
    fn image(&self, slot: BootSlot) -> Option<&NodeImage> {
        match slot {
            BootSlot::First => self.first.as_ref(),
            BootSlot::Second => self.second.as_ref(),
        }
    }

    fn set_image(&mut self, slot: BootSlot, image: Option<NodeImage>) {
        match slot {
            BootSlot::First => self.first = image,
            BootSlot::Second => self.second = image,
        }
    }
}

pub struct EspBootSlots {
    layout: EspLayout,
    data_directory: PathBuf,
    booted: BootSlot,
    keys: TrustedImageKeys,
    http: reqwest::Client,
    restart: Arc<dyn NodeRestart>,
    state: Mutex<SlotState>,
}

fn write_failure(error: impl std::fmt::Display) -> BootSlotError {
    BootSlotError::Write(error.to_string())
}

fn read_manifest(path: &Path) -> Option<NodeImage> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_environment(path: &Path, environment: GrubEnvironment) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    file.seek(io::SeekFrom::Start(0))?;
    file.write_all(&environment.render())?;
    file.sync_all()
}

impl EspBootSlots {
    pub fn open(
        layout: EspLayout,
        data_directory: PathBuf,
        booted: BootSlot,
        keys: TrustedImageKeys,
        restart: Arc<dyn NodeRestart>,
    ) -> Result<Self, BootSlotError> {
        let environment = std::fs::read_to_string(layout.environment())
            .map(|block| GrubEnvironment::parse(&block))
            .unwrap_or_default();
        let history = BootHistory::load(&data_directory).after_booting(booted);
        history.save(&data_directory).map_err(write_failure)?;
        let state = SlotState {
            environment,
            history,
            first: read_manifest(&layout.manifest(BootSlot::First)),
            second: read_manifest(&layout.manifest(BootSlot::Second)),
        };
        Ok(Self {
            layout,
            data_directory,
            booted,
            keys,
            http: http_client()?,
            restart,
            state: Mutex::new(state),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember_refusal(&self, image: &NodeImage, error: BootSlotError) -> BootSlotError {
        let mut state = self.state();
        state.history.refused = Some(RefusedImage { image: image.identity(), reason: error.to_string() });
        if let Err(saving) = state.history.save(&self.data_directory) {
            tracing::warn!(%saving, "the refusal of an image could not be kept");
        }
        error
    }

    fn change_environment(&self, change: impl FnOnce(&mut GrubEnvironment)) -> Result<(), BootSlotError> {
        let mut state = self.state();
        let mut environment = state.environment;
        change(&mut environment);
        write_environment(&self.layout.environment(), environment).map_err(write_failure)?;
        state.environment = environment;
        Ok(())
    }

    fn change_history(&self, change: impl FnOnce(&mut BootHistory)) -> Result<(), BootSlotError> {
        let mut state = self.state();
        let mut history = state.history.clone();
        change(&mut history);
        history.save(&self.data_directory).map_err(write_failure)?;
        state.history = history;
        Ok(())
    }
}

fn install_bundle(
    layout: &EspLayout,
    slot: BootSlot,
    bundle: &Path,
    image: &NodeImage,
) -> Result<(), BootSlotError> {
    let (directory, incoming) = (layout.slot_directory(slot), layout.incoming_directory(slot));
    for stale in [&directory, &incoming] {
        if stale.exists() {
            std::fs::remove_dir_all(stale).map_err(write_failure)?;
        }
    }
    let files = extract(bundle, &incoming).map_err(|error| BootSlotError::Mismatch(error.to_string()))?;
    std::fs::rename(&incoming, &directory).map_err(write_failure)?;
    let manifest = serde_json::to_vec_pretty(image).map_err(write_failure)?;
    std::fs::write(layout.manifest(slot), manifest).map_err(write_failure)?;
    synchronize_file_systems();
    tracing::info!(slot = slot.letter(), files, image = %image.identity(), "the inactive boot slot holds the image");
    Ok(())
}

fn synchronize_file_systems() {
    if let Ok(directory) = std::fs::File::open("/") {
        let _ = directory.sync_all();
    }
}

#[async_trait::async_trait]
impl BootSlots for EspBootSlots {
    fn report(&self) -> BootReport {
        let state = self.state();
        BootReport {
            booted_slot: self.booted,
            default_slot: state.environment.default_slot,
            booted_image: state.image(self.booted).map(NodeImage::identity),
            inactive_image: state.image(self.booted.other()).map(NodeImage::identity),
            fallen_back_from: state.history.fallen_back_from.clone(),
            refused: state.history.refused.clone(),
        }
    }

    async fn write_inactive_slot(&self, image: &NodeImage) -> Result<(), BootSlotError> {
        if let Err(error) = self.keys.verify(image) {
            return Err(self.remember_refusal(image, error.into()));
        }
        let inactive = self.booted.other();
        self.state().set_image(inactive, None);
        let bundle = self.data_directory.join(DOWNLOAD_FILE);
        let downloaded = download(&self.http, image, &bundle).await;
        if let Err(error) = downloaded {
            let _ = tokio::fs::remove_file(&bundle).await;
            return Err(if error.is_permanent() { self.remember_refusal(image, error) } else { error });
        }
        let installed = {
            let (layout, image, bundle) = (self.layout.clone(), image.clone(), bundle.clone());
            tokio::task::spawn_blocking(move || install_bundle(&layout, inactive, &bundle, &image))
                .await
                .map_err(write_failure)?
        };
        let _ = tokio::fs::remove_file(&bundle).await;
        if let Err(error) = installed {
            return Err(if error.is_permanent() { self.remember_refusal(image, error) } else { error });
        }
        self.state().set_image(inactive, Some(image.clone()));
        Ok(())
    }

    async fn boot_inactive_slot_next(&self) -> Result<(), BootSlotError> {
        let inactive = self.booted.other();
        let image = self
            .state()
            .image(inactive)
            .map(NodeImage::identity)
            .ok_or_else(|| BootSlotError::Write(String::from("the inactive slot holds no image")))?;
        self.change_history(|history| {
            history.trial = Some(Trial { slot: inactive, image });
            history.fallen_back_from = None;
        })?;
        self.change_environment(|environment| environment.next_slot = Some(inactive))
    }

    async fn confirm_booted_slot(&self) -> Result<(), BootSlotError> {
        let booted = self.booted;
        self.change_environment(|environment| {
            environment.default_slot = booted;
            environment.next_slot = None;
        })?;
        self.change_history(|history| history.trial = None)
    }

    async fn reboot(&self) -> Result<(), BootSlotError> {
        self.restart.restart().map_err(|error| BootSlotError::Reboot(error.to_string()))
    }
}
