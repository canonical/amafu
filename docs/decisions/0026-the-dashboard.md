# 0026: A stateless dashboard that forwards each user's token, in React and Vanilla

Status: accepted for milestone 6

## Decision

- **The dashboard is a separate process on management machines**, not a role of the nodes. Two or more instances
  share a virtual address through keepalived; `amafu dashboard enable` deploys them from MAAS.
- **It holds no state.** A session is the user's token and project sealed with AES-256-GCM under a key every
  instance shares, kept in an `HttpOnly`, `Secure`, `SameSite=Strict` cookie. Any instance can open any session,
  so losing an instance loses nothing.
- **It has no authority of its own.** Every call is forwarded to a core member with the signed-in user's token, so
  the dashboard can do only what the user may, and every action is audited under the user's name. Changes must
  carry a header only the dashboard's own script sends.
- **Live tables come from one event stream per browser.** The API watches a whole collection (`?watch=true` on a
  list, in one project or every project); the dashboard opens one upstream watch per collection a page shows and
  merges them into a single server-sent event stream, so a page never needs more than one long-lived connection.
  A collection the user may not read is reported as refused, and the page says so.
- **The interface is React with Canonical's Vanilla Framework and its React components**, built with Vite. The
  Ubuntu fonts are bundled, so the dashboard works without internet access. The built bundle is committed and
  embedded in the binary, so building Amafu never needs Node.
- **Every form shows the equivalent `amafu` command** as it is filled in, and the dashboard follows the system's
  light or dark theme.
- **It covers every kind the API has:** VMs, containers, images, storage backends and plugins, volumes, networks and
  external networks, floating IPs, security groups, nodes, operations, the cluster version and audit sinks.
  Migrations and evacuations are started as operations, built as the command line builds them.

## Consequences

- The dashboard cannot know the user's role in advance; actions the user may not take are refused by the cluster and
  explained, rather than hidden.
- Upgrades still start from the command line, where the signed image manifest is read.
- The bundle is about 180 KiB of script and 80 KiB of styles compressed; it is served uncompressed for now.
