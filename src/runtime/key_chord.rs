//! Key-chord matching: a declared keyMap entry against a host key event.
//!
//! Two spellings coexist, one per JS twin:
//! - key-based (`useKeyboardRouter.keyChord`): `Control+Alt+Shift+Meta+<event.key>`,
//!   used by the editor capture table;
//! - code-based (`keyMapEvent.keyMapCode`): `Mod+Shift+Alt+<event.code>`, used by
//!   the board `keyMap`, where a bare entry still fires under undeclared modifiers.
//!
//! Whether the event's DOM target is editable is the host's call
//! ([`is_editable_element`] decides it from the element's facts).

use std::collections::{BTreeMap, BTreeSet};

/// The modifier keys held during a key event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

/// Key-based spelling: modifiers in the fixed order Control, Alt, Shift, Meta, then the key.
pub fn key_chord(key: &str, mods: Modifiers) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(5);
    if mods.ctrl {
        parts.push("Control");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    if mods.meta {
        parts.push("Meta");
    }
    parts.push(key);
    parts.join("+")
}

/// Code-based spelling: `Mod+` (meta or ctrl) before `Shift+` before `Alt+`, then the code.
pub fn key_map_code(code: &str, mods: Modifiers) -> String {
    let mut out = String::new();
    if mods.meta || mods.ctrl {
        out.push_str("Mod+");
    }
    if mods.shift {
        out.push_str("Shift+");
    }
    if mods.alt {
        out.push_str("Alt+");
    }
    out.push_str(code);
    out
}

/// A modified entry wins over the bare code; a bare entry still fires under
/// modifiers it never declared; keystrokes in editable elements never route.
pub fn resolve_key_map_event<'a>(
    map: Option<&'a BTreeMap<String, String>>,
    code: &str,
    mods: Modifiers,
    target_editable: bool,
) -> Option<&'a str> {
    let map = map?;
    if target_editable {
        return None;
    }
    map.get(&key_map_code(code, mods))
        .or_else(|| map.get(code))
        .map(String::as_str)
}

/// Whether an element swallows keystrokes: an input, textarea or select, or a
/// `contenteditable` attribute of `""`, `"true"` or `"plaintext-only"`. The tag
/// is the DOM's upper-case `tagName`, compared exactly as the JS twin does.
pub fn is_editable_element(tag_name: &str, contenteditable_attr: Option<&str>) -> bool {
    matches!(tag_name, "INPUT" | "TEXTAREA" | "SELECT")
        || matches!(contenteditable_attr, Some("" | "true" | "plaintext-only"))
}

/// One capture-table entry: the trait mode it belongs to and the keys it captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCaptureEntry {
    pub mode: String,
    pub keys: BTreeSet<String>,
}

/// Capture table keyed by editor id, or `"shell"` for chrome-level bindings.
pub type KeyCaptureTable = BTreeMap<String, KeyCaptureEntry>;

pub const SHELL_TARGET: &str = "shell";

/// Merge several tables: keys union per target; mode is `any` if any input says
/// so, else the shared mode when all agree, else the distinct modes sorted and
/// `|`-joined.
pub fn merge_capture_tables(tables: &[KeyCaptureTable]) -> KeyCaptureTable {
    let mut merged = KeyCaptureTable::new();
    for table in tables {
        for (target, entry) in table {
            match merged.get_mut(target) {
                None => {
                    merged.insert(target.clone(), entry.clone());
                }
                Some(current) => {
                    current.keys.extend(entry.keys.iter().cloned());
                    if current.mode == "any" || entry.mode == "any" {
                        current.mode = "any".to_string();
                    } else if current.mode != entry.mode {
                        let modes: BTreeSet<&str> = current
                            .mode
                            .split('|')
                            .chain(std::iter::once(entry.mode.as_str()))
                            .collect();
                        current.mode = modes.into_iter().collect::<Vec<_>>().join("|");
                    }
                }
            }
        }
    }
    merged
}

/// What the router does with one keydown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRoute {
    /// The capture target: the focused editor id, else `"shell"`.
    pub editor_id: String,
    /// True when the target's entry lists the bare key or its chord.
    pub captured: bool,
}

/// Route one keydown. `None` while an IME composition is in progress (no emit).
pub fn route_key(
    table: &KeyCaptureTable,
    focused_editor: Option<&str>,
    key: &str,
    mods: Modifiers,
    is_composing: bool,
) -> Option<KeyRoute> {
    if is_composing {
        return None;
    }
    let target = focused_editor.unwrap_or(SHELL_TARGET);
    let captured = table
        .get(target)
        .is_some_and(|e| e.keys.contains(key) || e.keys.contains(&key_chord(key, mods)));
    Some(KeyRoute {
        editor_id: target.to_string(),
        captured,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
    };
    const META: Modifiers = Modifiers { meta: true, ..NONE };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn entry(mode: &str, keys: &[&str]) -> KeyCaptureEntry {
        KeyCaptureEntry {
            mode: mode.into(),
            keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn table(rows: &[(&str, KeyCaptureEntry)]) -> KeyCaptureTable {
        rows.iter()
            .map(|(k, e)| (k.to_string(), e.clone()))
            .collect()
    }

    // keyMapEvent.test.ts: isEditableTarget
    #[test]
    fn editable_elements() {
        for tag in ["INPUT", "TEXTAREA", "SELECT"] {
            assert!(is_editable_element(tag, None), "{tag}");
        }
        assert!(is_editable_element("DIV", Some("true")));
        assert!(is_editable_element("DIV", Some("")));
        assert!(is_editable_element("DIV", Some("plaintext-only")));
        assert!(!is_editable_element("DIV", Some("false")));
        assert!(!is_editable_element("CANVAS", None));
    }

    // keyMapEvent.test.ts: keyMapCode
    #[test]
    fn key_map_code_spellings() {
        assert_eq!(key_map_code("KeyZ", NONE), "KeyZ");
        assert_eq!(key_map_code("KeyZ", META), "Mod+KeyZ");
        assert_eq!(key_map_code("KeyZ", CTRL), "Mod+KeyZ");
        assert_eq!(
            key_map_code(
                "KeyZ",
                Modifiers {
                    meta: true,
                    shift: true,
                    alt: true,
                    ctrl: false
                }
            ),
            "Mod+Shift+Alt+KeyZ"
        );
        // control: meta and ctrl together are still one Mod+
        assert_eq!(
            key_map_code(
                "KeyZ",
                Modifiers {
                    meta: true,
                    ctrl: true,
                    ..NONE
                }
            ),
            "Mod+KeyZ"
        );
    }

    // keyMapEvent.test.ts: resolveKeyMapEvent
    #[test]
    fn resolve_returns_none_without_map_or_in_editable_target() {
        assert_eq!(resolve_key_map_event(None, "KeyZ", NONE, false), None);
        let m = map(&[("KeyZ", "UNDO")]);
        assert_eq!(resolve_key_map_event(Some(&m), "KeyZ", NONE, true), None);
        // control: same map, non-editable target fires
        assert_eq!(
            resolve_key_map_event(Some(&m), "KeyZ", NONE, false),
            Some("UNDO")
        );
    }

    #[test]
    fn resolve_mod_entries() {
        let m = map(&[("Mod+KeyZ", "UNDO")]);
        assert_eq!(
            resolve_key_map_event(Some(&m), "KeyZ", META, false),
            Some("UNDO")
        );
        assert_eq!(
            resolve_key_map_event(Some(&m), "KeyZ", CTRL, false),
            Some("UNDO")
        );
        let redo = map(&[("Mod+Shift+KeyZ", "REDO")]);
        assert_eq!(
            resolve_key_map_event(
                Some(&redo),
                "KeyZ",
                Modifiers {
                    meta: true,
                    shift: true,
                    ..NONE
                },
                false
            ),
            Some("REDO")
        );
    }

    #[test]
    fn resolve_bare_fallback_and_precedence() {
        let bare = map(&[("KeyZ", "OTHER")]);
        assert_eq!(
            resolve_key_map_event(Some(&bare), "KeyZ", SHIFT, false),
            Some("OTHER")
        );
        let both = map(&[("Mod+KeyZ", "UNDO"), ("KeyZ", "OTHER")]);
        assert_eq!(
            resolve_key_map_event(Some(&both), "KeyZ", META, false),
            Some("UNDO")
        );
        assert_eq!(
            resolve_key_map_event(Some(&both), "KeyZ", NONE, false),
            Some("OTHER")
        );
        let miss = map(&[("KeyA", "MOVE")]);
        assert_eq!(
            resolve_key_map_event(Some(&miss), "KeyZ", NONE, false),
            None
        );
    }

    // useKeyboardRouter.test.tsx: keyChord
    #[test]
    fn key_chord_fixed_modifier_order() {
        assert_eq!(
            key_chord(
                "k",
                Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: true,
                    meta: true
                }
            ),
            "Control+Shift+Meta+k"
        );
        assert_eq!(key_chord("Escape", NONE), "Escape");
        assert_eq!(
            key_chord(
                "x",
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    meta: true
                }
            ),
            "Control+Alt+Shift+Meta+x"
        );
    }

    // useKeyboardRouter.test.tsx: capture rule
    #[test]
    fn captures_a_declared_key_for_the_focused_editor() {
        let t = table(&[("e1", entry("NORMAL", &["h"]))]);
        let r = route_key(&t, Some("e1"), "h", NONE, false).unwrap();
        assert_eq!(
            r,
            KeyRoute {
                editor_id: "e1".into(),
                captured: true
            }
        );
    }

    #[test]
    fn undeclared_key_is_routed_but_not_captured() {
        let t = table(&[("e1", entry("NORMAL", &["h"]))]);
        let r = route_key(&t, Some("e1"), "a", NONE, false).unwrap();
        assert_eq!(
            r,
            KeyRoute {
                editor_id: "e1".into(),
                captured: false
            }
        );
    }

    #[test]
    fn shell_target_when_unfocused_and_mode_any_without_keys_captures_nothing() {
        let t = table(&[("shell", entry("any", &[]))]);
        let r = route_key(&t, None, "z", NONE, false).unwrap();
        assert_eq!(
            r,
            KeyRoute {
                editor_id: "shell".into(),
                captured: false
            }
        );
    }

    #[test]
    fn chord_entries_capture_only_when_modifiers_match() {
        let t = table(&[("shell", entry("any", &["Meta+k", "Control+Shift+P"]))]);
        assert!(!route_key(&t, None, "k", NONE, false).unwrap().captured);
        assert!(route_key(&t, None, "k", META, false).unwrap().captured);
        assert!(!route_key(&t, None, "P", SHIFT, false).unwrap().captured);
        assert!(
            route_key(
                &t,
                None,
                "P",
                Modifiers {
                    ctrl: true,
                    shift: true,
                    ..NONE
                },
                false
            )
            .unwrap()
            .captured
        );
    }

    #[test]
    fn empty_table_captures_nothing() {
        let r = route_key(&KeyCaptureTable::new(), None, "z", NONE, false).unwrap();
        assert!(!r.captured);
    }

    #[test]
    fn falls_back_to_shell_after_blur() {
        let t = table(&[
            ("e1", entry("NORMAL", &["h"])),
            ("shell", entry("any", &[])),
        ]);
        // the host clears focus on a matching blur; the route then targets the shell
        let r = route_key(&t, None, "x", NONE, false).unwrap();
        assert_eq!(r.editor_id, "shell");
    }

    #[test]
    fn composition_keydowns_are_ignored() {
        let t = table(&[("shell", entry("any", &["a"]))]);
        assert_eq!(route_key(&t, None, "a", NONE, true), None);
        // control: the same key outside composition routes
        assert!(route_key(&t, None, "a", NONE, false).is_some());
    }

    // useKeyboardRouter.test.tsx: mergeCaptureTables
    #[test]
    fn merge_unions_keys_and_keeps_shared_mode() {
        let m = merge_capture_tables(&[
            table(&[("editor", entry("NORMAL", &["h", "j"]))]),
            table(&[("editor", entry("NORMAL", &["j", "k"]))]),
        ]);
        assert_eq!(m["editor"].mode, "NORMAL");
        assert_eq!(
            m["editor"].keys.iter().cloned().collect::<Vec<_>>(),
            vec!["h", "j", "k"]
        );
        let three = merge_capture_tables(&[
            table(&[("editor", entry("NORMAL", &["h"]))]),
            table(&[("editor", entry("NORMAL", &["j"]))]),
            table(&[("editor", entry("NORMAL", &["k"]))]),
        ]);
        assert_eq!(three["editor"].mode, "NORMAL");
    }

    #[test]
    fn merge_any_wins_and_distinct_modes_join_sorted() {
        let any = merge_capture_tables(&[
            table(&[("editor", entry("NORMAL", &["h"]))]),
            table(&[("editor", entry("any", &[]))]),
        ]);
        assert_eq!(any["editor"].mode, "any");
        let any_first = merge_capture_tables(&[
            table(&[("editor", entry("any", &[]))]),
            table(&[("editor", entry("NORMAL", &["h"]))]),
        ]);
        assert_eq!(any_first["editor"].mode, "any");
        let joined = merge_capture_tables(&[
            table(&[("editor", entry("VISUAL", &["v"]))]),
            table(&[("editor", entry("NORMAL", &["n"]))]),
            table(&[("editor", entry("INSERT", &["i"]))]),
        ]);
        assert_eq!(joined["editor"].mode, "INSERT|NORMAL|VISUAL");
    }

    #[test]
    fn merge_empty_and_single_table_targets() {
        assert!(merge_capture_tables(&[]).is_empty());
        let m = merge_capture_tables(&[
            table(&[("editor", entry("NORMAL", &["h"]))]),
            table(&[("shell", entry("any", &[]))]),
        ]);
        assert_eq!(m["editor"], entry("NORMAL", &["h"]));
        assert_eq!(m["shell"], entry("any", &[]));
    }
}
