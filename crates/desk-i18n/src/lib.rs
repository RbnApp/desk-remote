//! Localization and translation engine for Desk-Remote.
//!
//! Embeds the canonical English catalog (`locales/en.json`) and Turkish catalog (`locales/tr.json`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};

const EN_JSON: &str = include_str!("../../../locales/en.json");
const TR_JSON: &str = include_str!("../../../locales/tr.json");

/// Supported UI locales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Locale {
    #[default]
    En,
    Tr,
}

impl Locale {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Tr => "tr",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("tr") {
            Self::Tr
        } else {
            Self::En
        }
    }
}

static ACTIVE_LOCALE: AtomicU8 = AtomicU8::new(0); // 0 = En, 1 = Tr

/// Sets the global runtime locale.
pub fn set_locale(locale: Locale) {
    let val = match locale {
        Locale::En => 0,
        Locale::Tr => 1,
    };
    ACTIVE_LOCALE.store(val, Ordering::Relaxed);
}

/// Gets the current global runtime locale.
pub fn current_locale() -> Locale {
    match ACTIVE_LOCALE.load(Ordering::Relaxed) {
        1 => Locale::Tr,
        _ => Locale::En,
    }
}

fn lookup_nested(root: &Value, path: &str) -> Option<String> {
    let mut current = root;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    match current {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Translates a key using the specified locale, falling back to English if missing.
pub fn translate(key: &str, locale: Locale) -> String {
    let target_json = match locale {
        Locale::Tr => TR_JSON,
        Locale::En => EN_JSON,
    };

    if let Ok(val) = serde_json::from_str::<Value>(target_json) {
        if let Some(res) = lookup_nested(&val, key) {
            return res;
        }
    }

    // Fallback to English if target was Turkish and lookup failed
    if locale != Locale::En {
        if let Ok(en_val) = serde_json::from_str::<Value>(EN_JSON) {
            if let Some(res) = lookup_nested(&en_val, key) {
                return res;
            }
        }
    }

    key.to_string()
}

/// Translates a key using the active global locale.
pub fn t(key: &str) -> String {
    translate(key, current_locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translations_and_fallback() {
        assert_eq!(translate("status.online", Locale::En), "Online");
        assert_eq!(translate("status.online", Locale::Tr), "Çevrimiçi");

        assert_eq!(translate("app.name", Locale::En), "Desk-Remote");
        assert_eq!(translate("app.name", Locale::Tr), "Desk-Remote");

        // Missing key returns the key itself
        assert_eq!(translate("nonexistent.key", Locale::En), "nonexistent.key");
        assert_eq!(translate("nonexistent.key", Locale::Tr), "nonexistent.key");
    }

    #[test]
    fn test_locale_switching() {
        set_locale(Locale::Tr);
        assert_eq!(current_locale(), Locale::Tr);
        assert_eq!(t("status.offline"), "Çevrimdışı");

        set_locale(Locale::En);
        assert_eq!(current_locale(), Locale::En);
        assert_eq!(t("status.offline"), "Offline");
    }
}
