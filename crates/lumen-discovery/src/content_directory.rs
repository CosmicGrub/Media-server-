//! DLNA `ContentDirectory:1`: the SOAP `Browse` and `Search` actions, and the DIDL-Lite XML they
//! return.
//!
//! **Not a general XML parser or writer.** `parse_browse_request`/`parse_search_request` are a
//! bounded scan for the handful of specific, known leaf elements a `Browse`/`Search` SOAP body
//! carries -- the same "extract only what a specific, known structure needs" scope `lumen-probe`'s
//! EBML/ISOBMFF readers already keep to, not a document tree. `build_didl_lite` writes exactly the
//! DIDL-Lite shape a `Browse`/`Search` response needs, nothing more general.
//!
//! **`Search`'s scope is deliberately bounded, not a general UPnP search-grammar parser.** The real
//! `SearchCriteria` grammar UPnP `ContentDirectory:1` defines supports arbitrary boolean expressions
//! (`and`/`or`, parenthesised nesting), several relational operators (`contains`, `derivedfrom`, `=`,
//! `<`, ...) over arbitrary properties (`dc:title`, `dc:creator`, `upnp:class`, ...), and full string
//! escaping throughout. Implementing that honestly is a small parser-combinator project in its own
//! right, and almost no real client sends anything beyond two shapes in practice: `"*"` (match
//! everything, letting the client filter client-side) and a single `dc:title contains "..."` clause
//! (the "search box" case). [`SearchCriteria`] models exactly those two, plus an explicit
//! [`SearchCriteria::Unsupported`] third case for everything else -- answered with a `708` SOAP fault
//! (see `dlna.rs`'s `handle_search`) rather than silently guessed at, misinterpreted as `MatchAll`, or
//! panicking. Extending this to more of the real grammar is future work, not a claim of full spec
//! coverage today.

/// Which of DIDL-Lite's two browse modes was requested: the object itself (`BrowseMetadata`, used to
/// resolve one object's own properties) or its children (`BrowseDirectChildren`, used to list a
/// folder). The only two values UPnP `ContentDirectory:1` defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseFlag {
    Metadata,
    DirectChildren,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseRequest {
    pub object_id: String,
    pub flag: BrowseFlag,
    /// How many results to skip before the first one returned -- pagination, not a filter.
    pub starting_index: u32,
    /// `0` means "no limit" per the UPnP spec's own convention, not "return nothing".
    pub requested_count: u32,
}

/// Parse a `Browse` SOAP request body. `None` when the required elements (`ObjectID`, `BrowseFlag`)
/// are missing or `BrowseFlag` is not one of the two values this responder understands -- the caller
/// answers a SOAP fault rather than guessing at what was meant.
pub fn parse_browse_request(soap_body: &str) -> Option<BrowseRequest> {
    let object_id = xml_element_text(soap_body, "ObjectID")?;
    let flag = match xml_element_text(soap_body, "BrowseFlag")?.as_str() {
        "BrowseMetadata" => BrowseFlag::Metadata,
        "BrowseDirectChildren" => BrowseFlag::DirectChildren,
        _ => return None,
    };
    let starting_index =
        xml_element_text(soap_body, "StartingIndex").and_then(|s| s.parse().ok()).unwrap_or(0);
    let requested_count =
        xml_element_text(soap_body, "RequestedCount").and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(BrowseRequest { object_id, flag, starting_index, requested_count })
}

/// The bounded subset of UPnP `SearchCriteria` this responder understands -- see this module's own
/// doc for why the real grammar is not implemented in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchCriteria {
    /// The literal criteria string `"*"`: every item under the searched container, recursively. The
    /// common case a client sends when it wants a flat listing to filter client-side.
    MatchAll,
    /// The single-clause form `dc:title contains "TEXT"` -- matched case-insensitively per the UPnP
    /// spec's own `contains` semantics. `TEXT` has already had UPnP's `""`-for-a-literal-`"` quoting
    /// undone.
    TitleContains(String),
    /// Anything this parser could not confidently interpret as one of the two cases above: the full
    /// boolean grammar, a property other than `dc:title`, an operator other than `contains`, or a
    /// malformed quoted string. Distinct from the whole request failing to parse (`None` from
    /// [`parse_search_request`]) -- `ContainerID`/`StartingIndex`/`RequestedCount` can still be
    /// genuinely known even when the criteria string itself is unintelligible, so the caller decides
    /// what to do with an otherwise-valid request carrying criteria it cannot honour (a `708` SOAP
    /// fault, in `dlna.rs`'s `handle_search`).
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub container_id: String,
    pub criteria: SearchCriteria,
    /// How many results to skip before the first one returned -- pagination, not a filter.
    pub starting_index: u32,
    /// `0` means "no limit" per the UPnP spec's own convention, not "return nothing".
    pub requested_count: u32,
}

/// Parse a `Search` SOAP request body. `None` only when a required element (`ContainerID` or
/// `SearchCriteria`) is missing entirely -- an unintelligible-but-present `SearchCriteria` string
/// still yields `Some(SearchRequest { criteria: SearchCriteria::Unsupported, .. })` rather than
/// failing the whole request, since the other fields can still be genuinely known (see
/// [`SearchCriteria::Unsupported`]'s own doc).
pub fn parse_search_request(soap_body: &str) -> Option<SearchRequest> {
    let container_id = xml_element_text(soap_body, "ContainerID")?;
    let criteria = parse_search_criteria(&xml_element_text(soap_body, "SearchCriteria")?);
    let starting_index =
        xml_element_text(soap_body, "StartingIndex").and_then(|s| s.parse().ok()).unwrap_or(0);
    let requested_count =
        xml_element_text(soap_body, "RequestedCount").and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(SearchRequest { container_id, criteria, starting_index, requested_count })
}

/// Classify one already-extracted `SearchCriteria` string into the bounded set this responder
/// understands. Never panics on malformed input -- an unrecognised shape is
/// [`SearchCriteria::Unsupported`], not a crash.
fn parse_search_criteria(text: &str) -> SearchCriteria {
    let trimmed = text.trim();
    if trimmed == "*" {
        return SearchCriteria::MatchAll;
    }
    if let Some(rest) = trimmed.strip_prefix("dc:title") {
        if let Some(rest) = rest.trim_start().strip_prefix("contains") {
            if let Some(needle) = parse_search_quoted_string(rest.trim_start()) {
                return SearchCriteria::TitleContains(needle);
            }
        }
    }
    SearchCriteria::Unsupported
}

/// Parse one UPnP search-string literal: a double-quoted string in which `""` represents a literal
/// `"` (the escaping form the `ContentDirectory` search grammar defines for quoted values). `None` --
/// never a panic -- for anything that is not a complete, properly quoted literal: missing opening
/// quote, an unterminated string, or non-whitespace trailing the closing quote.
fn parse_search_quoted_string(s: &str) -> Option<String> {
    let body = s.strip_prefix('"')?;
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            if chars.get(i + 1) == Some(&'"') {
                out.push('"');
                i += 2;
                continue;
            }
            let trailing: String = chars[i + 1..].iter().collect();
            return trailing.trim().is_empty().then_some(out);
        }
        out.push(chars[i]);
        i += 1;
    }
    None // No closing quote was ever found.
}

/// The DIDL-Lite `upnp:class` for one object -- what tells a renderer whether something is a folder
/// to descend into or a file to play, and for a file, roughly what kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObjectClass {
    StorageFolder,
    VideoItem,
    AudioItem,
    ImageItem,
}

impl ObjectClass {
    fn upnp_class(self) -> &'static str {
        match self {
            Self::StorageFolder => "object.container.storageFolder",
            Self::VideoItem => "object.item.videoItem",
            Self::AudioItem => "object.item.audioItem",
            Self::ImageItem => "object.item.imageItem",
        }
    }

    fn is_container(self) -> bool {
        matches!(self, Self::StorageFolder)
    }
}

/// The playable resource one DIDL-Lite `<item>` carries -- a URL a renderer fetches directly, with no
/// auth of its own, since DLNA carries none by protocol design (see this crate's module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DidlResource {
    pub url: String,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
}

/// One DIDL-Lite object: a container (folder) or an item (playable file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DidlObject {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub class: ObjectClass,
    /// `Some` for a playable item, `None` for a container -- a container has nothing to stream, only
    /// children to browse into.
    pub resource: Option<DidlResource>,
}

/// Build the `<DIDL-Lite>...</DIDL-Lite>` document a `Browse` response's `Result` element carries.
pub fn build_didl_lite(objects: &[DidlObject]) -> String {
    let mut s = String::from(
        "<DIDL-Lite xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">",
    );
    for o in objects {
        if o.class.is_container() {
            s.push_str(&format!(
                "<container id=\"{}\" parentID=\"{}\" restricted=\"1\" searchable=\"1\">\
                 <dc:title>{}</dc:title><upnp:class>{}</upnp:class></container>",
                escape_xml(&o.id),
                escape_xml(&o.parent_id),
                escape_xml(&o.title),
                o.class.upnp_class(),
            ));
        } else {
            let res = o.resource.as_ref().map_or_else(String::new, |r| {
                let size_attr = r.size_bytes.map_or_else(String::new, |n| format!(" size=\"{n}\""));
                format!(
                    "<res protocolInfo=\"http-get:*:{}:*\"{size_attr}>{}</res>",
                    escape_xml(&r.mime_type),
                    escape_xml(&r.url),
                )
            });
            s.push_str(&format!(
                "<item id=\"{}\" parentID=\"{}\" restricted=\"1\">\
                 <dc:title>{}</dc:title><upnp:class>{}</upnp:class>{res}</item>",
                escape_xml(&o.id),
                escape_xml(&o.parent_id),
                escape_xml(&o.title),
                o.class.upnp_class(),
            ));
        }
    }
    s.push_str("</DIDL-Lite>");
    s
}

/// Wrap a `Browse` response's DIDL-Lite document in its SOAP envelope. `Result` carries the DIDL-Lite
/// XML *escaped as text*, per the UPnP spec -- it is not inlined as child elements of the envelope,
/// which is why [`escape_xml`] is applied to already-XML content here rather than being a mistake.
pub fn build_browse_response(
    didl_lite: &str,
    number_returned: u32,
    total_matches: u32,
    update_id: u32,
) -> String {
    build_content_directory_response(
        "BrowseResponse",
        didl_lite,
        number_returned,
        total_matches,
        update_id,
    )
}

/// As [`build_browse_response`], for a `Search` response -- identical shape (`Result`,
/// `NumberReturned`, `TotalMatches`, `UpdateID`), just with `SearchResponse` as the outer element name
/// per the UPnP spec. Named `build_cd_search_response` rather than `build_search_response`: this
/// crate's `message` module already exports a public `build_search_response` for a wholly different
/// concept (an SSDP `M-SEARCH` reply), and the crate root re-exports both modules' public items, so a
/// second same-named function here would not even compile.
pub fn build_cd_search_response(
    didl_lite: &str,
    number_returned: u32,
    total_matches: u32,
    update_id: u32,
) -> String {
    build_content_directory_response(
        "SearchResponse",
        didl_lite,
        number_returned,
        total_matches,
        update_id,
    )
}

/// Shared shape behind [`build_browse_response`] and [`build_cd_search_response`] -- the two response
/// bodies differ only in their outer element name.
fn build_content_directory_response(
    outer_element: &str,
    didl_lite: &str,
    number_returned: u32,
    total_matches: u32,
    update_id: u32,
) -> String {
    format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:{outer_element} xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
         <Result>{}</Result>\
         <NumberReturned>{number_returned}</NumberReturned>\
         <TotalMatches>{total_matches}</TotalMatches>\
         <UpdateID>{update_id}</UpdateID>\
         </u:{outer_element}></s:Body></s:Envelope>",
        escape_xml(didl_lite),
    )
}

/// A SOAP fault, for a `Browse` request this responder could not satisfy -- an unrecognised
/// `ObjectID`, chief among the real cases, is `701` ("No such object") per the ContentDirectory spec.
pub fn build_soap_fault(code: u32, description: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>\
         <detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
         <errorCode>{code}</errorCode><errorDescription>{}</errorDescription>\
         </UPnPError></detail></s:Fault></s:Body></s:Envelope>",
        escape_xml(description),
    )
}

/// Extract the text content of a leaf element, regardless of XML namespace prefix (`<ObjectID>` and
/// `<u:ObjectID>` are both found by searching for `ObjectID`). Bounded and total: `None` for a tag
/// that never appears, never a panic on truncated or malformed input.
fn xml_element_text(xml: &str, tag: &str) -> Option<String> {
    let mut search_from = 0;
    loop {
        let rel = xml[search_from..].find(tag)?;
        let idx = search_from + rel;
        let preceded_ok = idx > 0 && matches!(xml.as_bytes()[idx - 1], b'<' | b':');
        let after = &xml[idx + tag.len()..];
        let followed_ok =
            after.starts_with('>') || after.starts_with(' ') || after.starts_with('/');
        if preceded_ok && followed_ok {
            let gt = xml[idx..].find('>')?;
            let open_end = idx + gt + 1;
            if open_end >= 2 && xml.as_bytes()[open_end - 2] == b'/' {
                return Some(String::new()); // Self-closing, e.g. `<SortCriteria/>`.
            }
            let lt = xml[open_end..].find('<')?;
            return Some(unescape_xml(&xml[open_end..open_end + lt]));
        }
        search_from = idx + tag.len();
        if search_from >= xml.len() {
            return None;
        }
    }
}

/// Escape the five characters XML text/attribute content cannot carry literally. Applied to every
/// piece of caller-supplied text (titles, IDs, URLs) before it goes anywhere near a tag boundary --
/// a title containing `&` or `<` is real, ordinary data (a Blu-ray title with "Ampersand & Friends"),
/// not something to reject.
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

/// The inverse of [`escape_xml`], for reading a leaf element's text content back out.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&") // Last: must not re-unescape an entity produced by an earlier replace.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn browse_soap(object_id: &str, flag: &str, starting_index: &str, count: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
             <s:Body><u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <ObjectID>{object_id}</ObjectID><BrowseFlag>{flag}</BrowseFlag>\
             <Filter>*</Filter><StartingIndex>{starting_index}</StartingIndex>\
             <RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>\
             </u:Browse></s:Body></s:Envelope>"
        )
    }

    #[test]
    fn a_well_formed_browse_request_is_parsed_regardless_of_namespace_prefix() {
        let req =
            parse_browse_request(&browse_soap("0", "BrowseDirectChildren", "0", "50")).unwrap();
        assert_eq!(req.object_id, "0");
        assert_eq!(req.flag, BrowseFlag::DirectChildren);
        assert_eq!(req.starting_index, 0);
        assert_eq!(req.requested_count, 50);
    }

    #[test]
    fn browse_metadata_is_distinguished_from_direct_children() {
        let req = parse_browse_request(&browse_soap("42", "BrowseMetadata", "0", "1")).unwrap();
        assert_eq!(req.flag, BrowseFlag::Metadata);
    }

    #[test]
    fn an_unrecognised_browse_flag_is_refused_rather_than_guessed_at() {
        assert!(parse_browse_request(&browse_soap("0", "SomethingElse", "0", "0")).is_none());
    }

    #[test]
    fn missing_pagination_fields_default_rather_than_being_refused() {
        let soap = "<s:Body><u:Browse><ObjectID>0</ObjectID>\
                     <BrowseFlag>BrowseDirectChildren</BrowseFlag></u:Browse></s:Body>";
        let req = parse_browse_request(soap).unwrap();
        assert_eq!(req.starting_index, 0);
        assert_eq!(req.requested_count, 0, "0 means unlimited, the correct absent-header default");
    }

    #[test]
    fn a_missing_object_id_or_browse_flag_is_refused() {
        assert!(parse_browse_request("<s:Body></s:Body>").is_none());
        assert!(
            parse_browse_request("<s:Body><ObjectID>0</ObjectID></s:Body>").is_none(),
            "no BrowseFlag at all"
        );
    }

    #[test]
    fn didl_lite_distinguishes_containers_from_items_and_carries_a_resource() {
        let objects = vec![
            DidlObject {
                id: "1".into(),
                parent_id: "0".into(),
                title: "Movies".into(),
                class: ObjectClass::StorageFolder,
                resource: None,
            },
            DidlObject {
                id: "2".into(),
                parent_id: "1".into(),
                title: "Arrival (2016).mkv".into(),
                class: ObjectClass::VideoItem,
                resource: Some(DidlResource {
                    url: "http://192.168.1.5:8200/dlna/2".into(),
                    mime_type: "video/x-matroska".into(),
                    size_bytes: Some(4_000_000_000),
                }),
            },
        ];
        let didl = build_didl_lite(&objects);
        assert!(didl.starts_with("<DIDL-Lite"));
        assert!(didl.contains("<container id=\"1\" parentID=\"0\""));
        assert!(didl.contains("object.container.storageFolder"));
        assert!(didl.contains("<item id=\"2\" parentID=\"1\""));
        assert!(didl.contains("object.item.videoItem"));
        assert!(didl.contains("protocolInfo=\"http-get:*:video/x-matroska:*\""));
        assert!(didl.contains("size=\"4000000000\""));
        assert!(didl.contains("http://192.168.1.5:8200/dlna/2"));
    }

    #[test]
    fn a_container_never_emits_a_res_element() {
        let objects = vec![DidlObject {
            id: "1".into(),
            parent_id: "0".into(),
            title: "Movies".into(),
            class: ObjectClass::StorageFolder,
            resource: None,
        }];
        let didl = build_didl_lite(&objects);
        assert!(!didl.contains("<res"));
    }

    #[test]
    fn a_title_with_reserved_xml_characters_is_escaped_and_readable_back_out() {
        let objects = vec![DidlObject {
            id: "3".into(),
            parent_id: "0".into(),
            title: "Ampersand & \"Friends\" <Special>".into(),
            class: ObjectClass::VideoItem,
            resource: None,
        }];
        let didl = build_didl_lite(&objects);
        assert!(!didl.contains("& \""), "raw & or \" must never appear unescaped in the XML");
        assert!(didl.contains("Ampersand &amp; &quot;Friends&quot; &lt;Special&gt;"));
    }

    #[test]
    fn a_browse_response_carries_the_didl_lite_as_escaped_text_and_the_right_counts() {
        let didl = build_didl_lite(&[]);
        let response = build_browse_response(&didl, 0, 0, 7);
        assert!(response.contains("<NumberReturned>0</NumberReturned>"));
        assert!(response.contains("<TotalMatches>0</TotalMatches>"));
        assert!(response.contains("<UpdateID>7</UpdateID>"));
        // The DIDL-Lite's own `<` must be escaped once it is inside `<Result>`, or the outer SOAP
        // document is no longer well-formed XML (an embedded `<DIDL-Lite>` opening tag would look
        // like a real child element of `<Result>` instead of text content).
        assert!(response.contains("&lt;DIDL-Lite"));
        assert!(!response.contains("<Result><DIDL-Lite"));
    }

    #[test]
    fn a_soap_fault_names_the_error_code_and_description() {
        let fault = build_soap_fault(701, "No such object");
        assert!(fault.contains("<errorCode>701</errorCode>"));
        assert!(fault.contains("No such object"));
        assert!(fault.contains("s:Fault"));
    }

    #[test]
    fn escape_and_unescape_round_trip_every_reserved_character() {
        let original = "<tag> & \"quoted\" 'apostrophe'";
        let escaped = escape_xml(original);
        assert!(!escaped.contains(['<', '>', '"', '\'']) || escaped.contains("&lt;"));
        assert_eq!(unescape_xml(&escaped), original);
    }

    #[test]
    fn every_container_is_now_searchable() {
        let objects = vec![DidlObject {
            id: "1".into(),
            parent_id: "0".into(),
            title: "Movies".into(),
            class: ObjectClass::StorageFolder,
            resource: None,
        }];
        let didl = build_didl_lite(&objects);
        assert!(didl.contains("searchable=\"1\""), "{didl}");
        assert!(!didl.contains("searchable=\"0\""), "{didl}");
    }

    fn search_soap(
        container_id: &str,
        criteria: &str,
        starting_index: &str,
        count: &str,
    ) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
             <s:Body><u:Search xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <ContainerID>{container_id}</ContainerID><SearchCriteria>{criteria}</SearchCriteria>\
             <Filter>*</Filter><StartingIndex>{starting_index}</StartingIndex>\
             <RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>\
             </u:Search></s:Body></s:Envelope>"
        )
    }

    #[test]
    fn a_wildcard_search_criteria_is_match_all() {
        let req = parse_search_request(&search_soap("0", "*", "0", "0")).unwrap();
        assert_eq!(req.container_id, "0");
        assert_eq!(req.criteria, SearchCriteria::MatchAll);
    }

    #[test]
    fn a_title_contains_search_criteria_is_parsed_case_preserving_and_unquoted() {
        let req =
            parse_search_request(&search_soap("0", "dc:title contains \"chernobyl\"", "0", "50"))
                .unwrap();
        assert_eq!(req.criteria, SearchCriteria::TitleContains("chernobyl".into()));
        assert_eq!(req.starting_index, 0);
        assert_eq!(req.requested_count, 50);
    }

    #[test]
    fn a_doubled_quote_inside_the_search_string_is_unescaped_to_one_literal_quote() {
        // UPnP's own quoting for a literal `"` inside a search string is `""`, not backslash-escaping.
        let req = parse_search_request(&search_soap(
            "0",
            "dc:title contains \"a \"\"quoted\"\" word\"",
            "0",
            "0",
        ))
        .unwrap();
        assert_eq!(req.criteria, SearchCriteria::TitleContains("a \"quoted\" word".into()));
    }

    #[test]
    fn a_malformed_or_unrecognised_criteria_is_unsupported_not_a_failed_parse() {
        // The full boolean grammar, an unsupported property, and a truncated quoted string are all
        // real shapes a client could send that this bounded parser does not attempt -- each must
        // still yield a well-formed `SearchRequest` (the other fields are still genuinely known),
        // just with `SearchCriteria::Unsupported`, never `None` for the whole request.
        for criteria in [
            "dc:creator contains \"Nolan\"",
            "dc:title contains \"unterminated",
            "upnp:class derivedfrom \"object.item\"",
            "dc:title contains \"a\" and dc:title contains \"b\"",
        ] {
            let req = parse_search_request(&search_soap("0", criteria, "0", "0")).unwrap();
            assert_eq!(req.criteria, SearchCriteria::Unsupported, "{criteria}");
        }
    }

    #[test]
    fn a_missing_container_id_or_search_criteria_fails_the_whole_request() {
        assert!(parse_search_request("<s:Body></s:Body>").is_none());
        assert!(
            parse_search_request("<s:Body><ContainerID>0</ContainerID></s:Body>").is_none(),
            "no SearchCriteria at all"
        );
        assert!(
            parse_search_request("<s:Body><SearchCriteria>*</SearchCriteria></s:Body>").is_none(),
            "no ContainerID at all"
        );
    }

    #[test]
    fn missing_search_pagination_fields_default_rather_than_being_refused() {
        let soap = "<s:Body><u:Search><ContainerID>0</ContainerID>\
                     <SearchCriteria>*</SearchCriteria></u:Search></s:Body>";
        let req = parse_search_request(soap).unwrap();
        assert_eq!(req.starting_index, 0);
        assert_eq!(req.requested_count, 0, "0 means unlimited, the correct absent-header default");
    }

    #[test]
    fn a_search_response_carries_a_search_response_outer_element_not_browse() {
        let didl = build_didl_lite(&[]);
        let response = build_cd_search_response(&didl, 2, 5, 3);
        assert!(response.contains("<u:SearchResponse"), "{response}");
        assert!(response.contains("</u:SearchResponse>"), "{response}");
        assert!(!response.contains("BrowseResponse"), "{response}");
        assert!(response.contains("<NumberReturned>2</NumberReturned>"));
        assert!(response.contains("<TotalMatches>5</TotalMatches>"));
        assert!(response.contains("<UpdateID>3</UpdateID>"));
    }
}
