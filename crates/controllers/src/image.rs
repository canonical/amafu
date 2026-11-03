use std::collections::BTreeSet;
use std::sync::Arc;

use amafu_domain::kinds::image::{Image, ImagePhase, ImageStatus};
use amafu_domain::kinds::vm::Vm;
use amafu_domain::kinds::volume::Volume;
use amafu_domain::{Change, NodeName, Object, ObjectReference};
use amafu_interfaces::objects::{apply, get, list, remove, update_status};
use amafu_interfaces::{ActionError, CachedImage, Consistency, ImageRequest, ImageStore, Store};
use amafu_reconciler::{Controller, KeyMapping, WatchedKind};

pub struct ImageController {
    node: NodeName,
    store: Arc<dyn Store>,
    images: Arc<dyn ImageStore>,
    sees_every_user: bool,
}

impl ImageController {
    #[must_use]
    pub fn new(
        node: NodeName,
        store: Arc<dyn Store>,
        images: Arc<dyn ImageStore>,
        sees_every_user: bool,
    ) -> Self {
        Self { node, store, images, sees_every_user }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageObservation {
    pub node: NodeName,
    pub image: Option<Object<Image>>,
    pub cached: Option<CachedImage>,
    pub is_in_use: bool,
    pub sees_every_user: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageAction {
    Fetch { request: ImageRequest, observed_generation: u64 },
    ReportCached { size_bytes: u64 },
    DiscardCache,
    ReportDiscarded,
    Remove,
}

fn plan_deletion(image: &Object<Image>, observation: &ImageObservation) -> Vec<ImageAction> {
    if observation.is_in_use {
        return Vec::new();
    }
    if observation.cached.is_some() {
        return vec![ImageAction::DiscardCache];
    }
    if image.status.cached_on.contains(&observation.node) {
        return vec![ImageAction::ReportDiscarded];
    }
    if image.status.cached_on.is_empty() && observation.sees_every_user {
        return vec![ImageAction::Remove];
    }
    Vec::new()
}

#[must_use]
pub fn plan_image(observation: &ImageObservation) -> Vec<ImageAction> {
    let Some(image) = &observation.image else {
        return Vec::new();
    };
    if image.metadata.is_deleting {
        return plan_deletion(image, observation);
    }
    match &observation.cached {
        None if image.status.phase == ImagePhase::Failed && image.is_status_current() => Vec::new(),
        None => vec![ImageAction::Fetch {
            request: ImageRequest { image_id: image.metadata.id, source: image.specification.source.clone() },
            observed_generation: image.metadata.generation,
        }],
        Some(cached) => {
            let is_reported = image.status.phase == ImagePhase::Ready
                && image.status.size_bytes == cached.size_bytes
                && image.status.cached_on.contains(&observation.node)
                && image.is_status_current();
            if is_reported {
                Vec::new()
            } else {
                vec![ImageAction::ReportCached { size_bytes: cached.size_bytes }]
            }
        }
    }
}

fn with_node_cached(image: &Object<Image>, node: &NodeName, size_bytes: u64) -> Option<ImageStatus> {
    let mut cached_on = image.status.cached_on.clone();
    cached_on.insert(node.clone());
    let status = ImageStatus { phase: ImagePhase::Ready, size_bytes, cached_on, message: None };
    (status != image.status || !image.is_status_current()).then_some(status)
}

fn without_node(image: &Object<Image>, node: &NodeName) -> Option<ImageStatus> {
    let mut cached_on: BTreeSet<NodeName> = image.status.cached_on.clone();
    cached_on.remove(node).then(|| ImageStatus { cached_on, ..image.status.clone() })
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
        Ok(ImageObservation {
            node: self.node.clone(),
            image,
            cached,
            is_in_use,
            sees_every_user: self.sees_every_user,
        })
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
            ImageAction::ReportCached { size_bytes } => {
                update_status::<Image>(store, key, |image| with_node_cached(image, &self.node, size_bytes))
                    .await?;
                Ok(())
            }
            ImageAction::ReportDiscarded => {
                update_status::<Image>(store, key, |image| without_node(image, &self.node)).await?;
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
            sees_every_user: true,
        };
        assert!(matches!(plan_image(&fresh).as_slice(), [ImageAction::Fetch { .. }]));
        let fetched = ImageObservation { cached: Some(cached()), ..fresh };
        assert_eq!(only(plan_image(&fetched)), ImageAction::ReportCached { size_bytes: 42 });
    }

    #[test]
    fn each_node_adds_itself_to_the_nodes_caching_the_image() {
        let mut elsewhere = image(ImagePhase::Ready);
        elsewhere.status.cached_on.insert(NodeName::new("node-9").unwrap());
        elsewhere.status.size_bytes = 42;
        let status = with_node_cached(&elsewhere, &here(), 42).unwrap();
        assert_eq!(status.cached_on.len(), 2);
        let mut reported = elsewhere.clone();
        reported.status = status;
        assert_eq!(with_node_cached(&reported, &here(), 42), None);
        assert_eq!(without_node(&reported, &here()).unwrap().cached_on, elsewhere.status.cached_on);
        let observation = ImageObservation {
            node: here(),
            image: Some(reported),
            cached: Some(cached()),
            is_in_use: false,
            sees_every_user: false,
        };
        assert_eq!(plan_image(&observation), Vec::new());
    }

    #[test]
    fn a_failed_image_is_not_fetched_again_at_the_same_generation() {
        let failed = ImageObservation {
            node: here(),
            image: Some(image(ImagePhase::Failed)),
            cached: None,
            is_in_use: false,
            sees_every_user: true,
        };
        assert_eq!(plan_image(&failed), Vec::new());
    }

    #[test]
    fn a_deleted_image_discards_every_cache_then_disappears() {
        let mut deleting = image(ImagePhase::Ready);
        deleting.metadata.is_deleting = true;
        deleting.status.cached_on = BTreeSet::from([here()]);
        let in_use = ImageObservation {
            node: here(),
            image: Some(deleting.clone()),
            cached: Some(cached()),
            is_in_use: true,
            sees_every_user: true,
        };
        assert_eq!(plan_image(&in_use), Vec::new());
        let with_cache = ImageObservation { is_in_use: false, ..in_use };
        assert_eq!(plan_image(&with_cache), vec![ImageAction::DiscardCache]);
        let without_cache = ImageObservation { cached: None, ..with_cache };
        assert_eq!(plan_image(&without_cache), vec![ImageAction::ReportDiscarded]);
        deleting.status.cached_on.clear();
        let forgotten = ImageObservation { image: Some(deleting), ..without_cache };
        assert_eq!(plan_image(&forgotten), vec![ImageAction::Remove]);
        let partial_view = ImageObservation { sees_every_user: false, ..forgotten };
        assert_eq!(plan_image(&partial_view), Vec::new());
    }
}
