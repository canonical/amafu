use amafu_domain::ObjectId;
use amafu_domain::kinds::image::ImageSource;
use amafu_interfaces::{ImageRequest, ImageStore};
use amafu_storage::CachedImages;

const BUSYBOX: &str = "docker.io/library/busybox@sha256:a7dbc319b1438115a207e11930dc4bb3e643dcb3ec0bdd694bb36cd8fad289ec";

#[tokio::test]
#[ignore = "needs access to Docker Hub and mkfs.erofs"]
async fn a_pinned_container_image_is_pulled_into_an_erofs_root() {
    let directory = tempfile::tempdir().unwrap();
    let images = CachedImages::new(directory.path().to_path_buf(), "mkfs.erofs".into()).unwrap();
    let request = ImageRequest {
        image_id: ObjectId::from_uuid(uuid::Uuid::from_u128(1)),
        source: ImageSource::Container { reference: BUSYBOX.parse().unwrap() },
    };
    let cached = images.ensure(&request).await.unwrap();
    assert!(cached.path.ends_with("root.erofs"));
    assert!(cached.size_bytes > 1024 * 1024);
    assert_eq!(cached.container.unwrap().command, vec!["sh"]);
    assert_eq!(images.cached(&request.image_id).await.unwrap().map(|image| image.path), Some(cached.path));
    images.remove(&request.image_id).await.unwrap();
    assert_eq!(images.cached(&request.image_id).await.unwrap(), None);
}
