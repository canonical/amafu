import { Button, Icon } from "@canonical/react-components";

interface IconActionProps {
  icon: string;
  label: string;
  onClick: () => void;
  isDisabled?: boolean;
}

export default function IconAction({ icon, label, onClick, isDisabled = false }: IconActionProps) {
  return (
    <Button
      type="button"
      appearance="base"
      dense
      hasIcon
      className="u-no-margin--bottom icon-action"
      title={label}
      aria-label={label}
      disabled={isDisabled}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      <Icon name={icon} />
    </Button>
  );
}
