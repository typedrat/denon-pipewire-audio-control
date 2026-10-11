//! Finds Denon/Marantz receivers by their HEOS mDNS advertisement.

use std::net::Ipv4Addr;
use std::time::Duration;

pub async fn find(_selector: Option<&str>, _timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    anyhow::bail!("discovery is not implemented")
}
