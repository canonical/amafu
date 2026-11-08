import { ConfirmationModal, Notification } from "@canonical/react-components";

import { remove } from "../cluster/client";
import type { Scope } from "../cluster/client";
import { useSubmission } from "./submission";

interface DeleteConfirmationProps {
  scope: Scope;
  name: string;
  noun: string;
  consequence?: string;
  onClose: () => void;
}

export default function DeleteConfirmation({ scope, name, noun, consequence, onClose }: DeleteConfirmationProps) {
  const { isBusy, problem, submit } = useSubmission();
  return (
    <ConfirmationModal
      title={`Delete ${noun} ${name}?`}
      confirmButtonLabel="Delete"
      confirmButtonAppearance="negative"
      confirmButtonLoading={isBusy}
      onConfirm={() => void submit(() => remove(scope, name), onClose)}
      close={onClose}
    >
      {problem && (
        <Notification severity="negative" title="The cluster refused">
          {problem}
        </Notification>
      )}
      <p>{consequence ?? "This cannot be undone."}</p>
    </ConfirmationModal>
  );
}
