//! Finds Denon/Marantz receivers by their HEOS mDNS advertisement.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, bail};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::{info, warn};

const SERVICE_TYPE: &str = "_heos-audio._tcp.local.";

/// Whether an advertisement matches the selector: the HEOS device ID (`did` TXT record), or
/// the friendly name as the first label of the instance or host name. Case-insensitive.
pub fn matches(selector: &str, did: Option<&str>, fullname: &str, hostname: &str) -> bool {
    let first_label = |name: &str| name.split('.').next().unwrap_or_default().to_owned();
    did.is_some_and(|did| did.eq_ignore_ascii_case(selector))
        || first_label(fullname).eq_ignore_ascii_case(selector)
        || first_label(hostname).eq_ignore_ascii_case(selector)
}

/// Returns the IPv4 address of the first receiver that answers and matches `selector`.
pub async fn find(selector: Option<&str>, timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    let daemon = ServiceDaemon::new().context("starting mDNS")?;
    let result = tokio::time::timeout(timeout, browse(&daemon, selector)).await;
    if let Err(e) = daemon.shutdown() {
        warn!("stopping mDNS: {e}");
    }
    result.context("no matching receiver answered over mDNS")?
}

async fn browse(daemon: &ServiceDaemon, selector: Option<&str>) -> anyhow::Result<Ipv4Addr> {
    let events = daemon.browse(SERVICE_TYPE).context("browsing mDNS")?;
    while let Ok(event) = events.recv_async().await {
        let ServiceEvent::ServiceResolved(service) = event else {
            continue;
        };
        let did = service.get_property_val_str("did");
        if let Some(selector) = selector
            && !matches(
                selector,
                did,
                service.get_fullname(),
                service.get_hostname(),
            )
        {
            continue;
        }
        if let Some(ip) = service.get_addresses_v4().into_iter().next() {
            info!(name = service.get_fullname(), %ip, "found receiver");
            return Ok(ip);
        }
    }
    bail!("mDNS browse ended")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DID: &str = "3C424E4F0005CDFEC712";
    const FULLNAME: &str = "HTAVR._heos-audio._tcp.local.";
    const HOSTNAME: &str = "HTAVR.local.";

    #[test]
    fn matches_device_id() {
        assert!(matches(DID, Some(DID), FULLNAME, HOSTNAME));
        assert!(matches(&DID.to_lowercase(), Some(DID), FULLNAME, HOSTNAME));
    }

    #[test]
    fn matches_friendly_name() {
        assert!(matches("HTAVR", Some(DID), FULLNAME, HOSTNAME));
        assert!(matches("htavr", None, FULLNAME, HOSTNAME));
    }

    #[test]
    fn rejects_other_receivers() {
        assert!(!matches("Den", Some(DID), FULLNAME, HOSTNAME));
        assert!(!matches("0005CDFEC712", Some(DID), FULLNAME, HOSTNAME));
    }
}
