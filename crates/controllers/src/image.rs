use std::collections::BTreeSet;
use std::sync::Arc;

use amafu_domain::kinds::image::{Image, ImagePhase, ImageStatus};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, NodeName, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove};
use amafu_interfaces::{ActionError, CachedImage, Consistency, ImageRequest, ImageStore, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct ImageController {
    node: NodeName,
    store: Arc<dyn Store>,
    images: Arc<dyn ImageStore>,
}

impl ImageController {
    #[must_use]
    pub fn new(node: NodeName, store: Arc<dyn Store>, images: Arc<dyn ImageStore>) -> Self {
        Self { node, store, images }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageObservation {
    pub node: NodeName,
    pub image: Option<Object<Image>>,
    pub cached: Option<CachedImage>,
    pub is_in_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageAction {
    Fetch { request: ImageRequest, observed_generation: u64 },
    Report { observed_generation: u64, status: ImageStatus },
    DiscardCache,
    Remove,
}

#[must_use]
pub fn plan_image(observation: &ImageObservation) -> Vec<ImageAction> {
    let Some(image) = &observation.image else {
        return Vec::new();
    };
    if image.metadata.is_deleting {
        return if observation.is_in_use {
            Vec::new()
        } else if observation.cached.is_some() {
            vec![ImageAction::DiscardCache]
        } else {
            vec![ImageAction::Remove]
        };
    }
    let observed_generation = image.metadata.generation;
    match &observation.cached {
        None if image.status.phase == ImagePhase::Failed && image.is_status_current() => Vec::new(),
        None => vec![ImageAction::Fetch {
            request: ImageRequest { image_id: image.metadata.id, source: image.specification.source.clone() },
            observed_generation,
        }],
        Some(cached) => {
            let status = ImageStatus {
                phase: ImagePhase::Ready,
                size_bytes: cached.size_bytes,
                cached_on: BTreeSet::from([observation.node.clone()]),
                message: None,
            };
            if image.status == status && image.is_status_current() {
                Vec::new()
            } else {
                vec![ImageAction::Report { observed_generation, status }]
            }
        }
    }
}

impl ImageController {
    async fn is_in_use(&self, image: &Object<Image>) -> Result<bool, ActionError> {
        let (store, project, name) =
            (self.store.as_ref(), image.metadata.project.as_ref(), &image.metadata.name);
        let volumes = list::<Volume>(store, project, Consistency::Linearizable).await?;
        if volumes.iter().any(|volume| volume.specification.source_image.as_ref() == Some(name)) {
            return Ok(true);
        }
        let vms = list::<Vm>(store, project, Consistency::Linearizable).await?;
        Ok(vms.iter().any(|vm| vm.specification.container_image() == Some(name)))
    }
}

impl Controller for ImageController {
    type Key = ObjectReference;
    type Observation = ImageObservation;
    type Action = ImageAction;

    const NAME: &'static str = "image";
    const WATCHES: &'static [WatchedKind<ObjectReference>] = &[
        WatchedKind { kind: "Image", mapping: KeyMapping::ObjectKey(|reference| Some(reference.clone())) },
        WatchedKind { kind: "Volume", mapping: KeyMapping::EveryKey },
        WatchedKind { kind: "Vm", mapping: KeyMapping::EveryKey },
    ];

    async fn discover_keys(&self) -> Result<Vec<ObjectReference>, ActionError> {
        let images = list::<Image>(self.store.as_ref(), None, Consistency::Linearizable).await?;
        Ok(images.iter().map(Object::reference).collect())
    }

    async fn observe(&self, key: &ObjectReference) -> Result<ImageObservation, ActionError> {
        let image = get::<Image>(self.store.as_ref(), key, Consistency::Linearizable).await?;
        let cached = match &image {
            Some(image) => self.images.cached(&image.metadata.id).await?,
            None => None,
        };
        let is_in_use = match &image {
            Some(image) => self.is_in_use(image).await?,
            None => false,
        };
        Ok(ImageObservation { node: self.node.clone(), image, cached, is_in_use })
    }

    fn plan(&self, observation: &ImageObservation) -> Vec<ImageAction> {
        plan_image(observation)
    }

    async fn apply(&self, key: &ObjectReference, action: ImageAction) -> Result<(), ActionError> {
        let store = self.store.as_ref();
        match action {
            ImageAction::Fetch { request, observed_generation } => match self.images.ensure(&request).await {
                Ok(_) => Ok(()),
                Err(ActionError::Invalid(reason)) => {
                    let status = ImageStatus {
                        phase: ImagePhase::Failed,
                        message: Some(reason.clone()),
                        ..ImageStatus::default()
                    };
                    apply::<Image>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
                    Err(ActionError::Invalid(reason))
                }
                Err(error) => Err(error),
            },
            ImageAction::Report { observed_generation, status } => {
                apply::<Image>(store, key, Change::UpdateStatus { observed_generation, status }).await?;
                Ok(())
            }
            ImageAction::DiscardCache => {
                let image = get::<Image>(store, key, Consistency::Linearizable).await?;
                if let Some(image) = image {
                    self.images.remove(&image.metadata.id).await?;
                }
                Ok(())
            }
            ImageAction::Remove => Ok(remove::<Image>(store, key).await?),
        }
    }
}

#[cfg(test)]
mod tests {
    use amafu_domain::kinds::image::{ImageSource, ImageSpecification};

    use super::*;
    use crate::testing::{metadata, only};

    fn here() -> NodeName {
        NodeName::new("node-0").unwrap()
    }

    fn image(phase: ImagePhase) -> Object<Image> {
        let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".parse().unwrap();
        Object {
            metadata: metadata("ubuntu", Some("project-a")),
            specification: ImageSpecification {
                source: ImageSource::Disk { url: String::from("https://example.com/u.img"), sha256 },
            },
            status: ImageStatus { phase, ..ImageStatus::default() },
        }
    }

    fn cached() -> CachedImage {
        CachedImage { path: "/var/lib/amafu/images/ubuntu".into(), size_bytes: 42, container: None }
    }

    #[test]
    fn an_uncached_image_is_fetched_then_reported_ready() {
        let fresh = ImageObservation {
            node: here(),
            image: Some(image(ImagePhase::Pending)),
            cached: None,
            is_in_use: false,
        };
        assert!(matches!(plan_image(&fresh).as_slice(), [ImageAction::Fetch { .. }]));
        let fetched = ImageObservation { cached: Some(cached()), ..fresh };
        let ImageAction::Report { status, .. } = only(plan_image(&fetched)) else {
            panic!("expected a report")
        };
        assert_eq!(status.size_bytes, 42);
        assert!(status.cached_on.contains(&here()));
    }

    #[test]
    fn a_failed_image_is_not_fetched_again_at_the_same_generation() {
        let failed = ImageObservation {
            node: here(),
            image: Some(image(ImagePhase::Failed)),
            cached: None,
            is_in_use: false,
        };
        assert_eq!(plan_image(&failed), Vec::new());
    }

    #[test]
    fn a_deleted_image_discards_its_cache_then_disappears() {
        let mut deleting = image(ImagePhase::Ready);
        deleting.metadata.is_deleting = true;
        let in_use =
            ImageObservation { node: here(), image: Some(deleting), cached: Some(cached()), is_in_use: true };
        assert_eq!(plan_image(&in_use), Vec::new());
        let with_cache = ImageObservation { is_in_use: false, ..in_use };
        assert_eq!(plan_image(&with_cache), vec![ImageAction::DiscardCache]);
        let without_cache = ImageObservation { cached: None, ..with_cache };
        assert_eq!(plan_image(&without_cache), vec![ImageAction::Remove]);
    }
}
