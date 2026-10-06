//! The Bonjour tag Macs of one vault find each other by, new each UTC day (OFE-83), and the advert
//! that carries it (local.rs).
use super::*;

/// Seconds since the Unix epoch: the clock `start` advertises by.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The UTC day `secs` falls in.
pub(super) fn day(secs: u64) -> u64 {
    secs / 86_400
}

/// Advertised so Macs of one vault find each other. Keyed with the LAN key, so neither the relay
/// (which knows the vault id and token) nor anyone else can compute it or link it to a vault, and new
/// each UTC day, so whoever records Bonjour on one network can't recognize the same Macs on another
/// network on another day (OFE-83).
pub fn tag(lan: &Key, day: u64) -> String {
    hex(&hmac(lan, &[b"ozen-sync lan tag", &day.to_be_bytes()])
        .finalize()
        .into_bytes()[..8])
}

/// Whether `theirs` is this vault's tag at `secs`: yesterday's, today's or tomorrow's, so two Macs on
/// either side of midnight, or with clocks up to a day apart, still find each other.
pub(super) fn ours(lan: &Key, theirs: &str, secs: u64) -> bool {
    let d = day(secs);
    [d.saturating_sub(1), d, d + 1]
        .iter()
        .any(|&x| tag(lan, x) == theirs)
}

/// What this Mac advertises on `day`: its tag then and its instance id.
pub(super) fn advert(
    lan: &Key,
    id: &str,
    port: u16,
    day: u64,
) -> Result<ServiceInfo, mdns_sd::Error> {
    let tag = tag(lan, day);
    let props = [("tag", tag.as_str()), ("id", id)];
    Ok(ServiceInfo::new(
        &service_type(),
        id,
        &format!("{id}.local."),
        "",
        port,
        &props[..],
    )?
    .enable_addr_auto())
}
