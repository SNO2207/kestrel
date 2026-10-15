//! Shortcut text: what the settings dialog shows and stores, and what Windows registers.
//!
//! Shortcuts are written `Ctrl+Alt+Space`: modifiers in a fixed order (Ctrl, Alt, Shift, Win)
//! and then one key, named as on the keyboard (`K`, `5`, `F13`, `Space`, `Up`). Parsing also
//! accepts the browser's key codes (`KeyK`, `Digit5`, `ArrowUp`), which is what the dialog
//! records, and other spellings of the modifiers (`Control`, `Super`).

use std::str::FromStr;

use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// What the quick launcher answers to unless the user picks something else.
pub const DEFAULT: &str = "Ctrl+Alt+Space";

/// Parses a shortcut, refusing ones that would swallow ordinary typing: a letter, digit or
/// symbol needs Ctrl, Alt or Win with it. Function keys may stand alone.
pub fn parse(text: &str) -> Result<Shortcut, String> {
    let cleaned: String = text.split('+').map(|part| alias(part.trim())).collect::<Vec<_>>().join("+");
    let shortcut = Shortcut::from_str(&cleaned).map_err(|_| format!("“{}” isn't a shortcut Kestrel can use.", text.trim()))?;
    let held = shortcut.mods & (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER | Modifiers::META);
    if held.is_empty() && !is_function_key(shortcut.key) {
        return Err(format!("{} needs Ctrl, Alt or Win with it, or it would take over typing.", display(&shortcut)));
    }
    Ok(shortcut)
}

/// Spellings the dialog or a person might use that the parser does not know.
fn alias(part: &str) -> String {
    match part.to_ascii_lowercase().as_str() {
        "win" | "windows" | "meta" => "Super".into(),
        "esc" => "Escape".into(),
        "del" => "Delete".into(),
        "ins" => "Insert".into(),
        "pgup" => "PageUp".into(),
        "pgdn" => "PageDown".into(),
        _ => part.into(),
    }
}

fn is_function_key(code: Code) -> bool {
    let name = code.to_string();
    name.len() >= 2 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit())
}

/// The shortcut as people read it: `Ctrl+Alt+Shift+Win+K`.
pub fn display(shortcut: &Shortcut) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (flag, name) in [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER | Modifiers::META, "Win"),
    ] {
        if shortcut.mods.intersects(flag) {
            parts.push(name.into());
        }
    }
    parts.push(key_name(shortcut.key));
    parts.join("+")
}

fn key_name(code: Code) -> String {
    let name = code.to_string();
    if let Some(letter) = name.strip_prefix("Key") {
        return letter.into();
    }
    if let Some(digit) = name.strip_prefix("Digit") {
        return digit.into();
    }
    if let Some(arrow) = name.strip_prefix("Arrow") {
        return arrow.into();
    }
    name
}

/// Re-writes stored or typed text in the canonical form, or `None` if it does not parse.
pub fn normalize(text: &str) -> Option<String> {
    parse(text).ok().map(|shortcut| display(&shortcut))
}

/// Whether a registration error means some other program holds the keys.
pub fn taken_elsewhere(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("already registered") || error.contains("1409")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_parse_from_either_spelling_and_display_one_way() {
        assert_eq!(normalize("Ctrl+Alt+Space").as_deref(), Some("Ctrl+Alt+Space"));
        assert_eq!(normalize("alt + control + space").as_deref(), Some("Ctrl+Alt+Space"));
        assert_eq!(normalize("Shift+Control+KeyK").as_deref(), Some("Ctrl+Shift+K"));
        assert_eq!(normalize("Win+Digit5").as_deref(), Some("Win+5"));
        assert_eq!(normalize("Ctrl+ArrowUp").as_deref(), Some("Ctrl+Up"));
        assert_eq!(normalize("Ctrl+Alt+Shift+F16").as_deref(), Some("Ctrl+Alt+Shift+F16"));
        assert_eq!(normalize("F13").as_deref(), Some("F13"), "a function key may stand alone");
        let parsed = parse("Ctrl+Alt+Space").unwrap();
        assert_eq!(parsed, parse("ctrl+alt+SPACE").unwrap(), "the same keys are the same shortcut");
    }

    #[test]
    fn shortcuts_that_would_take_over_typing_or_do_not_parse_are_refused() {
        assert!(parse("K").unwrap_err().contains("needs Ctrl, Alt or Win"));
        assert!(parse("Shift+K").is_err());
        assert!(parse("Ctrl+K+L").is_err());
        assert!(parse("").is_err());
        assert!(parse("Ctrl+Nonsense").unwrap_err().contains("isn't a shortcut"));
    }

    #[test]
    fn errors_naming_a_taken_shortcut_are_recognised() {
        assert!(taken_elsewhere("HotKey already registered: HotKey { mods: ... }"));
        assert!(taken_elsewhere("Hot key is already registered. (os error 1409)"));
        assert!(!taken_elsewhere("Unable to register hotkey: Unknown VKCode for Fn"));
    }
}
