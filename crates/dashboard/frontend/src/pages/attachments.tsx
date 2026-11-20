import type { Network, SecurityGroup, StorageBackend, Volume } from "../cluster/kinds";
import { useCollection } from "../cluster/live";
import ChipSelect from "../components/ChipSelect";
import type { Choice } from "../components/SearchSelect";
import { useProject } from "../session";

export function useNetworkChoices(): Choice[] {
  const project = useProject();
  const networks = useCollection<Network>({ plural: "networks", project });
  return networks.items.map((network) => ({ value: network.metadata.name, label: network.metadata.name, detail: network.specification.subnet }));
}

export function useSecurityGroupChoices(): Choice[] {
  const project = useProject();
  const groups = useCollection<SecurityGroup>({ plural: "security-groups", project });
  return groups.items.map((group) => ({
    value: group.metadata.name,
    label: group.metadata.name,
    detail: `${group.specification.rules.length} rules`,
  }));
}

export function useFreeVolumeChoices(): Choice[] {
  const project = useProject();
  const volumes = useCollection<Volume>({ plural: "volumes", project });
  return volumes.items
    .filter((volume) => !volume.status.attached_to && !volume.metadata.is_deleting)
    .map((volume) => ({
      value: volume.metadata.name,
      label: volume.metadata.name,
      detail: [`${volume.specification.size_gibibytes} GiB`, volume.specification.backend, volume.specification.source_image].filter(Boolean).join(" · "),
    }));
}

export function useBackendChoices(): Choice[] {
  const backends = useCollection<StorageBackend>({ plural: "storage-backends" });
  return backends.items.map((backend) => ({
    value: backend.metadata.name,
    label: backend.metadata.name,
    detail: backend.specification.provider.type === "local-disk" ? "local disk, cannot move" : backend.specification.provider.type,
  }));
}

interface AttachmentProps {
  chosen: string[];
  onChange: (chosen: string[]) => void;
}

export function NetworkChoices({ chosen, onChange }: AttachmentProps) {
  return (
    <ChipSelect
      id="networks"
      label="Networks"
      choices={useNetworkChoices()}
      selected={chosen}
      onChange={onChange}
      placeholder="Search networks"
      emptyMessage="No networks in this project yet"
      help="One interface on each network, in this order."
    />
  );
}

export function SecurityGroupChoices({ chosen, onChange }: AttachmentProps) {
  return (
    <ChipSelect
      id="security-groups"
      label="Security groups"
      choices={useSecurityGroupChoices()}
      selected={chosen}
      onChange={onChange}
      placeholder="Search security groups"
      emptyMessage="No security groups in this project yet"
      help="Applied to every interface."
    />
  );
}
