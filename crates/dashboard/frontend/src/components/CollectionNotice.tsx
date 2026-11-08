import { Notification, Spinner } from "@canonical/react-components";

import type { CollectionState } from "../cluster/live";

interface CollectionNoticeProps {
  state: CollectionState;
  noun: string;
}

export default function CollectionNotice({ state, noun }: CollectionNoticeProps) {
  if (state === "loading") {
    return <Spinner text={`Loading ${noun}…`} />;
  }
  if (state === "forbidden") {
    return (
      <Notification severity="caution" title="Not visible to you">
        Your role may not read {noun}.
      </Notification>
    );
  }
  if (state === "reconnecting") {
    return (
      <Notification severity="information" title="Reconnecting">
        The connection to the cluster dropped; these {noun} may be out of date until it is back.
      </Notification>
    );
  }
  return null;
}
