//! SSDP (Simple Service Discovery Protocol) -- how a UPnP/DLNA device announces itself on a LAN and
//! answers a control point's search, so something like a smart TV's "media servers" list can find
//! `lumen serve` without being told its address.
//!
//! **Stage 0.** This crate is the discovery protocol machinery alone: message parsing/building
//! ([`message`]) and a multicast [`Responder`]. It does not yet declare `lumen serve` a UPnP
//! `MediaServer` or serve a device-description document -- doing so honestly requires a working
//! `ContentDirectory` SOAP service behind it (a real UPnP client that finds a `MediaServer` expects
//! to be able to browse it), which is the next stage. Advertising the device type before that service
//! exists would be a real bug: a client would find this server, try to browse it, and get nothing.
//! What this stage *does* provide is the generic, reusable, protocol-correct half -- any caller
//! supplies its own list of [`Announcement`]s once it has something real to announce.
//!
//! **Deliberately unauthenticated, and deliberately not on `lumen serve`'s existing TLS listener.**
//! SSDP and the DLNA `ContentDirectory`/`AVTransport` services it is meant to advertise are
//! unauthenticated by protocol design -- any device on the LAN must be able to discover and browse a
//! `MediaServer` with no handshake at all, which is structurally incompatible with `lumen serve`'s
//! existing pairing-code-plus-pinned-TLS security model. This is why discovery lives in its own
//! crate rather than folding into `lumen-play`'s remote module: it is a genuinely different, weaker
//! trust posture, and conflating the two would either weaken the paired control channel or falsely
//! imply DLNA discovery carries the same protection pairing does. A `lumen serve` operator opts into
//! this surface explicitly; it is never on by default.

#![forbid(unsafe_code)]

mod content_directory;
mod descriptor;
mod message;

pub use content_directory::{
    BrowseFlag, BrowseRequest, DidlObject, DidlResource, ObjectClass, build_browse_response,
    build_didl_lite, build_soap_fault, parse_browse_request,
};
pub use descriptor::{
    DeviceIdentity, build_device_description, build_get_current_connection_ids_response,
    build_get_protocol_info_response, connection_manager_scpd, content_directory_scpd,
};
pub use message::{
    Announcement, MSearchRequest, build_notify_alive, build_notify_byebye, build_search_response,
    matches_search_target, parse_msearch, server_header,
};

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

/// The standard SSDP multicast group and port every UPnP participant on a LAN listens on.
pub const SSDP_MULTICAST_ADDR: &str = "239.255.255.250:1900";
const SSDP_MULTICAST_IP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;

/// How long a control point may cache one of this responder's announcements before treating it as
/// stale. SSDP convention is 30 minutes; fixed here rather than left to the caller so every
/// announcement this responder issues -- search replies and `NOTIFY`s alike -- stays internally
/// consistent with the interval `run` actually re-announces on.
pub const DEFAULT_MAX_AGE_SECS: u32 = 1800;

/// Owns the SSDP multicast socket and answers `M-SEARCH` requests for whatever [`Announcement`]s it
/// was given, all pointing at one `location` (the device-description document's URL).
#[derive(Debug)]
pub struct Responder {
    socket: UdpSocket,
    announcements: Vec<Announcement>,
    location: String,
}

impl Responder {
    /// Bind the SSDP port and join the multicast group. `SO_REUSEADDR` (and, on Unix, `SO_REUSEPORT`)
    /// is set before bind so this can coexist with another SSDP participant already on the machine --
    /// see the `Cargo.toml` comment on why that needs `socket2` rather than plain `std`.
    pub fn bind(location: String, announcements: Vec<Announcement>) -> std::io::Result<Self> {
        Self::bind_on(SSDP_PORT, location, announcements)
    }

    /// As [`Self::bind`], but on an arbitrary port rather than the fixed SSDP port. Test-only: SSDP
    /// has exactly one real port, so concurrent test runs binding it with `SO_REUSEPORT` would race
    /// each other for which instance actually receives a given datagram (Linux load-balances between
    /// same-port listeners by flow hash, not "whichever socket you meant"). Multicast group
    /// membership is per-interface and per-multicast-address, not tied to the local port, so this
    /// still exercises the identical bind/reuse/join path a real caller goes through.
    #[doc(hidden)]
    pub fn bind_on(
        port: u16,
        location: String,
        announcements: Vec<Announcement>,
    ) -> std::io::Result<Self> {
        let sock = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        sock.set_reuse_address(true)?;
        #[cfg(unix)]
        sock.set_reuse_port(true)?; // No Windows equivalent; SO_REUSEADDR alone covers it there.
        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        sock.bind(&bind_addr.into())?;
        sock.join_multicast_v4(&SSDP_MULTICAST_IP, &Ipv4Addr::UNSPECIFIED)?;
        Ok(Self { socket: sock.into(), announcements, location })
    }

    /// Multicast one `ssdp:alive` `NOTIFY` per announcement -- called once by [`Self::run`] on
    /// startup and then on every `renotify_interval`, and available directly for a caller that wants
    /// to announce immediately without waiting for the first tick.
    pub fn notify_alive(&self) -> std::io::Result<()> {
        for a in &self.announcements {
            let msg = build_notify_alive(a, &self.location, DEFAULT_MAX_AGE_SECS);
            self.socket.send_to(msg.as_bytes(), SSDP_MULTICAST_ADDR)?;
        }
        Ok(())
    }

    /// Multicast one `ssdp:byebye` `NOTIFY` per announcement. Not called automatically by anything in
    /// this crate -- a caller wanting a clean withdrawal on shutdown calls this itself before exiting.
    pub fn notify_byebye(&self) -> std::io::Result<()> {
        for a in &self.announcements {
            self.socket.send_to(build_notify_byebye(a).as_bytes(), SSDP_MULTICAST_ADDR)?;
        }
        Ok(())
    }

    /// Wait up to `timeout` for one incoming datagram and answer it if it is a matching `M-SEARCH`.
    /// `Ok(true)` if a reply was sent, `Ok(false)` for a timeout or any datagram that was not a
    /// matching search -- a `NOTIFY` from another device on the same group, an `M-SEARCH` for
    /// something this responder does not announce, malformed input -- none of which are errors, only
    /// a genuine socket failure is.
    pub fn respond_once(&self, timeout: Duration) -> std::io::Result<bool> {
        self.socket.set_read_timeout(Some(timeout))?;
        let mut buf = [0u8; 2048];
        let (n, from) = match self.socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e) if is_timeout(&e) => return Ok(false),
            Err(e) => return Err(e),
        };
        let Some(req) = parse_msearch(&buf[..n]) else { return Ok(false) };
        let mut answered = false;
        for a in &self.announcements {
            if matches_search_target(&a.notification_type, &req.search_target) {
                let msg = build_search_response(a, &self.location, DEFAULT_MAX_AGE_SECS);
                self.socket.send_to(msg.as_bytes(), from)?;
                answered = true;
            }
        }
        Ok(answered)
    }

    /// Block forever: answer every matching `M-SEARCH` as it arrives, and re-announce `ssdp:alive` on
    /// `renotify_interval` so a control point that joined the network after startup still sees this
    /// device without having to ask. Mirrors `lumen_play::remote::server::run`'s own "blocks until the
    /// process is killed" shape -- meant to run on its own thread, the same way that server does.
    pub fn run(&self, renotify_interval: Duration, log: impl Fn(&str)) {
        let _ = self.notify_alive();
        let mut last_notify = std::time::Instant::now();
        loop {
            if let Err(e) = self.respond_once(Duration::from_millis(500)) {
                log(&format!("SSDP responder socket error: {e}"));
            }
            if last_notify.elapsed() >= renotify_interval {
                let _ = self.notify_alive();
                last_notify = std::time::Instant::now();
            }
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement() -> Announcement {
        Announcement {
            notification_type: "upnp:rootdevice".into(),
            unique_service_name: "uuid:test-device::upnp:rootdevice".into(),
        }
    }

    /// A real bind, a real multicast group join, and a real `M-SEARCH` answered over an actual UDP
    /// socket -- the same "prove the pieces work assembled" bar `lumen-play`'s own
    /// `remote_serve.rs` integration test holds itself to. Sent as unicast directly at the
    /// responder's own bound address rather than through the multicast group itself, so this does
    /// not depend on the sandbox actually routing multicast traffic (many CI network namespaces do
    /// not) -- a bound socket answers unicast traffic on its port exactly the same way regardless of
    /// what multicast groups it has also joined.
    ///
    /// Skipped, not failed, when this environment cannot bind the SSDP port or join the multicast
    /// group at all -- a sandboxed network namespace, insufficient privilege, or another responder
    /// already holding the port in a way `SO_REUSEADDR` cannot reconcile. Infrastructure, not a
    /// defect in this crate, the same convention `remote_serve.rs` uses for a missing `mpv`.
    #[test]
    fn a_real_msearch_over_the_wire_gets_a_real_unicast_reply() {
        let responder = match Responder::bind_on(
            19191,
            "http://127.0.0.1:0/desc.xml".into(),
            vec![announcement()],
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("skipping: cannot bind/join the SSDP multicast group here: {e}");
                return;
            }
        };

        let searcher =
            UdpSocket::bind("0.0.0.0:0").expect("an ordinary ephemeral bind must succeed");
        // Not `responder.socket.local_addr()`: a socket bound to `INADDR_ANY` reports its own local
        // address as `0.0.0.0:19191`, which is a wildcard for *binding*, not a valid destination to
        // *send* to. `127.0.0.1:19191` reaches the same socket, since binding to ANY means "receive on
        // every local address, loopback included".
        let request =
            b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\nST: upnp:rootdevice\r\nMX: 1\r\n\r\n";
        searcher
            .send_to(request, (Ipv4Addr::LOCALHOST, 19191))
            .expect("sending a unicast datagram to a bound UDP socket must succeed");

        let answered = responder
            .respond_once(Duration::from_secs(3))
            .expect("a genuine socket error would be unexpected here");
        assert!(answered, "a matching M-SEARCH must be answered");

        searcher.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut buf = [0u8; 2048];
        let (n, _) = searcher.recv_from(&mut buf).expect("the reply must reach the searcher");
        let reply = String::from_utf8_lossy(&buf[..n]);
        assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
        assert!(reply.contains("uuid:test-device::upnp:rootdevice"), "{reply}");
    }

    #[test]
    fn a_search_for_something_not_announced_gets_no_reply() {
        let responder = match Responder::bind_on(
            19192,
            "http://127.0.0.1:0/desc.xml".into(),
            vec![announcement()],
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("skipping: cannot bind/join the SSDP multicast group here: {e}");
                return;
            }
        };
        let searcher = UdpSocket::bind("0.0.0.0:0").unwrap();
        let request = b"M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\n\
                         ST: urn:schemas-upnp-org:device:MediaServer:1\r\nMX: 1\r\n\r\n";
        searcher.send_to(request, (Ipv4Addr::LOCALHOST, 19192)).unwrap();

        let answered = responder.respond_once(Duration::from_secs(2)).unwrap();
        assert!(!answered, "no announcement matches this search target");
    }
}
