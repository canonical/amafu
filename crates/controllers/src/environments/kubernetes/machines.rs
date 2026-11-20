use amafu_domain::kinds::kubernetes_cluster::{ClusterMember, KubernetesCluster};
use amafu_domain::kinds::vm::{BootSource, NetworkInterfaceSpecification, PowerState, Vm, VmSpecification};
use amafu_domain::kinds::volume::{Volume, VolumeSpecification};
use amafu_domain::{Object, ObjectName, ObjectReference};
use amafu_interfaces::ActionError;

use super::scope::{ClusterScope, UBUNTU_IMAGE};

pub struct Machine<'a> {
    scope: &'a ClusterScope<'a>,
    name: ObjectName,
}

impl<'a> Machine<'a> {
    pub fn of(scope: &'a ClusterScope<'a>, member: &ClusterMember) -> Result<Self, ActionError> {
        let name =
            ObjectName::new(member.vm_name(scope.ownership.environment())).map_err(ActionError::fatal)?;
        Ok(Self { scope, name })
    }

    #[must_use]
    pub fn name(&self) -> &ObjectName {
        &self.name
    }

    fn vm_reference(&self) -> ObjectReference {
        self.scope.ownership.reference::<Vm>(self.name.clone())
    }

    fn volume_reference(&self) -> ObjectReference {
        self.scope.ownership.reference::<Volume>(self.name.clone())
    }

    pub async fn vm(&self) -> Result<Option<Object<Vm>>, ActionError> {
        self.scope.objects().read::<Vm>(&self.vm_reference()).await
    }

    pub async fn create(
        &self,
        cluster: &Object<KubernetesCluster>,
        member: &ClusterMember,
        user_data: String,
    ) -> Result<(), ActionError> {
        let objects = self.scope.objects();
        let labels = self.scope.ownership.labels();
        let shape = self.scope.shape_of(cluster, member);
        let image = ObjectName::new(UBUNTU_IMAGE).map_err(ActionError::fatal)?;
        let volume = VolumeSpecification {
            size_gibibytes: shape.disk_gibibytes(),
            backend: cluster.specification.storage_backend.clone(),
            source_image: Some(image),
        };
        objects.ensure::<Volume>(&self.volume_reference(), labels.clone(), volume).await?;
        let vm = VmSpecification {
            cpu_count: shape.cpu_count(),
            memory_mebibytes: u64::from(shape.memory_mebibytes()),
            boot: BootSource::Disk,
            volumes: vec![self.name.clone()],
            network_interfaces: vec![NetworkInterfaceSpecification {
                network: cluster.specification.network.clone(),
                security_groups: vec![self.scope.security_group()?],
                address: None,
            }],
            power_state: PowerState::Running,
            user_data: Some(user_data),
            priority: 0,
            anti_affinity_group: Some(self.scope.ownership.environment().clone()),
            minimum_cpu_baseline: None,
        };
        objects.ensure::<Vm>(&self.vm_reference(), labels, vm).await?;
        Ok(())
    }

    pub async fn set_power(&self, power_state: PowerState) -> Result<(), ActionError> {
        self.scope
            .objects()
            .change_specification::<Vm>(&self.vm_reference(), |current| {
                (current.power_state != power_state)
                    .then(|| VmSpecification { power_state, ..current.clone() })
            })
            .await
    }

    pub async fn remove(&self) -> Result<bool, ActionError> {
        let objects = self.scope.objects();
        if !objects.delete::<Vm>(&self.vm_reference()).await? {
            return Ok(false);
        }
        objects.delete::<Volume>(&self.volume_reference()).await
    }
}
