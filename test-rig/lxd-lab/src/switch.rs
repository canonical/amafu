use std::net::Ipv4Addr;

use anyhow::bail;

use crate::lxd::LxdClient;

pub const SWITCH_INSTANCE: &str = "lab-switch";

pub async fn probe_http(lxd: &LxdClient, address: Ipv4Addr, port: u16) -> anyhow::Result<String> {
    let output =
        lxd.execute(SWITCH_INSTANCE, &format!("wget -q -T 3 -O - http://{address}:{port}/ || true")).await?;
    if output.exit_code != 0 {
        bail!("probing {address}:{port} from the lab switch failed: {}", output.standard_error);
    }
    Ok(output.standard_output)
}
