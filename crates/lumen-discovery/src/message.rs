//! SSDP message parsing and building -- pure, socket-free, and total: a malformed or truncated
//! datagram from anything on the LAN is `None`, never a panic, the same "unknown is not fatal"
//! posture every other parser in this workspace already takes for hostile or corrupt input.
//!
//! SSDP's own request/response lines are HTTP-shaped text carried over UDP rather than TCP -- no
//! connection, no body, just a request or status line followed by `Header: value` lines and a blank
//! line -- so this reuses that same well-known shape rather than inventing a bespoke grammar.

use std::collections::HashMap;

/// An `M-SEARCH` request, as a search target and how long (seconds) the searcher said it will wait
/// for responses to trickle in before giving up -- callers may use this to jitter their reply rather
/// than answering every request instantly, though [`crate::Responder`] does not do so today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MSearchRequest {
    pub search_target: String,
    pub max_delay_secs: u8,
}

/// One (notification-type, unique-service-name) pair a device announces itself under. A real UPnP
/// root device announces several of these at once -- `upnp:rootdevice`, its own bare `uuid:...`, and
/// one per device/service type it implements -- which is why this is a value callers collect into a
/// list rather than a single fixed identity `Responder` hardcodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// The `NT` (`NOTIFY`) / `ST`-matching value, e.g. `upnp:rootdevice` or
    /// `urn:schemas-upnp-org:device:MediaServer:1`.
    pub notification_type: String,
    /// The `USN` value: almost always `uuid:<device-uuid>::<notification_type>`, except for the bare
    /// `uuid:<device-uuid>` announcement, where it is just the UUID with no `::` suffix.
    pub unique_service_name: String,
}

/// `SERVER` header value SSDP convention expects in the `OS/version UPnP/1.0 product/version` shape.
/// The OS/version segment is a stand-in -- detecting a real one needs a new dependency for a header
/// no real client's behaviour depends on -- but the overall shape matches what the spec describes.
pub fn server_header() -> String {
    format!("lumen/1.0 UPnP/1.0 lumen-serve/{}", env!("CARGO_PKG_VERSION"))
}

/// Parse an `M-SEARCH * HTTP/1.1` request datagram. `None` for anything that is not a well-formed
/// SSDP search -- including a `NOTIFY` announcement from some *other* device on the multicast group,
/// which arrives on the exact same socket and must be silently ignored rather than misread as a
/// request to answer.
pub fn parse_msearch(datagram: &[u8]) -> Option<MSearchRequest> {
    let text = std::str::from_utf8(datagram).ok()?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    if !request_line.starts_with("M-SEARCH") {
        return None;
    }

    let headers = parse_headers(lines);
    let man = headers.get("man")?;
    // MAN must be exactly the quoted token `"ssdp:discover"` per the spec; anything else (or a
    // missing header) is not a discovery request this responder answers.
    if man.trim_matches('"') != "ssdp:discover" {
        return None;
    }
    let search_target = headers.get("st")?.clone();
    // MX is advisory (how long the searcher is willing to wait) and not always present or numeric on
    // a real network; default to a generous handful of seconds rather than refusing the request over
    // a header that carries no information this responder actually needs to act correctly.
    let max_delay_secs = headers.get("mx").and_then(|v| v.parse().ok()).unwrap_or(5);
    Some(MSearchRequest { search_target, max_delay_secs })
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    headers
}

/// Does an announced `NT` satisfy a searcher's `ST`? `ssdp:all` matches every announcement (a
/// "what's out there at all" search); everything else must match exactly -- SSDP defines no partial
/// or prefix matching, and guessing at a looser rule would answer searches this device was never
/// actually asked to.
pub fn matches_search_target(notification_type: &str, search_target: &str) -> bool {
    search_target == "ssdp:all" || search_target == notification_type
}

/// The unicast reply to one matching `M-SEARCH`. `max_age_secs` becomes `CACHE-CONTROL: max-age=`,
/// how long the searcher may treat this answer as valid without re-asking.
pub fn build_search_response(a: &Announcement, location: &str, max_age_secs: u32) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age={max_age_secs}\r\n\
         EXT:\r\n\
         LOCATION: {location}\r\n\
         SERVER: {}\r\n\
         ST: {}\r\n\
         USN: {}\r\n\r\n",
        server_header(),
        a.notification_type,
        a.unique_service_name,
    )
}

/// An unsolicited `ssdp:alive` announcement, multicast periodically and once on startup so a control
/// point does not have to have been listening at the exact moment this device joined the network.
pub fn build_notify_alive(a: &Announcement, location: &str, max_age_secs: u32) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: {}\r\n\
         CACHE-CONTROL: max-age={max_age_secs}\r\n\
         LOCATION: {location}\r\n\
         NT: {}\r\n\
         NTS: ssdp:alive\r\n\
         SERVER: {}\r\n\
         USN: {}\r\n\r\n",
        crate::SSDP_MULTICAST_ADDR,
        a.notification_type,
        server_header(),
        a.unique_service_name,
    )
}

/// The withdrawal announcement a well-behaved device multicasts on a clean shutdown, so control
/// points drop it immediately rather than waiting out `max-age` on a stale, no-longer-valid entry.
pub fn build_notify_byebye(a: &Announcement) -> String {
    format!(
        "NOTIFY * HTTP/1.1\r\n\
         HOST: {}\r\n\
         NT: {}\r\n\
         NTS: ssdp:byebye\r\n\
         USN: {}\r\n\r\n",
        crate::SSDP_MULTICAST_ADDR,
        a.notification_type,
        a.unique_service_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement() -> Announcement {
        Announcement {
            notification_type: "upnp:rootdevice".into(),
            unique_service_name: "uuid:1234::upnp:rootdevice".into(),
        }
    }

    #[test]
    fn a_well_formed_msearch_is_parsed() {
        let datagram = b"M-SEARCH * HTTP/1.1\r\n\
                          HOST: 239.255.255.250:1900\r\n\
                          MAN: \"ssdp:discover\"\r\n\
                          MX: 3\r\n\
                          ST: ssdp:all\r\n\r\n";
        let req = parse_msearch(datagram).unwrap();
        assert_eq!(req.search_target, "ssdp:all");
        assert_eq!(req.max_delay_secs, 3);
    }

    #[test]
    fn missing_mx_defaults_rather_than_being_refused() {
        let datagram =
            b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\nST: upnp:rootdevice\r\n\r\n";
        let req = parse_msearch(datagram).unwrap();
        assert_eq!(req.max_delay_secs, 5, "MX is advisory; absence must not refuse the request");
    }

    #[test]
    fn a_notify_from_another_device_on_the_same_multicast_group_is_ignored() {
        // NOTIFY announcements from other SSDP participants land on the exact same socket this
        // responder listens on; they must never be misread as a search to answer.
        let datagram =
            b"NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNTS: ssdp:alive\r\n\r\n";
        assert_eq!(parse_msearch(datagram), None);
    }

    #[test]
    fn a_missing_man_or_wrong_value_is_refused() {
        let no_man = b"M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n";
        assert_eq!(parse_msearch(no_man), None);
        let wrong_man = b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:byebye\"\r\nST: ssdp:all\r\n\r\n";
        assert_eq!(parse_msearch(wrong_man), None);
    }

    #[test]
    fn a_missing_search_target_is_refused() {
        let datagram = b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\n\r\n";
        assert_eq!(parse_msearch(datagram), None);
    }

    #[test]
    fn malformed_or_non_utf8_input_is_none_not_a_panic() {
        assert_eq!(parse_msearch(b""), None);
        assert_eq!(parse_msearch(b"not ssdp at all"), None);
        assert_eq!(parse_msearch(&[0xFF, 0xFE, 0x00, 0x01]), None);
    }

    #[test]
    fn ssdp_all_matches_every_announcement_but_a_specific_target_only_matches_itself() {
        assert!(matches_search_target("upnp:rootdevice", "ssdp:all"));
        assert!(matches_search_target("urn:schemas-upnp-org:device:MediaServer:1", "ssdp:all"));
        assert!(matches_search_target("upnp:rootdevice", "upnp:rootdevice"));
        assert!(!matches_search_target(
            "upnp:rootdevice",
            "urn:schemas-upnp-org:device:MediaServer:1"
        ));
    }

    #[test]
    fn a_search_response_names_the_announcement_and_location() {
        let msg = build_search_response(&announcement(), "http://192.168.1.5:8200/desc.xml", 1800);
        assert!(msg.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(msg.contains("LOCATION: http://192.168.1.5:8200/desc.xml"));
        assert!(msg.contains("ST: upnp:rootdevice"));
        assert!(msg.contains("USN: uuid:1234::upnp:rootdevice"));
        assert!(msg.contains("CACHE-CONTROL: max-age=1800"));
        assert!(msg.ends_with("\r\n\r\n"), "must end with the blank line terminating headers");
    }

    #[test]
    fn a_notify_alive_names_the_multicast_host_and_nts() {
        let msg = build_notify_alive(&announcement(), "http://192.168.1.5:8200/desc.xml", 1800);
        assert!(msg.starts_with("NOTIFY * HTTP/1.1\r\n"));
        assert!(msg.contains("HOST: 239.255.255.250:1900"));
        assert!(msg.contains("NTS: ssdp:alive"));
        assert!(msg.contains("NT: upnp:rootdevice"));
    }

    #[test]
    fn a_notify_byebye_carries_no_stale_location_or_cache_control() {
        let msg = build_notify_byebye(&announcement());
        assert!(msg.contains("NTS: ssdp:byebye"));
        assert!(!msg.contains("LOCATION"), "a withdrawal has nothing left to locate");
        assert!(!msg.contains("CACHE-CONTROL"), "nothing is being cached anymore");
    }
}
