import { ApplicationLayout, Button, Icon, Spinner } from "@canonical/react-components";
import { NavLink, Route, Routes } from "react-router";

import Containers from "./pages/Containers";
import FloatingIps from "./pages/FloatingIps";
import Images from "./pages/Images";
import Networks from "./pages/Networks";
import Nodes from "./pages/Nodes";
import Operations from "./pages/Operations";
import Overview from "./pages/Overview";
import SecurityGroups from "./pages/SecurityGroups";
import Settings from "./pages/Settings";
import SignIn from "./pages/SignIn";
import Storage from "./pages/Storage";
import VirtualMachines from "./pages/VirtualMachines";
import Volumes from "./pages/Volumes";
import { useSession } from "./session";
import { useSystemTheme } from "./theme";

const PAGES = [
  { path: "/", label: "Overview", icon: "home", page: <Overview /> },
  { path: "/virtual-machines", label: "Virtual machines", icon: "machines", page: <VirtualMachines /> },
  { path: "/containers", label: "Containers", icon: "containers", page: <Containers /> },
  { path: "/images", label: "Images", icon: "image", page: <Images /> },
  { path: "/storage", label: "Storage", icon: "storage-pool", page: <Storage /> },
  { path: "/volumes", label: "Volumes", icon: "storage-volume", page: <Volumes /> },
  { path: "/networks", label: "Networks", icon: "connected", page: <Networks /> },
  { path: "/floating-ips", label: "Floating IPs", icon: "exposed", page: <FloatingIps /> },
  { path: "/security-groups", label: "Security groups", icon: "security", page: <SecurityGroups /> },
  { path: "/nodes", label: "Nodes", icon: "cluster-host", page: <Nodes /> },
  { path: "/operations", label: "Operations", icon: "history", page: <Operations /> },
];

const SETTINGS = { path: "/settings", label: "Settings", icon: "settings", page: <Settings /> };

function SessionFooter() {
  const session = useSession();
  const project = session.phase === "signed-in" ? session.project : null;
  return (
    <div className="session-footer">
      <p className="p-text--small-caps session-footer__label">Project</p>
      <p className="session-footer__project">{project ?? "your token's project"}</p>
      <Button appearance="base" className="is-dark session-footer__sign-out" hasIcon onClick={() => void session.signOut()}>
        <Icon name="log-out" light />
        <span>Sign out</span>
      </Button>
    </div>
  );
}

export default function Application() {
  useSystemTheme();
  const session = useSession();
  if (session.phase === "checking") {
    return <Spinner className="u-align--center" text="Connecting…" />;
  }
  if (session.phase === "signed-out") {
    return <SignIn />;
  }
  const link = ({ path, label, icon }: { path: string; label: string; icon: string }) => ({
    component: NavLink,
    to: path,
    end: path === "/",
    label,
    icon,
  });
  return (
    <ApplicationLayout
      id="l-application"
      dark
      logo={
        <NavLink className="p-panel__logo" to="/">
          <img className="p-panel__logo-icon" src="/logo.svg" alt="" width={24} height={24} />
          <span className="p-panel__logo-name is-fading-when-collapsed">Amafu</span>
        </NavLink>
      }
      navItems={[
        { items: PAGES.map(link) },
        { items: [link(SETTINGS)] },
        { items: [<SessionFooter key="session" />] },
      ]}
    >
      <Routes>
        {[...PAGES, SETTINGS].map(({ path, page }) => (
          <Route key={path} path={path} element={page} />
        ))}
        <Route path="*" element={<Overview />} />
      </Routes>
    </ApplicationLayout>
  );
}
