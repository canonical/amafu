import { Button, Icon, Notification, SidePanel } from "@canonical/react-components";
import type { FormEvent, ReactNode } from "react";

interface PanelProps {
  title: ReactNode;
  onClose: () => void;
  children: ReactNode;
  problem?: string | null;
  footer?: ReactNode;
  onSubmit?: () => void;
  isWide?: boolean;
}

export default function Panel({ title, onClose, children, problem, footer, onSubmit, isWide = false }: PanelProps) {
  const submitted = (event: FormEvent) => {
    event.preventDefault();
    onSubmit?.();
  };
  const body = (
    <>
      <SidePanel.Header>
        <SidePanel.HeaderTitle>{title}</SidePanel.HeaderTitle>
        <SidePanel.HeaderControls>
          <Button appearance="base" hasIcon onClick={onClose} aria-label="Close" type="button">
            <Icon name="close" />
          </Button>
        </SidePanel.HeaderControls>
      </SidePanel.Header>
      <SidePanel.Content>
        {problem && (
          <Notification severity="negative" title="The cluster refused">
            {problem}
          </Notification>
        )}
        {children}
      </SidePanel.Content>
      {footer && <SidePanel.Footer className="panel-footer">{footer}</SidePanel.Footer>}
    </>
  );
  return (
    <SidePanel overlay width={isWide ? "wide" : "narrow"} className="amafu-panel">
      {onSubmit ? (
        <form onSubmit={submitted} className="amafu-panel__form">
          {body}
        </form>
      ) : (
        body
      )}
    </SidePanel>
  );
}

interface SubmitFooterProps {
  label: string;
  isBusy: boolean;
  onCancel: () => void;
}

export function SubmitFooter({ label, isBusy, onCancel }: SubmitFooterProps) {
  return (
    <>
      <Button type="button" onClick={onCancel}>
        Cancel
      </Button>
      <Button type="submit" appearance="positive" disabled={isBusy}>
        {label}
      </Button>
    </>
  );
}
