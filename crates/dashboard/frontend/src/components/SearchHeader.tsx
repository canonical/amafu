import { SearchBox } from "@canonical/react-components";

interface SearchHeaderProps {
  noun: string;
  value: string;
  onChange: (value: string) => void;
}

export default function SearchHeader({ noun, value, onChange }: SearchHeaderProps) {
  return (
    <SearchBox
      className="u-no-margin--bottom page-search"
      placeholder={`Search ${noun}`}
      externallyControlled
      value={value}
      onChange={onChange}
      onClear={() => onChange("")}
    />
  );
}

export function matches(query: string, ...fields: (string | undefined | null)[]): boolean {
  const wanted = query.trim().toLowerCase();
  return wanted === "" || fields.some((field) => field?.toLowerCase().includes(wanted));
}
