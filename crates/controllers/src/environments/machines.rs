use std::collections::BTreeMap;

use amafu_domain::kinds::machine_shape::MachineShape;
use amafu_domain::kinds::vm::{
    BootSource, NetworkInterfaceSpecification, PowerState, Vm, VmPhase, VmSpecification,
};
use amafu_domain::kinds::volume::{Volume, VolumeSpecification};
use amafu_domain::{Object, ObjectName, ObjectReference};
use amafu_interfaces::ActionError;

use super::{EnvironmentObjects, Ownership};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinePlan {
    pub shape: MachineShape,
    pub image: ObjectName,
    pub network: ObjectName,
    pub storage_backend: ObjectName,
    pub security_groups: Vec<ObjectName>,
    pub user_data: String,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPlan {
    pub image: ObjectName,
    pub environment: BTreeMap<String, String>,
    pub network: ObjectName,
    pub security_groups: Vec<ObjectName>,
}

pub struct Machine<'a> {
    objects: EnvironmentObjects<'a>,
    ownership: &'a Ownership,
    name: ObjectName,
}

impl<'a> Machine<'a> {
    #[must_use]
    pub fn new(objects: EnvironmentObjects<'a>, ownership: &'a Ownership, name: ObjectName) -> Self {
        Self { objects, ownership, name }
    }

    #[must_use]
    pub fn name(&self) -> &ObjectName {
        &self.name
    }

    fn vm_reference(&self) -> ObjectReference {
        self.ownership.reference::<Vm>(self.name.clone())
    }

    fn volume_reference(&self) -> ObjectReference {
        self.ownership.reference::<Volume>(self.name.clone())
    }

    pub async fn vm(&self) -> Result<Option<Object<Vm>>, ActionError> {
        self.objects.read::<Vm>(&self.vm_reference()).await
    }

    pub async fn phase(&self) -> Result<Option<VmPhase>, ActionError> {
        Ok(self.vm().await?.map(|vm| vm.status.phase))
    }

    pub async fn is_running(&self) -> Result<bool, ActionError> {
        Ok(self.phase().await? == Some(VmPhase::Running))
    }

    fn labels(&self, extra: BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut labels = self.ownership.labels();
        labels.extend(extra);
        labels
    }

    pub async fn create(&self, plan: MachinePlan) -> Result<(), ActionError> {
        let spread = Some(self.ownership.environment().clone());
        self.create_with(plan, spread).await
    }

    pub async fn run_once(&self, plan: MachinePlan) -> Result<(), ActionError> {
        self.create_with(plan, None).await
    }

    async fn create_with(
        &self,
        plan: MachinePlan,
        anti_affinity_group: Option<ObjectName>,
    ) -> Result<(), ActionError> {
        let labels = self.labels(plan.labels);
        let volume = VolumeSpecification {
            size_gibibytes: plan.shape.disk_gibibytes(),
            backend: plan.storage_backend,
            source_image: Some(plan.image),
        };
        self.objects.ensure::<Volume>(&self.volume_reference(), labels.clone(), volume).await?;
        let vm = VmSpecification {
            cpu_count: plan.shape.cpu_count(),
            memory_mebibytes: u64::from(plan.shape.memory_mebibytes()),
            boot: BootSource::Disk,
            volumes: vec![self.name.clone()],
            network_interfaces: vec![NetworkInterfaceSpecification {
                network: plan.network,
                security_groups: plan.security_groups,
                address: None,
            }],
            power_state: PowerState::Running,
            user_data: Some(plan.user_data),
            priority: 0,
            anti_affinity_group,
            minimum_cpu_baseline: None,
            ssh_keys: Vec::new(),
        };
        self.objects.ensure::<Vm>(&self.vm_reference(), labels, vm).await?;
        Ok(())
    }

    pub async fn run_container(&self, plan: ContainerPlan) -> Result<(), ActionError> {
        let shape = MachineShape::Small;
        let vm = VmSpecification {
            cpu_count: shape.cpu_count(),
            memory_mebibytes: u64::from(shape.memory_mebibytes()),
            boot: BootSource::Container {
                image: plan.image,
                command: Vec::new(),
                environment: plan.environment,
            },
            volumes: Vec::new(),
            network_interfaces: vec![NetworkInterfaceSpecification {
                network: plan.network,
                security_groups: plan.security_groups,
                address: None,
            }],
            power_state: PowerState::Running,
            user_data: None,
            priority: 0,
            anti_affinity_group: None,
            minimum_cpu_baseline: None,
            ssh_keys: Vec::new(),
        };
        self.objects.ensure::<Vm>(&self.vm_reference(), self.labels(BTreeMap::new()), vm).await?;
        Ok(())
    }

    pub async fn set_power(&self, power_state: PowerState) -> Result<(), ActionError> {
        self.objects
            .change_specification::<Vm>(&self.vm_reference(), |current| {
                (current.power_state != power_state)
                    .then(|| VmSpecification { power_state, ..current.clone() })
            })
            .await
    }

    pub async fn remove(&self) -> Result<bool, ActionError> {
        if !self.objects.delete::<Vm>(&self.vm_reference()).await? {
            return Ok(false);
        }
        self.objects.delete::<Volume>(&self.volume_reference()).await
    }
}
