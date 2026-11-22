use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use amafu_controllers::environments::juju::{JujuEnvironment, JujuSettings, JujuSteps};
use amafu_controllers::environments::kubernetes::{
    IssuerTrust, KubernetesEnvironment, KubernetesSettings, KubernetesSteps,
};
use amafu_domain::kinds::environment_settings::{
    ENVIRONMENT_SETTINGS_NAME, EnvironmentSettings, EnvironmentSettingsSpecification,
};
use amafu_domain::kinds::floating_ip::FloatingIp;
use amafu_domain::kinds::image::{Image, ImagePhase, ImageSource};
use amafu_domain::kinds::operation::OperationOutcome;
use amafu_domain::kinds::security_group::SecurityGroup;
use amafu_domain::kinds::signing_key::{SigningKey, SigningKeySpecification};
use amafu_domain::kinds::vm::{BootSource, InterfaceStatus, PowerState, Vm, VmPhase};
use amafu_domain::kinds::volume::Volume;
use amafu_domain::labels::JUJU_MACHINE;
use amafu_domain::step_plan::Runnable;
use amafu_domain::{
    Change, MacAddress, Object, ObjectId, ObjectName, ObjectReference, ProjectName, Resource,
};
use amafu_interfaces::objects::{apply, get, list, remove, update_status};
use amafu_interfaces::{Consistency, Fence, JujuPlatform, KubernetesPlatform, Store};
use amafu_reconciler::{OperationContext, StepRegistry, advance};
use amafu_token_issuer::{KeyPair, Keyring};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use time::{Duration, OffsetDateTime};

use crate::VirtualClock;
use crate::fakes::{FakeJuju, FakeKubernetes, MemoryStore, SeededIdentifiers};

pub const PROJECT: &str = "web-team";
pub const FLOATING_ADDRESS: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);
pub const SERVICE_ADDRESS: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 9);
const PASSES: usize = 400;

#[must_use]
pub fn name(value: &str) -> ObjectName {
    ObjectName::new(value).unwrap_or_else(|error| panic!("{value} is not a name: {error}"))
}

#[must_use]
pub fn project() -> ProjectName {
    ProjectName::new(PROJECT).unwrap_or_else(|error| panic!("{error}"))
}

#[must_use]
pub fn reference<R: Resource>(object: &str) -> ObjectReference {
    ObjectReference::of::<R>(Some(project()), name(object))
}

const RUNNER_IMAGE: &str = "registry.example/amafu-juju-runner@sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn ubuntu_image() -> ImageSource {
    ImageSource::Disk {
        url: String::from("https://images.example/noble.img"),
        sha256: "0".repeat(64).parse().unwrap_or_else(|error| panic!("{error}")),
    }
}

fn trust() -> IssuerTrust {
    IssuerTrust {
        issuer: String::from("https://amafu.internal"),
        discovery_url: String::from("https://amafu.example/.well-known/openid-configuration"),
        certificate_authority: String::from("amafu-ca"),
    }
}

pub struct EnvironmentWorld {
    pub store: Arc<MemoryStore>,
    pub kubernetes: Arc<FakeKubernetes>,
    pub juju: Arc<FakeJuju>,
    clock: VirtualClock,
}

impl EnvironmentWorld {
    pub async fn new() -> Self {
        let store = Arc::new(MemoryStore::new());
        let now = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        let key = KeyPair::generate().unwrap_or_else(|error| panic!("{error}"));
        let signing = SigningKeySpecification::window_starting(STANDARD.encode(key.pkcs8()), now);
        let world = Self {
            store,
            kubernetes: Arc::new(FakeKubernetes::new()),
            juju: Arc::new(FakeJuju::new()),
            clock: VirtualClock::starting_at(now),
        };
        world.create::<SigningKey>(None, "key-1", signing).await;
        let settings = EnvironmentSettingsSpecification {
            ubuntu_image: Some(ubuntu_image()),
            juju_runner_image: Some(RUNNER_IMAGE.parse().unwrap_or_else(|error| panic!("{error}"))),
        };
        world.create::<EnvironmentSettings>(None, ENVIRONMENT_SETTINGS_NAME, settings).await;
        world
    }

    #[must_use]
    pub fn keyring(&self) -> Keyring {
        Keyring::new(Arc::clone(&self.store) as Arc<dyn Store>)
    }

    #[must_use]
    pub fn kubernetes_steps(&self) -> KubernetesSteps {
        KubernetesSteps::new(KubernetesEnvironment {
            platform: Arc::clone(&self.kubernetes) as Arc<dyn KubernetesPlatform>,
            keyring: self.keyring(),
            identifiers: Arc::new(SeededIdentifiers::new(7)),
            settings: KubernetesSettings { trust: trust() },
        })
    }

    #[must_use]
    pub fn juju_steps(&self) -> JujuSteps {
        JujuSteps::new(JujuEnvironment {
            platform: Arc::clone(&self.juju) as Arc<dyn JujuPlatform>,
            kubernetes: Arc::clone(&self.kubernetes) as Arc<dyn KubernetesPlatform>,
            keyring: self.keyring(),
            identifiers: Arc::new(SeededIdentifiers::new(11)),
            settings: JujuSettings {
                token_refresh_url: String::from("https://amafu.example/.well-known/jwks.json"),
                amafu_certificate_authority: String::from("amafu-ca"),
            },
        })
    }

    pub async fn create<R: Resource>(
        &self,
        project: Option<ProjectName>,
        object: &str,
        specification: R::Specification,
    ) {
        let reference = ObjectReference::of::<R>(project.clone(), name(object));
        let change = Change::Create {
            id: ObjectId::from_uuid(uuid::Uuid::new_v4()),
            name: name(object),
            project,
            labels: BTreeMap::new(),
            specification,
        };
        apply::<R>(&*self.store, &reference, change).await.unwrap_or_else(|error| panic!("{error}"));
    }

    pub async fn set_status<R: Resource>(
        &self,
        object: &Object<R>,
        change: impl Fn(&mut R::Status) + Send + Sync,
    ) where
        R::Status: PartialEq,
    {
        update_status::<R>(&*self.store, &object.reference(), |current| {
            let mut status = current.status.clone();
            change(&mut status);
            (status != current.status).then_some(status)
        })
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    }

    pub async fn get<R: Resource>(&self, object: &str) -> Option<Object<R>> {
        get::<R>(&*self.store, &reference::<R>(object), Consistency::Linearizable)
            .await
            .unwrap_or_else(|error| panic!("{error}"))
    }

    pub async fn owned<R: Resource>(&self) -> Vec<Object<R>> {
        list::<R>(&*self.store, Some(&project()), Consistency::Linearizable)
            .await
            .unwrap_or_else(|error| panic!("{error}"))
    }

    pub async fn run<K: Runnable>(
        &self,
        steps: &dyn StepRegistry<K>,
        change: &str,
        specification: K::Specification,
    ) -> OperationOutcome {
        self.create::<K>(Some(project()), change, specification).await;
        let fence = Fence { name: String::from("environments"), token: 1 };
        for _ in 0..PASSES {
            let operation = self.get::<K>(change).await.unwrap_or_else(|| panic!("{change} vanished"));
            if let Some(outcome) = operation.status.outcome.clone() {
                return outcome;
            }
            let context = OperationContext {
                operation: &operation,
                store: self.store.as_ref(),
                clock: &self.clock,
                fence: &fence,
            };
            drop(advance(&context, steps).await);
            self.settle().await;
        }
        panic!("the change {change} did not finish");
    }

    pub async fn settle(&self) {
        for image in self.owned::<Image>().await {
            self.set_status::<Image>(&image, |status| status.phase = ImagePhase::Ready).await;
        }
        for address in self.owned::<FloatingIp>().await {
            self.set_status::<FloatingIp>(&address, |status| status.address = Some(FLOATING_ADDRESS)).await;
        }
        for (index, vm) in self.owned::<Vm>().await.into_iter().enumerate() {
            self.settle_vm(index, &vm).await;
        }
        self.forget_deleted::<Vm>().await;
        self.forget_deleted::<Volume>().await;
        self.forget_deleted::<FloatingIp>().await;
        self.forget_deleted::<SecurityGroup>().await;
    }

    async fn settle_vm(&self, index: usize, vm: &Object<Vm>) {
        if let BootSource::Container { environment, .. } = &vm.specification.boot {
            self.juju.bootstrap(environment.get("JUJU_VERSION").map_or("4.2.1", String::as_str));
            if environment.get("JUJU_SUBSTRATE").map(String::as_str) == Some("kubernetes") {
                let controller = environment.get("JUJU_CONTROLLER_NAME").cloned().unwrap_or_default();
                self.kubernetes.put(&json!({
                    "apiVersion": "v1",
                    "kind": "Service",
                    "metadata": {"name": "controller-service", "namespace": format!("controller-{controller}")},
                    "status": {"loadBalancer": {"ingress": [{"ip": SERVICE_ADDRESS.to_string()}]}},
                }));
            }
            self.set_status::<Vm>(vm, |status| status.phase = VmPhase::Stopped).await;
            return;
        }
        let phase = match vm.specification.power_state {
            PowerState::Running => VmPhase::Running,
            PowerState::Stopped => VmPhase::Stopped,
        };
        let host = u8::try_from(index % 200).unwrap_or_default() + 10;
        let interfaces: Vec<InterfaceStatus> = vm
            .specification
            .network_interfaces
            .iter()
            .map(|interface| InterfaceStatus {
                network: interface.network.clone(),
                mac: MacAddress::new([2, 0, 0, 0, 0, host]),
                address: Ipv4Addr::new(10, 1, 0, host),
            })
            .collect();
        self.set_status::<Vm>(vm, |status| {
            status.phase = phase;
            status.interfaces.clone_from(&interfaces);
        })
        .await;
        if let Some(machine) = vm.metadata.labels.get(JUJU_MACHINE) {
            self.juju.start(machine);
        }
        if vm.metadata.name.as_str().ends_with("-cp-0") && self.kubernetes.nodes().is_empty() {
            self.kubernetes.bootstrap(vm.metadata.name.as_str());
        }
    }

    pub async fn forget<R: Resource>(&self, object: &str) {
        let target = reference::<R>(object);
        if let Some(found) = self.get::<R>(object).await
            && !found.metadata.is_deleting
        {
            apply::<R>(&*self.store, &target, Change::Delete).await.unwrap_or_else(|error| panic!("{error}"));
        }
        remove::<R>(&*self.store, &target).await.unwrap_or_else(|error| panic!("{error}"));
    }

    async fn forget_deleted<R: Resource>(&self) {
        for object in self.owned::<R>().await.into_iter().filter(|object| object.metadata.is_deleting) {
            remove::<R>(&*self.store, &object.reference()).await.unwrap_or_else(|error| panic!("{error}"));
        }
    }
}
