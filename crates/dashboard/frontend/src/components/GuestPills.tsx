import type { GuestInstance, GuestInventory } from "../cluster/kinds";
import Command from "./Command";

const KIND_LABELS: Record<GuestInstance["kind"], string> = { container: "lxc", "virtual-machine": "vm" };

function tone(status: string): string {
  if (status === "Running") {
    return "guest-pill--running";
  }
  return status === "Stopped" ? "guest-pill--stopped" : "guest-pill--changing";
}

export function guestCount(inventory: GuestInventory | undefined): number {
  return inventory?.specification.lxd.access === "trusted" ? (inventory.specification.lxd.instances ?? []).length : 0;
}

export default function GuestPills({ inventory }: { inventory: GuestInventory }) {
  const lxd = inventory.specification.lxd;
  if (lxd.access === "untrusted") {
    return (
      <div className="guest-pills">
        <p className="u-text--muted">LXD runs here but does not trust Amafu yet. On this VM, run:</p>
        <Command words={["amafu", "lxd", "certificate", ">", "amafu.crt", "&&", "lxc", "config", "trust", "add-certificate", "amafu.crt"]} />
      </div>
    );
  }
  const instances = lxd.instances ?? [];
  if (instances.length === 0) {
    return <p className="guest-pills u-text--muted">LXD on {lxd.server} hosts no containers or VMs.</p>;
  }
  return (
    <ul className="guest-pills">
      {instances.map((instance) => (
        <li key={instance.name} className={`guest-pill ${tone(instance.status)}`} title={instance.location ? `on ${instance.location}` : undefined}>
          <span className="guest-pill__kind">{KIND_LABELS[instance.kind]}</span>
          <span className="guest-pill__name">{instance.name}</span>
          <span className="guest-pill__status">{instance.status}</span>
        </li>
      ))}
    </ul>
  );
}
