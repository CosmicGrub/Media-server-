//! The UPnP device-description document (the `LOCATION` URL every SSDP announcement points at) and
//! the two services a `MediaServer` needs to be usable, not just discoverable:
//! `ContentDirectory:1` (browsing, the actual point) and `ConnectionManager:1` (many real renderers
//! query this before attempting to browse at all, and treat its absence as "not a real MediaServer" --
//! a UPnP-spec requirement this crate honours even though `Browse` is the feature that matters).

/// Everything the device description document needs to name this specific server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub friendly_name: String,
    /// The bare device UUID, no `uuid:` prefix and no `::`-suffixed notification type -- callers that
    /// need the `UDN` form (`uuid:<this>`) or a `USN` (`uuid:<this>::<nt>`) add those themselves, the
    /// same raw value [`crate::Announcement`] and this document both build their own prefixed forms
    /// from.
    pub uuid: String,
}

/// The root device description document. `base_url` (e.g. `http://192.168.1.5:8200`) prefixes every
/// relative URL this document carries, so `SCPDURL`/`controlURL`/`eventSubURL` resolve correctly no
/// matter what address the server is actually reachable at on this network.
pub fn build_device_description(identity: &DeviceIdentity, base_url: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\
         <root xmlns=\"urn:schemas-upnp-org:device-1-0\">\
         <specVersion><major>1</major><minor>0</minor></specVersion>\
         <device>\
         <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>\
         <friendlyName>{}</friendlyName>\
         <manufacturer>lumen</manufacturer>\
         <manufacturerURL>https://github.com/CosmicGrub/Media-server-</manufacturerURL>\
         <modelDescription>lumen serve</modelDescription>\
         <modelName>lumen-serve</modelName>\
         <modelNumber>{}</modelNumber>\
         <UDN>uuid:{}</UDN>\
         <serviceList>\
         <service>\
         <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>\
         <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>\
         <SCPDURL>{base_url}/dlna/cd.xml</SCPDURL>\
         <controlURL>{base_url}/dlna/cd/control</controlURL>\
         <eventSubURL>{base_url}/dlna/cd/event</eventSubURL>\
         </service>\
         <service>\
         <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>\
         <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>\
         <SCPDURL>{base_url}/dlna/cm.xml</SCPDURL>\
         <controlURL>{base_url}/dlna/cm/control</controlURL>\
         <eventSubURL>{base_url}/dlna/cm/event</eventSubURL>\
         </service>\
         </serviceList>\
         </device>\
         </root>",
        escape_xml(&identity.friendly_name),
        env!("CARGO_PKG_VERSION"),
        identity.uuid,
    )
}

/// `ContentDirectory:1`'s SCPD: the three actions this responder actually implements (`Browse`;
/// `Search` -- only over the bounded `SearchCriteria` subset `content_directory`'s own module doc
/// describes: `"*"` and single-clause `dc:title contains "..."`, not the full UPnP search grammar;
/// and `GetSystemUpdateID`, whose single `Id` out-argument is the evented `SystemUpdateID` state
/// variable itself, answered from `dlna.rs`'s live counter -- see
/// [`crate::build_get_system_update_id_response`]), plus the read-only state variables every real
/// client expects to find declared even when it never queries them directly (`SortCriteria` among
/// them). Declaring an action this server does not implement would be the dishonest direction to get
/// wrong -- a client that trusts the SCPD and calls it gets a SOAP fault it had no way to expect --
/// which is exactly why `GetSystemUpdateID` was *not* declared until `dlna.rs` had a value that
/// actually moves to answer it with. `SystemUpdateID` is declared `sendEvents="yes"` per the spec,
/// but this responder serves no `eventSubURL` subscription (GENA) -- a client learns of a change by
/// polling `GetSystemUpdateID` or by comparing the `UpdateID` its next `Browse` returns, both of which
/// real renderers already do; pushing GENA events is future work, not a claim made here. `Search`'s
/// `ContainerID` argument reuses `A_ARG_TYPE_ObjectID` rather than declaring its own state variable,
/// matching the real UPnP `ContentDirectory:1` spec's own SCPD (a container id and an object id are
/// the same string type; UPnP does not require a state variable's name to mirror the argument name
/// that uses it).
pub fn content_directory_scpd() -> &'static str {
    "<?xml version=\"1.0\"?>\
     <scpd xmlns=\"urn:schemas-upnp-org:service-1-0\">\
     <specVersion><major>1</major><minor>0</minor></specVersion>\
     <actionList>\
     <action>\
     <name>Browse</name>\
     <argumentList>\
     <argument><name>ObjectID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ObjectID</relatedStateVariable></argument>\
     <argument><name>BrowseFlag</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_BrowseFlag</relatedStateVariable></argument>\
     <argument><name>Filter</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Filter</relatedStateVariable></argument>\
     <argument><name>StartingIndex</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Index</relatedStateVariable></argument>\
     <argument><name>RequestedCount</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>SortCriteria</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_SortCriteria</relatedStateVariable></argument>\
     <argument><name>Result</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Result</relatedStateVariable></argument>\
     <argument><name>NumberReturned</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>TotalMatches</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>UpdateID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_UpdateID</relatedStateVariable></argument>\
     </argumentList>\
     </action>\
     <action>\
     <name>Search</name>\
     <argumentList>\
     <argument><name>ContainerID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ObjectID</relatedStateVariable></argument>\
     <argument><name>SearchCriteria</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_SearchCriteria</relatedStateVariable></argument>\
     <argument><name>Filter</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Filter</relatedStateVariable></argument>\
     <argument><name>StartingIndex</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Index</relatedStateVariable></argument>\
     <argument><name>RequestedCount</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>SortCriteria</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_SortCriteria</relatedStateVariable></argument>\
     <argument><name>Result</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Result</relatedStateVariable></argument>\
     <argument><name>NumberReturned</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>TotalMatches</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_Count</relatedStateVariable></argument>\
     <argument><name>UpdateID</name><direction>out</direction><relatedStateVariable>A_ARG_TYPE_UpdateID</relatedStateVariable></argument>\
     </argumentList>\
     </action>\
     <action>\
     <name>GetSystemUpdateID</name>\
     <argumentList>\
     <argument><name>Id</name><direction>out</direction><relatedStateVariable>SystemUpdateID</relatedStateVariable></argument>\
     </argumentList>\
     </action>\
     </actionList>\
     <serviceStateTable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_ObjectID</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_BrowseFlag</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_SearchCriteria</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_Filter</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_Index</name><dataType>ui4</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_Count</name><dataType>ui4</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_SortCriteria</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_Result</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_UpdateID</name><dataType>ui4</dataType></stateVariable>\
     <stateVariable sendEvents=\"yes\"><name>SystemUpdateID</name><dataType>ui4</dataType></stateVariable>\
     </serviceStateTable>\
     </scpd>"
}

/// `ConnectionManager:1`'s SCPD: the three actions the spec requires every implementation to answer,
/// declared honestly as the stubs `content_directory` module's server-side handler actually returns
/// (`GetProtocolInfo` alone carries real information -- what MIME types this server can source).
pub fn connection_manager_scpd() -> &'static str {
    "<?xml version=\"1.0\"?>\
     <scpd xmlns=\"urn:schemas-upnp-org:service-1-0\">\
     <specVersion><major>1</major><minor>0</minor></specVersion>\
     <actionList>\
     <action><name>GetProtocolInfo</name><argumentList>\
     <argument><name>Source</name><direction>out</direction><relatedStateVariable>SourceProtocolInfo</relatedStateVariable></argument>\
     <argument><name>Sink</name><direction>out</direction><relatedStateVariable>SinkProtocolInfo</relatedStateVariable></argument>\
     </argumentList></action>\
     <action><name>GetCurrentConnectionIDs</name><argumentList>\
     <argument><name>ConnectionIDs</name><direction>out</direction><relatedStateVariable>CurrentConnectionIDs</relatedStateVariable></argument>\
     </argumentList></action>\
     <action><name>GetCurrentConnectionInfo</name><argumentList>\
     <argument><name>ConnectionID</name><direction>in</direction><relatedStateVariable>A_ARG_TYPE_ConnectionID</relatedStateVariable></argument>\
     </argumentList></action>\
     </actionList>\
     <serviceStateTable>\
     <stateVariable sendEvents=\"yes\"><name>SourceProtocolInfo</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"yes\"><name>SinkProtocolInfo</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"yes\"><name>CurrentConnectionIDs</name><dataType>string</dataType></stateVariable>\
     <stateVariable sendEvents=\"no\"><name>A_ARG_TYPE_ConnectionID</name><dataType>i4</dataType></stateVariable>\
     </serviceStateTable>\
     </scpd>"
}

/// `ConnectionManager:1#GetProtocolInfo`'s response: `source_mime_types` becomes the `Source` value
/// (what this server can send), `Sink` is always empty -- `lumen serve` only ever sources media, it
/// never accepts an incoming push.
pub fn build_get_protocol_info_response(source_mime_types: &[&str]) -> String {
    let source: Vec<String> =
        source_mime_types.iter().map(|m| format!("http-get:*:{m}:*")).collect();
    format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:GetProtocolInfoResponse xmlns:u=\"urn:schemas-upnp-org:service:ConnectionManager:1\">\
         <Source>{}</Source><Sink></Sink>\
         </u:GetProtocolInfoResponse></s:Body></s:Envelope>",
        escape_xml(&source.join(",")),
    )
}

/// `ConnectionManager:1#GetCurrentConnectionIDs`'s response: always `"0"` -- `lumen serve` does not
/// track individual DLNA connections as distinct sessions (a real gap, not a claim of full protocol
/// support), so the single implicit connection ID every client already assumes if none is enumerated.
pub fn build_get_current_connection_ids_response() -> String {
    "<?xml version=\"1.0\"?>\
     <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
     s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
     <s:Body><u:GetCurrentConnectionIDsResponse xmlns:u=\"urn:schemas-upnp-org:service:ConnectionManager:1\">\
     <ConnectionIDs>0</ConnectionIDs>\
     </u:GetCurrentConnectionIDsResponse></s:Body></s:Envelope>"
        .to_string()
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> DeviceIdentity {
        DeviceIdentity { friendly_name: "Living Room PC".into(), uuid: "abcd-1234".into() }
    }

    #[test]
    fn the_device_description_names_the_media_server_type_and_both_services() {
        let doc = build_device_description(&identity(), "http://192.168.1.5:8200");
        assert!(doc.contains("<deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>"));
        assert!(doc.contains("<friendlyName>Living Room PC</friendlyName>"));
        assert!(doc.contains("<UDN>uuid:abcd-1234</UDN>"));
        assert!(doc.contains("urn:schemas-upnp-org:service:ContentDirectory:1"));
        assert!(doc.contains("urn:schemas-upnp-org:service:ConnectionManager:1"));
        assert!(doc.contains("http://192.168.1.5:8200/dlna/cd/control"));
        assert!(doc.contains("http://192.168.1.5:8200/dlna/cm/control"));
    }

    #[test]
    fn a_friendly_name_with_reserved_characters_is_escaped() {
        let doc = build_device_description(
            &DeviceIdentity { friendly_name: "Bob & Alice's PC".into(), uuid: "x".into() },
            "http://x",
        );
        assert!(doc.contains("Bob &amp; Alice&apos;s PC"));
    }

    #[test]
    fn the_content_directory_scpd_declares_exactly_the_three_actions_this_server_implements() {
        let scpd = content_directory_scpd();
        assert!(scpd.contains("<name>Browse</name>"));
        assert!(scpd.contains("<name>ObjectID</name>"));
        assert!(scpd.contains("<name>Result</name>"));
        assert!(scpd.contains("<name>Search</name>"));
        assert!(scpd.contains("<name>ContainerID</name>"));
        assert!(scpd.contains("<name>SearchCriteria</name>"));
        assert!(
            scpd.contains("A_ARG_TYPE_SearchCriteria"),
            "SearchCriteria must be declared as its own state variable: {scpd}"
        );
        assert!(scpd.contains("<name>GetSystemUpdateID</name>"), "{scpd}");
        assert_eq!(
            scpd.matches("<action>").count(),
            3,
            "Browse, Search, GetSystemUpdateID -- and nothing this server does not answer: {scpd}"
        );
    }

    #[test]
    fn get_system_update_id_is_declared_with_a_single_out_argument_bound_to_system_update_id() {
        let scpd = content_directory_scpd();
        let start = scpd.find("<name>GetSystemUpdateID</name>").expect("the action is declared");
        let end = start + scpd[start..].find("</action>").expect("the action is closed");
        let action = &scpd[start..end];
        assert_eq!(action.matches("<argument>").count(), 1, "exactly one argument: {action}");
        assert!(action.contains("<name>Id</name>"), "{action}");
        assert!(action.contains("<direction>out</direction>"), "{action}");
        assert!(
            action.contains("<relatedStateVariable>SystemUpdateID</relatedStateVariable>"),
            "Id must be bound to the evented SystemUpdateID variable itself, not an A_ARG_TYPE: {action}"
        );
        assert!(
            scpd.contains("<stateVariable sendEvents=\"yes\"><name>SystemUpdateID</name><dataType>ui4</dataType>"),
            "the bound state variable must still be declared as a ui4: {scpd}"
        );
    }

    #[test]
    fn the_connection_manager_scpd_declares_the_three_required_actions() {
        let scpd = connection_manager_scpd();
        for action in ["GetProtocolInfo", "GetCurrentConnectionIDs", "GetCurrentConnectionInfo"] {
            assert!(scpd.contains(&format!("<name>{action}</name>")), "missing {action}");
        }
    }

    #[test]
    fn protocol_info_lists_every_source_mime_type_and_leaves_sink_empty() {
        let resp = build_get_protocol_info_response(&["video/x-matroska", "video/mp4"]);
        assert!(resp.contains("http-get:*:video/x-matroska:*,http-get:*:video/mp4:*"));
        assert!(resp.contains("<Sink></Sink>"), "this server only ever sources, never receives");
    }

    #[test]
    fn current_connection_ids_reports_the_single_implicit_connection() {
        assert!(
            build_get_current_connection_ids_response()
                .contains("<ConnectionIDs>0</ConnectionIDs>")
        );
    }
}
