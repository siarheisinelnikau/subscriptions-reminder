//! Explicit UI messages. User data and technical error details are never translated.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt::Display,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Language {
    Russian = 1,
    French = 2,
    German = 3,
    Spanish = 4,
    Italian = 5,
    #[default]
    #[serde(other)]
    English = 0,
}

impl Language {
    pub const ALL: [Self; 6] = [
        Self::English,
        Self::Russian,
        Self::French,
        Self::German,
        Self::Spanish,
        Self::Italian,
    ];

    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
            Self::French => "Français",
            Self::German => "Deutsch",
            Self::Spanish => "Español",
            Self::Italian => "Italiano",
        }
    }

    pub fn select(self) {
        LANGUAGE.store(self as u8, Ordering::Relaxed);
    }

    pub fn translate(self, message: &str) -> &str {
        if self == Self::English {
            return message;
        }
        catalog()
            .get(message)
            .map_or(message, |row| row[self as usize - 1].as_str())
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(Language::English as u8);
static CATALOG: OnceLock<BTreeMap<String, [String; 5]>> = OnceLock::new();

fn catalog() -> &'static BTreeMap<String, [String; 5]> {
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("locales.json")).expect("validated bundled UI catalog")
    })
}

pub fn current() -> Language {
    Language::ALL
        .get(LANGUAGE.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
}

pub fn tr(message: &str) -> &str {
    current().translate(message)
}

/// Interpolate only explicit arguments, keeping subscription names and other data intact.
pub fn format_message(template: &str, arguments: &[&dyn Display]) -> String {
    let mut output = String::with_capacity(template.len() + 32);
    let mut parts = template.split("{}");
    output.push_str(parts.next().unwrap_or_default());
    for (index, part) in parts.enumerate() {
        if let Some(argument) = arguments.get(index) {
            output.push_str(&argument.to_string());
        } else {
            output.push_str("{}");
        }
        output.push_str(part);
    }
    output
}

#[macro_export]
macro_rules! trf {
    ($message:literal $(, $argument:expr)* $(,)?) => {
        $crate::i18n::format_message($crate::i18n::tr($message), &[$(&$argument as &dyn std::fmt::Display),*])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_translation_preserves_interpolation_slots() {
        for (english, translations) in catalog() {
            for translated in translations {
                assert!(
                    !translated.trim().is_empty(),
                    "missing translation: {english}"
                );
                assert_eq!(
                    english.matches("{}").count(),
                    translated.matches("{}").count(),
                    "placeholder mismatch: {english}"
                );
            }
        }
    }

    #[test]
    fn user_data_and_unknown_messages_are_preserved() {
        let name = "Settings / subscriptions {example}";
        assert_eq!(Language::Russian.translate(name), name);
        assert_eq!(
            format_message(
                Language::German.translate("Version {} is available."),
                &[&"0.6.0"]
            ),
            "Version 0.6.0 ist verfügbar."
        );
    }

    #[test]
    fn older_settings_keep_their_theme_and_default_to_english() {
        let settings: crate::storage::Settings =
            serde_json::from_str(r#"{"theme":"light","startup_update_check":false}"#).unwrap();
        assert_eq!(settings.language, Language::English);
        assert_eq!(settings.theme, crate::theme::Preference::Light);
        assert!(!settings.startup_update_check);
        for language in Language::ALL {
            let restored: Language =
                serde_json::from_str(&serde_json::to_string(&language).unwrap()).unwrap();
            assert_eq!(restored, language);
        }
        assert_eq!(
            serde_json::from_str::<Language>("\"unknown\"").unwrap(),
            Language::English
        );
    }
}
