import { Tabs } from "@canonical/react-components";
import type { ReactNode } from "react";
import { useState } from "react";

export interface PanelTab {
  label: string;
  content: ReactNode;
}

export default function PanelTabs({ tabs }: { tabs: PanelTab[] }) {
  const [current, setCurrent] = useState(tabs[0]?.label ?? "");
  const shown = tabs.find((tab) => tab.label === current) ?? tabs[0];
  return (
    <>
      <Tabs
        className="panel-tabs"
        links={tabs.map((tab) => ({ label: tab.label, active: tab.label === shown?.label, onClick: () => setCurrent(tab.label) }))}
      />
      <div className="panel-tabs__content">{shown?.content}</div>
    </>
  );
}
