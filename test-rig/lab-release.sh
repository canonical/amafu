#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

LAB_HOST="${LAB_HOST:-ubuntu@10.0.10.5}"
LAB_ROOT="${LAB_ROOT:-/home/ubuntu/amafu-lab}"
LAB_STATE_DIR="${LAB_STATE_DIR:-/home/ubuntu/amafu-lab/state}"
LAB_WEB_DIR="${LAB_WEB_DIR:-/home/ubuntu/amafu-images}"
LAB_IMAGE_HOST="${LAB_IMAGE_HOST:-10.20.0.1}"
LAB_IMAGE_PORT="${LAB_IMAGE_PORT:-8090}"
LAB_RELEASE_KEY="${LAB_RELEASE_KEY:-/home/ubuntu/amafu-lab/release.key}"
LAB_ENDPOINTS="${LAB_ENDPOINTS:-10.20.0.11,10.20.0.12,10.20.0.13}"
LAB_NODE_INSTANCES="${LAB_NODE_INSTANCES:-amafu-node-1,amafu-node-2,amafu-node-3}"
LAB_LXD_PROJECT="${LAB_LXD_PROJECT:-amafu}"
LAB_PROJECTS="${LAB_PROJECTS:-default}"
LAB_DASHBOARD_MACHINES="${LAB_DASHBOARD_MACHINES:-amafu-dashboard-1,amafu-dashboard-2}"
LAB_DASHBOARD_VIP="${LAB_DASHBOARD_VIP:-10.20.0.50}"
LAB_FEATURE_LEVEL="${LAB_FEATURE_LEVEL:-1}"
LAB_CANARY_COUNT="${LAB_CANARY_COUNT:-1}"
LAB_SOAK_SECONDS="${LAB_SOAK_SECONDS:-30}"
LAB_HEALTH_DEADLINE_SECONDS="${LAB_HEALTH_DEADLINE_SECONDS:-900}"
LAB_WAIT_SECONDS="${LAB_WAIT_SECONDS:-3500}"
LAB_HANG_SECONDS="${LAB_HANG_SECONDS:-360}"
LAB_POLL_SECONDS="${LAB_POLL_SECONDS:-30}"
LAB_RETRIES="${LAB_RETRIES:-3}"
LAB_KEEP_IMAGES="${LAB_KEEP_IMAGES:-5}"
LAB_BUILD_JOBS="${LAB_BUILD_JOBS:-3}"
LAB_MINIMUM_MEMORY_KIB="${LAB_MINIMUM_MEMORY_KIB:-3145728}"

usage() {
  cat <<'USAGE'
Usage: test-rig/lab-release.sh [options]

Builds a committed revision in a clean worktree, rolls it across the lab
cluster one node at a time, recovers nodes that hang while rebooting, then
upgrades the running dashboard machines to the same build.

Options:
  --ref REF          revision to release (default: HEAD)
  --version VERSION  release version (default: the next YY.MM.N after the lab's newest)
  --host USER@HOST   lab host (default: $LAB_HOST)
  --finalize         finalize the upgrade once every node runs it; the cluster can no longer go back
  --skip-build       reuse the artifacts already built for this revision
  --no-dashboard     leave the dashboard machines alone
  --dry-run          check the lab and print the plan, change nothing
  -h, --help         show this help

Environment overrides: every LAB_* variable at the top of this script.
USAGE
}

REF="HEAD"
VERSION=""
FINALIZE=0
SKIP_BUILD=0
UPGRADE_DASHBOARD=1
DRY_RUN=0

while (($# > 0)); do
  case "$1" in
    --ref) REF="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --host) LAB_HOST="$2"; shift 2 ;;
    --finalize) FINALIZE=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --no-dashboard) UPGRADE_DASHBOARD=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
done

for dependency in git cargo ssh scp jq sha256sum awk; do
  command -v "$dependency" >/dev/null || { echo "Missing dependency: $dependency" >&2; exit 1; }
done

COMMIT="$(git -C "$ROOT" rev-parse --verify "$REF^{commit}")"
SHORT_COMMIT="$(git -C "$ROOT" rev-parse --short "$COMMIT")"
ARTIFACTS="$ROOT/target/lab-release/$SHORT_COMMIT"
mkdir -p "$ARTIFACTS"
LOG="$ARTIFACTS/release-$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$LOG") 2>&1

say() {
  printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2
}

die() {
  say "error: $*"
  exit 1
}

lab() {
  ssh -o ConnectTimeout=10 -o BatchMode=yes "$LAB_HOST" "$@"
}

endpoint_list() {
  local endpoints=()
  local address
  for address in ${LAB_ENDPOINTS//,/ }; do
    endpoints+=("https://$address:7443")
  done
  local IFS=,
  echo "${endpoints[*]}"
}

AMAFU_ENVIRONMENT="export PATH=\"$LAB_ROOT/bin:\$PATH\" \
AMAFU_ENDPOINT='$(endpoint_list)' \
AMAFU_CERTIFICATE_AUTHORITY='$LAB_ROOT/ca.pem' \
AMAFU_TOKEN=\"\$(jq -r .admin_token '$LAB_STATE_DIR/cluster-credentials.json')\";"

amafu_on_lab() {
  lab "$AMAFU_ENVIRONMENT amafu $*"
}

next_version() {
  local prefix newest
  prefix="$(date -u +%y.%m)"
  newest="$(lab "ls '$LAB_ROOT'" | sed -n "s/^manifest-\($prefix\.[0-9]*\)\.json$/\1/p" | sort -V | tail -1)"
  if [[ -z "$newest" ]]; then
    echo "$prefix.1"
  else
    echo "$prefix.$(( ${newest##*.} + 1 ))"
  fi
}

preflight() {
  say "checking the lab on $LAB_HOST"
  local problems=()
  local running
  running="$(amafu_on_lab "--output json operation list" | jq -r '[.[] | select(.status.outcome == null) | .metadata.name] | join(", ")')"
  [[ -z "$running" ]] || problems+=("operations still running: $running")
  local unready
  unready="$(amafu_on_lab "--output json node list" | jq -r '[.[] | select(.status.phase != "ready") | .metadata.name] | join(", ")')"
  [[ -z "$unready" ]] || problems+=("nodes not ready: $unready")
  local local_backends
  local_backends="$(amafu_on_lab "--output json storage-backend list" | jq -c '[.[] | select(.specification.provider.type == "local-disk") | .metadata.name]')"
  local project pinned busy
  for project in ${LAB_PROJECTS//,/ }; do
    pinned="$(amafu_on_lab "--project '$project' --output json volume list" \
      | jq -r --argjson local "$local_backends" '[.[] | select(.status.attached_to != null and (.specification.backend as $b | $local | index($b))) | "\(.status.attached_to) (\(.metadata.name))"] | join(", ")')"
    [[ -z "$pinned" ]] || problems+=("project $project: VMs on local disks cannot be evacuated: $pinned")
    busy="$(amafu_on_lab "--project '$project' --output json juju changes" | jq -r '[.[] | select(.status.outcome == null) | .metadata.name] | join(", ")')"
    [[ -z "$busy" ]] || say "warning: project $project has Juju changes running, which a rollout will disturb: $busy"
    busy="$(amafu_on_lab "--project '$project' --output json kubernetes changes" 2>/dev/null | jq -r '[.[]? | select(.status.outcome == null) | .metadata.name] | join(", ")')"
    [[ -z "$busy" ]] || say "warning: project $project has Kubernetes changes running, which a rollout will disturb: $busy"
  done
  if ((${#problems[@]} > 0)); then
    local problem
    for problem in "${problems[@]}"; do
      say "refusing: $problem"
    done
    exit 1
  fi
  say "the lab is ready for a rollout"
}

guard_memory() {
  local watched="$1"
  while kill -0 "$watched" 2>/dev/null; do
    if (( $(awk '/MemAvailable/ {print $2}' /proc/meminfo) < LAB_MINIMUM_MEMORY_KIB )); then
      say "memory guard: less than $((LAB_MINIMUM_MEMORY_KIB / 1048576)) GiB left, stopping the build to protect the lab"
      pkill -P "$watched" || true
      kill "$watched" || true
      return 1
    fi
    sleep 5
  done
}

build() {
  local tree="$ROOT/target/lab-release/worktree"
  if ((SKIP_BUILD)) && [[ -f "$ARTIFACTS/amafu" && -f "$ARTIFACTS/node-image.tar" ]]; then
    say "reusing the artifacts built for $SHORT_COMMIT"
    return
  fi
  say "building $SHORT_COMMIT in a clean worktree"
  git -C "$ROOT" worktree remove --force "$tree" 2>/dev/null || true
  git -C "$ROOT" worktree add --detach "$tree" "$COMMIT" >/dev/null
  ln -sfn "$ROOT/target" "$tree/target"
  (
    cd "$tree"
    nice -n 19 env CARGO_BUILD_JOBS="$LAB_BUILD_JOBS" image/build release
    nice -n 19 cargo build --release -q -p amafu-command-line -j "$LAB_BUILD_JOBS"
  ) &
  local builder=$!
  guard_memory "$builder" &
  local guard=$!
  local status=0
  wait "$builder" || status=$?
  kill "$guard" 2>/dev/null || true
  ((status == 0)) || die "the build failed"
  cp "$tree/image/output/release/node-image.tar" "$ARTIFACTS/node-image.tar"
  cp "$ROOT/target/release/amafu" "$ARTIFACTS/amafu"
  git -C "$ROOT" worktree remove --force "$tree"
}

publish() {
  say "publishing $SHORT_COMMIT to $LAB_HOST"
  scp -q "$ARTIFACTS/amafu" "$LAB_HOST:$LAB_ROOT/amafu-fixed.new"
  scp -q "$ARTIFACTS/node-image.tar" "$LAB_HOST:$LAB_WEB_DIR/node-image-$SHORT_COMMIT.tar"
  lab "set -e; cd '$LAB_ROOT'; mv amafu-fixed.new amafu-fixed; chmod 0755 amafu-fixed; mkdir -p bin; ln -sfn '$LAB_ROOT/amafu-fixed' bin/amafu"
}

sign() {
  local version="$1"
  lab "set -e; cp '$LAB_WEB_DIR/node-image-$SHORT_COMMIT.tar' '$LAB_WEB_DIR/node-image-$version.tar'; \
    '$LAB_ROOT/bin/amafu' release sign --private-key '$LAB_RELEASE_KEY' \
      --bundle '$LAB_WEB_DIR/node-image-$version.tar' --version '$version' --feature-level '$LAB_FEATURE_LEVEL' \
      --url 'http://$LAB_IMAGE_HOST:$LAB_IMAGE_PORT/node-image-$version.tar' > '$LAB_ROOT/manifest-$version.json'"
  say "signed $SHORT_COMMIT as $version"
}

node_is_up() {
  lab "timeout 4 curl -sk -o /dev/null https://$1:7443/v1/health"
}

watch_rollout() {
  local version="$1"
  local log="$LAB_ROOT/upgrade-$version.log"
  local addresses=(${LAB_ENDPOINTS//,/ })
  local instances=(${LAB_NODE_INSTANCES//,/ })
  local down_since=()
  local last=""
  while lab "pgrep -f '[u]pgrade start --image $LAB_ROOT/manifest-$version.json' >/dev/null"; do
    local line
    line="$(lab "tail -1 '$log' 2>/dev/null" || true)"
    [[ -z "$line" || "$line" == "$last" ]] || { say "$line"; last="$line"; }
    local index
    for index in "${!addresses[@]}"; do
      if node_is_up "${addresses[$index]}"; then
        [[ -z "${down_since[$index]:-}" ]] || say "${instances[$index]} is back"
        down_since[$index]=""
      elif [[ -z "${down_since[$index]:-}" ]]; then
        down_since[$index]="$(date +%s)"
        say "${instances[$index]} went down"
      elif (( $(date +%s) - down_since[index] >= LAB_HANG_SECONDS )); then
        say "${instances[$index]} has been down for $LAB_HANG_SECONDS s: force-restarting it"
        lab "sudo lxc restart --force '${instances[$index]}' --project '$LAB_LXD_PROJECT'"
        down_since[$index]=""
      fi
    done
    sleep "$LAB_POLL_SECONDS"
  done
  lab "tail -1 '$log'"
}

uncordon_all() {
  local cordoned name
  cordoned="$(amafu_on_lab "--output json node list" | jq -r '.[] | select(.specification.is_cordoned) | .metadata.name')"
  for name in $cordoned; do
    say "uncordoning $name, left cordoned by the failed attempt"
    amafu_on_lab "node uncordon '$name'" >/dev/null
  done
}

roll_out() {
  local version="$1"
  local attempt
  for ((attempt = 1; attempt <= LAB_RETRIES + 1; attempt++)); do
    sign "$version"
    say "rolling out $version (attempt $attempt)"
    lab "$AMAFU_ENVIRONMENT cd '$LAB_ROOT'; nohup amafu upgrade start --image '$LAB_ROOT/manifest-$version.json' \
      --canary-count '$LAB_CANARY_COUNT' --soak-seconds '$LAB_SOAK_SECONDS' \
      --health-deadline-seconds '$LAB_HEALTH_DEADLINE_SECONDS' --wait '$LAB_WAIT_SECONDS' \
      > '$LAB_ROOT/upgrade-$version.log' 2>&1 < /dev/null &"
    local outcome
    outcome="$(watch_rollout "$version")"
    say "outcome: $outcome"
    if [[ "$outcome" == *succeeded* ]]; then
      RELEASED_VERSION="$version"
      return 0
    fi
    uncordon_all
    if [[ "$outcome" != *"could not boot"* ]]; then
      die "the rollout of $version failed for a reason a retry does not fix"
    fi
    version="$(next_version)"
    say "a node fell back to its previous image; the same image is re-signed as $version, since a version that failed to boot is not retried"
  done
  die "the rollout still failed after $LAB_RETRIES retries"
}

upgrade_dashboards() {
  local machine state
  for machine in ${LAB_DASHBOARD_MACHINES//,/ }; do
    state="$(lab "sudo lxc list '$machine' --project '$LAB_LXD_PROJECT' -f csv -c s")"
    if [[ "$state" != RUNNING ]]; then
      say "skipping $machine: it is ${state:-missing}"
      continue
    fi
    say "upgrading the dashboard on $machine"
    lab "set -e; sudo lxc file push -q '$LAB_ROOT/amafu-fixed' '$machine/usr/local/bin/amafu.new' --project '$LAB_LXD_PROJECT' --mode 0755; \
      sudo lxc exec '$machine' --project '$LAB_LXD_PROJECT' -- sh -c 'cd /usr/local/bin && mv amafu amafu.previous && mv amafu.new amafu && systemctl restart amafu-dashboard && sleep 2 && systemctl is-active --quiet amafu-dashboard'" \
      || die "the dashboard on $machine did not come back; amafu.previous is still there"
  done
  local answer=""
  local attempt
  for attempt in 1 2 3 4 5; do
    answer="$(lab "timeout 5 curl -sk -o /dev/null -w '%{http_code}' https://$LAB_DASHBOARD_VIP/" || true)"
    [[ "$answer" == 200 ]] && break
    sleep 3
  done
  [[ "$answer" == 200 ]] || die "the dashboard address $LAB_DASHBOARD_VIP does not answer"
  say "the dashboard answers on $LAB_DASHBOARD_VIP"
}

prune_images() {
  lab "cd '$LAB_WEB_DIR' && ls -t node-image-*.tar | tail -n +$((LAB_KEEP_IMAGES + 1)) | xargs -r rm -f"
}

[[ -n "$VERSION" ]] || VERSION="$(next_version)"
preflight
say "plan: release $SHORT_COMMIT as $VERSION, canary $LAB_CANARY_COUNT, soak ${LAB_SOAK_SECONDS}s, \
$( ((FINALIZE)) && echo finalize || echo "no finalize" ), $( ((UPGRADE_DASHBOARD)) && echo "then the dashboards" || echo "dashboards untouched" )"
if ((DRY_RUN)); then
  say "dry run: nothing changed"
  exit 0
fi

build
publish
RELEASED_VERSION=""
roll_out "$VERSION"
if ((FINALIZE)); then
  say "finalizing $RELEASED_VERSION"
  amafu_on_lab "upgrade finalize"
fi
if ((UPGRADE_DASHBOARD)); then
  upgrade_dashboards
fi
prune_images
amafu_on_lab "upgrade status"
say "done: $SHORT_COMMIT runs on the lab as $RELEASED_VERSION; log in $LOG"
