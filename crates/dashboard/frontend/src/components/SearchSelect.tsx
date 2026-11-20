import { CustomSelect } from "@canonical/react-components";
import type { CustomSelectOption } from "@canonical/react-components";
import type { ReactNode } from "react";

export interface Choice {
  value: string;
  label: string;
  detail?: string;
  group?: string;
}

const GROUP_PREFIX = "group:";

function optionOf(choice: Choice): CustomSelectOption {
  return {
    value: choice.value,
    text: [choice.label, choice.detail, choice.group].filter(Boolean).join(" "),
    label: (
      <span className="choice">
        <span className="choice__label">{choice.label}</span>
        {choice.detail && <span className="choice__detail">{choice.detail}</span>}
      </span>
    ),
    selectedLabel: <span className="choice__label">{choice.label}</span>,
  };
}

export function optionsOf(choices: Choice[]): CustomSelectOption[] {
  const groups = [...new Set(choices.map((choice) => choice.group ?? ""))];
  if (groups.length < 2) {
    return choices.map(optionOf);
  }
  return groups.flatMap((group) => [
    { value: `${GROUP_PREFIX}${group}`, text: "", disabled: true, label: <span className="choice__group">{group}</span> },
    ...choices.filter((choice) => (choice.group ?? "") === group).map(optionOf),
  ]);
}

interface SearchSelectProps {
  id: string;
  label: ReactNode;
  value: string;
  choices: Choice[];
  onChange: (value: string) => void;
  placeholder?: string;
  help?: ReactNode;
  error?: ReactNode;
  required?: boolean;
  disabled?: boolean;
}

export default function SearchSelect({ id, label, value, choices, onChange, placeholder, help, error, required, disabled }: SearchSelectProps) {
  return (
    <CustomSelect
      id={id}
      name={id}
      label={label}
      value={value}
      options={optionsOf(choices)}
      onChange={(chosen) => {
        if (!chosen.startsWith(GROUP_PREFIX)) {
          onChange(chosen);
        }
      }}
      searchable="auto"
      searchPlaceholder="Search"
      defaultToggleLabel={placeholder ?? "Choose…"}
      help={help}
      error={error}
      required={required}
      disabled={disabled || choices.length === 0}
    />
  );
}
