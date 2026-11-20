import { Input, Select } from "@canonical/react-components";

import type { ExternalNetwork, Network, StorageBackend } from "../cluster/kinds";
import { SHAPES } from "../cluster/environments";
import type { MachineShape } from "../cluster/environments";
import { useCollection } from "../cluster/live";
import SearchSelect from "../components/SearchSelect";
import { useProject } from "../session";

const NAME_PATTERN = /^[a-z][a-z0-9-]*$/;

export function nameProblem(name: string, taken: string[]): string | undefined {
  if (!name) {
    return undefined;
  }
  if (!NAME_PATTERN.test(name)) {
    return "Lowercase letters, digits and hyphens, starting with a letter";
  }
  return taken.includes(name) ? "This name is in use" : undefined;
}

export function ShapeSelect({
  id,
  label,
  value,
  onChange,
}: {
  id: string;
  label: string;
  value: MachineShape;
  onChange: (shape: MachineShape) => void;
}) {
  return (
    <Select
      id={id}
      label={label}
      value={value}
      onChange={(event) => onChange(event.target.value as MachineShape)}
      options={SHAPES}
    />
  );
}

export interface Placement {
  network: string;
  storageBackend: string;
  externalNetwork: string;
}

export function usePlacementChoices() {
  const project = useProject();
  const networks = useCollection<Network>({ plural: "networks", project });
  const backends = useCollection<StorageBackend>({ plural: "storage-backends" });
  const external = useCollection<ExternalNetwork>({ plural: "external-networks" });
  return {
    networks: networks.items.map((item) => item.metadata.name),
    backends: backends.items.map((item) => item.metadata.name),
    externalNetworks: external.items.map((item) => item.metadata.name),
  };
}

export function chosen(placement: Placement, choices: ReturnType<typeof usePlacementChoices>): Placement {
  return {
    network: placement.network || choices.networks[0] || "",
    storageBackend: placement.storageBackend || choices.backends[0] || "",
    externalNetwork: placement.externalNetwork || choices.externalNetworks[0] || "",
  };
}

export function PlacementFields({
  prefix,
  placement,
  choices,
  onChange,
}: {
  prefix: string;
  placement: Placement;
  choices: ReturnType<typeof usePlacementChoices>;
  onChange: (placement: Placement) => void;
}) {
  const options = (names: string[]) => names.map((name) => ({ value: name, label: name }));
  return (
    <>
      <SearchSelect
        id={`${prefix}-network`}
        label="Network"
        value={placement.network}
        onChange={(network) => onChange({ ...placement, network })}
        choices={options(choices.networks)}
      />
      <SearchSelect
        id={`${prefix}-storage`}
        label="Storage backend"
        value={placement.storageBackend}
        onChange={(storageBackend) => onChange({ ...placement, storageBackend })}
        choices={options(choices.backends)}
      />
      <SearchSelect
        id={`${prefix}-external`}
        label="Floating IPs from"
        value={placement.externalNetwork}
        onChange={(externalNetwork) => onChange({ ...placement, externalNetwork })}
        choices={options(choices.externalNetworks)}
      />
    </>
  );
}

export function NumberField({
  id,
  label,
  value,
  minimum,
  maximum,
  onChange,
  help,
}: {
  id: string;
  label: string;
  value: number;
  minimum: number;
  maximum: number;
  onChange: (value: number) => void;
  help?: string;
}) {
  return (
    <Input
      id={id}
      type="number"
      label={label}
      min={minimum}
      max={maximum}
      value={value}
      help={help}
      onChange={(event) => onChange(Number(event.target.value))}
    />
  );
}
