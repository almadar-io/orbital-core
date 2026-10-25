//! The `browser/tab-*` effect family at the kernel: the closed set of tab operations a program writes
//! ([`TabOp`]), the raw asks the kernel makes of a browser host to perform them ([`TabAsk`], [`TabCommand`])
//! and how a host fails one ([`TabFailure`]). The kernel validates every request and shapes every result; a host
//! only opens, closes and lists tabs and runs the raw commands in a page, and holds no decision.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

/// One variant per `browser/tab-<op>` effect.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabOp {
    List,
    Open,
    Close,
    Navigate,
    Query,
    Watch,
    Unwatch,
    Apply,
    ApplyMany,
    Act,
    Capture,
}

impl TabOp {
    pub const ALL: [TabOp; 11] = [
        TabOp::List,
        TabOp::Open,
        TabOp::Close,
        TabOp::Navigate,
        TabOp::Query,
        TabOp::Watch,
        TabOp::Unwatch,
        TabOp::Apply,
        TabOp::ApplyMany,
        TabOp::Act,
        TabOp::Capture,
    ];

    /// The effect's spelling in a program.
    pub fn name(self) -> &'static str {
        match self {
            TabOp::List => "browser/tab-list",
            TabOp::Open => "browser/tab-open",
            TabOp::Close => "browser/tab-close",
            TabOp::Navigate => "browser/tab-navigate",
            TabOp::Query => "browser/tab-query",
            TabOp::Watch => "browser/tab-watch",
            TabOp::Unwatch => "browser/tab-unwatch",
            TabOp::Apply => "browser/tab-apply",
            TabOp::ApplyMany => "browser/tab-apply-many",
            TabOp::Act => "browser/tab-act",
            TabOp::Capture => "browser/tab-capture",
        }
    }

    pub fn from_name(name: &str) -> Option<TabOp> {
        TabOp::ALL.into_iter().find(|op| op.name() == name)
    }

    /// The keys the op's request object may carry; any other key is refused.
    pub fn request_keys(self) -> &'static [&'static str] {
        match self {
            TabOp::List => &["match"],
            TabOp::Open => &["url", "params", "match", "active"],
            TabOp::Close => &["tab"],
            TabOp::Navigate => &["tab", "url"],
            TabOp::Query => &["tab", "selector", "fields", "limit", "waitMs"],
            TabOp::Watch => &["match", "selector", "fields", "event"],
            TabOp::Unwatch => &["watchId"],
            TabOp::Apply => &["tab", "elementId", "treatment", "label"],
            TabOp::ApplyMany => &["items"],
            TabOp::Act => &["tab", "elementId", "action", "text"],
            TabOp::Capture => &["tab", "format", "quality"],
        }
    }

    /// Whether `name` is a `browser/tab-*` effect.
    pub fn is_tab_effect(name: &str) -> bool {
        TabOp::from_name(name).is_some()
    }
}

/// How one field of a page element is read; the content script reads the declared fields of every element it
/// reports. `selector` names a descendant (absent: the element itself), `attr` an attribute (absent: its text),
/// `resolve` makes a URL value absolute, `template` composes the value from the other fields (`{name}` each; empty
/// when a referenced value is empty) and `required` leaves an element out until the field reads non-empty.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabField {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolve: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

/// One inline style the content script sets on an element for a treatment.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabStyle {
    pub property: String,
    pub value: String,
}

/// What the kernel's treatment (`hide`, `blur`, `highlight`, `label`, `restore`) comes to on one element: the
/// styles to set and the label text to show. Applying replaces the element's previous treatment; neither styles
/// nor a label restores it.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabApply {
    pub element_id: String,
    pub styles: Vec<TabStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// An interaction the content script performs on an element.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabAction {
    Click,
    Focus,
    Type,
}

impl TabAction {
    pub const ALL: [TabAction; 3] = [TabAction::Click, TabAction::Focus, TabAction::Type];

    pub fn as_str(self) -> &'static str {
        match self {
            TabAction::Click => "click",
            TabAction::Focus => "focus",
            TabAction::Type => "type",
        }
    }
}

/// The image format a `browser/tab-capture` asks for.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TabImageFormat {
    Png,
    Jpeg,
}

impl TabImageFormat {
    pub const ALL: [TabImageFormat; 2] = [TabImageFormat::Png, TabImageFormat::Jpeg];

    pub fn as_str(self) -> &'static str {
        match self {
            TabImageFormat::Png => "png",
            TabImageFormat::Jpeg => "jpeg",
        }
    }

    /// The `data:` URL prefix a host's capture of this format starts with.
    pub fn data_url_prefix(self) -> &'static str {
        match self {
            TabImageFormat::Png => "data:image/png;base64,",
            TabImageFormat::Jpeg => "data:image/jpeg;base64,",
        }
    }
}

/// A raw command for the content script of one tab.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabCommand {
    /// Read the elements matching `selector`, each with `fields`; with `waitMs`, wait that long for a first one.
    /// Replies the elements: `[{ elementId, fields }]`.
    #[serde(rename_all = "camelCase")]
    Query {
        selector: String,
        fields: Vec<TabField>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_ms: Option<u64>,
    },
    /// Report each matching element once, now and as the page changes, as the worker's `element` message.
    #[serde(rename_all = "camelCase")]
    Watch {
        watch_id: String,
        selector: String,
        fields: Vec<TabField>,
    },
    /// Stop a watch; replies whether it was running.
    #[serde(rename_all = "camelCase")]
    Unwatch { watch_id: String },
    /// Apply one treatment; replies `true`.
    Apply(TabApply),
    /// Apply each treatment; replies `{ applied: [elementId], missing: [elementId] }`, an element that left the
    /// page listed in `missing` instead of failing the rest.
    ApplyMany { items: Vec<TabApply> },
    /// Perform an interaction; replies `true`.
    #[serde(rename_all = "camelCase")]
    Act {
        element_id: String,
        action: TabAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// The declared external input one watched element is delivered to.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabDelivery {
    pub orbital: String,
    #[serde(rename = "trait")]
    pub trait_name: String,
    pub event: String,
}

/// One raw ask of the browser host. The reply of each is a JSON value of the shape its variant names.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TabAsk {
    /// Every open tab. Replies `[{ tab, url, title, active }]`.
    List,
    /// Open `url`. Replies the new tab's number.
    Open { url: String, active: bool },
    /// Close a tab. Replies `null`.
    Close { tab: i64 },
    /// Point a tab at `url`. Replies `null`.
    Navigate { tab: i64, url: String },
    /// Send a command to a tab's content script. Fails `not-ready` when the tab has no page script running.
    Command { tab: i64, command: TabCommand },
    /// Wait until a tab's page script reports in, for at most `timeoutMs`. Replies `{ ready, waitedMs }`.
    #[serde(rename_all = "camelCase")]
    AwaitReady { tab: i64, timeout_ms: u64 },
    /// Keep a watch: attach it to every open tab whose URL matches `match` and to each matching tab that loads
    /// later, and deliver every element the tabs report to `deliver` as `{ tab, elementId, url, fields }`. A watch
    /// asked again under the same id replaces the first. Replies `null`.
    #[serde(rename_all = "camelCase")]
    Watch {
        watch_id: String,
        #[serde(rename = "match")]
        match_pattern: String,
        selector: String,
        fields: Vec<TabField>,
        deliver: TabDelivery,
    },
    /// Drop a watch from the host and from every tab. Replies whether it was kept.
    #[serde(rename_all = "camelCase")]
    Unwatch { watch_id: String },
    /// Show a tab in its window and capture what the window shows, as `format` (`quality` 0-100 for jpeg).
    /// Replies the image as a `data:` URL.
    Capture {
        tab: i64,
        format: TabImageFormat,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<u8>,
    },
}

/// Why a host did not perform a [`TabAsk`].
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabFailureKind {
    /// The host has no browser tabs.
    Unavailable,
    /// The tab has no page script running (not loaded yet, or a page outside the extension's access).
    NotReady,
    /// The element is no longer on the page.
    NotFound,
    /// The page cannot take the command (an element that cannot be styled or typed into).
    Invalid,
    Failed,
}

impl TabFailureKind {
    /// The spelling a failure payload carries in its `kind` field.
    pub fn as_str(self) -> &'static str {
        match self {
            TabFailureKind::Unavailable => "unavailable",
            TabFailureKind::NotReady => "not-ready",
            TabFailureKind::NotFound => "not-found",
            TabFailureKind::Invalid => "invalid",
            TabFailureKind::Failed => "failed",
        }
    }
}

/// A host's failure answer to a [`TabAsk`].
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabFailure {
    pub kind: TabFailureKind,
    pub message: String,
}

impl TabFailure {
    pub fn new(kind: TabFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Chrome's match-pattern grammar: `<all_urls>`, or `<scheme>://<host><path>` where scheme is
/// `*|http|https|file|ftp`, host is `*`, `*.<domain>` or a domain, and path starts with `/`.
pub fn is_match_pattern(pattern: &str) -> bool {
    if pattern == "<all_urls>" {
        return true;
    }
    let Some((scheme, rest)) = pattern.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "*" | "http" | "https" | "file" | "ftp") {
        return false;
    }
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let host = &rest[..slash];
    let host_ok = host == "*"
        || (host.starts_with("*.") && host.len() > 2 && !host[2..].contains('*'))
        || (!host.is_empty() && !host.contains('*'));
    host_ok || (scheme == "file" && host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_op_round_trips_through_its_name() {
        for op in TabOp::ALL {
            assert_eq!(TabOp::from_name(op.name()), Some(op));
            assert!(op.name().starts_with("browser/tab-"));
        }
        assert_eq!(TabOp::from_name("browser/tab-nope"), None);
        assert_eq!(TabOp::from_name("browser/page-facts"), None);
    }

    #[test]
    fn match_patterns_follow_chromes_grammar() {
        for good in ["<all_urls>", "https://example.com/*", "*://*.example.org/a/*", "file:///tmp/*", "http://*/*"] {
            assert!(is_match_pattern(good), "{good}");
        }
        for bad in ["example.com", "https://example.com", "ws://example.com/*", "https://*.*/x", "https://a*b.com/*", "https://*./*"] {
            assert!(!is_match_pattern(bad), "{bad}");
        }
    }

    #[test]
    fn a_command_ask_serializes_to_the_wire_shape_the_hosts_read() {
        let ask = TabAsk::Command {
            tab: 4,
            command: TabCommand::Apply(TabApply {
                element_id: "el-1".to_string(),
                styles: vec![TabStyle {
                    property: "display".to_string(),
                    value: "none".to_string(),
                }],
                label: None,
            }),
        };
        assert_eq!(
            serde_json::to_value(&ask).unwrap(),
            json!({
                "kind": "command",
                "tab": 4,
                "command": { "kind": "apply", "elementId": "el-1", "styles": [{ "property": "display", "value": "none" }] }
            })
        );
        let back: TabAsk = serde_json::from_value(serde_json::to_value(&ask).unwrap()).unwrap();
        assert_eq!(back, ask);
    }

    #[test]
    fn a_watch_ask_carries_its_match_and_delivery() {
        let ask = TabAsk::Watch {
            watch_id: "w-1".to_string(),
            match_pattern: "https://example.com/*".to_string(),
            selector: ".item".to_string(),
            fields: vec![TabField {
                name: "title".to_string(),
                selector: Some("h2".to_string()),
                attr: None,
                resolve: false,
                template: None,
                required: true,
            }],
            deliver: TabDelivery {
                orbital: "O".to_string(),
                trait_name: "T".to_string(),
                event: "ITEM_FOUND".to_string(),
            },
        };
        assert_eq!(
            serde_json::to_value(&ask).unwrap(),
            json!({
                "kind": "watch",
                "watchId": "w-1",
                "match": "https://example.com/*",
                "selector": ".item",
                "fields": [{ "name": "title", "selector": "h2", "required": true }],
                "deliver": { "orbital": "O", "trait": "T", "event": "ITEM_FOUND" }
            })
        );
    }

    #[test]
    fn a_capture_ask_carries_its_tab_format_and_quality() {
        let ask = TabAsk::Capture { tab: 3, format: TabImageFormat::Jpeg, quality: Some(80) };
        assert_eq!(serde_json::to_value(&ask).unwrap(), json!({ "kind": "capture", "tab": 3, "format": "jpeg", "quality": 80 }));
        let png = TabAsk::Capture { tab: 3, format: TabImageFormat::Png, quality: None };
        assert_eq!(serde_json::to_value(&png).unwrap(), json!({ "kind": "capture", "tab": 3, "format": "png" }), "no quality is no key");
        assert_eq!(TabImageFormat::Jpeg.data_url_prefix(), "data:image/jpeg;base64,");
    }

    #[test]
    fn control_a_failure_kind_names_itself_as_its_payload_spelling() {
        let failure = TabFailure::new(TabFailureKind::NotReady, "Tab 3 has no page script");
        assert_eq!(serde_json::to_value(&failure).unwrap(), json!({ "kind": "not-ready", "message": "Tab 3 has no page script" }));
        assert_eq!(failure.kind.as_str(), "not-ready");
    }
}
