import { Chip } from "@canonical/react-components";
import type { ReactNode } from "react";

import SearchSelect from "./SearchSelect";
import type { Choice } from "./SearchSelect";

interface ChipSelectProps {
  id: string;
  label: ReactNode;
  choices: Choice[];
  selected: string[];
  onChange: (selected: string[]) => void;
  placeholder?: string;
  help?: ReactNode;
  emptyMessage?: string;
}

export default function ChipSelect({ id, label, choices, selected, onChange, placeholder, help, emptyMessage }: ChipSelectProps) {
  const remaining = choices.filter((choice) => !selected.includes(choice.value));
  const labelOf = (value: string) => choices.find((choice) => choice.value === value)?.label ?? value;
  return (
    <div className="chip-select">
      <SearchSelect
        id={id}
        label={label}
        value=""
        choices={remaining}
        onChange={(value) => onChange([...selected, value])}
        placeholder={choices.length === 0 ? (emptyMessage ?? "Nothing to choose from") : selected.length > 0 ? "Add another…" : (placeholder ?? "Choose…")}
        help={help}
      />
      {selected.length > 0 && (
        <div className="chip-select__chips" aria-label={`Chosen ${typeof label === "string" ? label.toLowerCase() : "items"}`}>
          {selected.map((value) => (
            <Chip key={value} value={labelOf(value)} onDismiss={() => onChange(selected.filter((chosen) => chosen !== value))} />
          ))}
        </div>
      )}
    </div>
  );
}
