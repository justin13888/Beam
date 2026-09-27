//! A title's pinned provider id (issue #184).
//!
//! A Kodi `.nfo` beside the media names the title's provider ids
//! (`<uniqueid type="tmdb">603</uniqueid>`). Beam stores the one it trusts
//! most as the title's **pin**: enrichment fetches a pinned title by that id
//! rather than searching for it by name, and a second file naming the same id
//! joins the same title whatever its path says.

/// A provider id a title is pinned to. Stored as the same `"provider:id"`
/// text a match is recorded in ([`crate::providers::enrichment::ExternalMediaRef`]),
/// so a pin a configured provider can resolve is its own lookup key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProviderPin {
    /// A TMDB id -- of a movie or of a show, by the title it pins.
    Tmdb(u32),
    /// An AniList media id.
    Anilist(u32),
    /// An IMDb title id, `tt` and seven or more digits.
    Imdb(String),
    /// A TheTVDB series id.
    Tvdb(u32),
}

impl ProviderPin {
    /// The provider's name, as a stored match and `available_providers` spell it.
    pub fn provider(&self) -> &'static str {
        match self {
            ProviderPin::Tmdb(_) => "tmdb",
            ProviderPin::Anilist(_) => "anilist",
            ProviderPin::Imdb(_) => "imdb",
            ProviderPin::Tvdb(_) => "tvdb",
        }
    }

    /// The stored `"provider:id"` form.
    pub fn to_ref_string(&self) -> String {
        match self {
            ProviderPin::Tmdb(id) | ProviderPin::Anilist(id) | ProviderPin::Tvdb(id) => {
                format!("{}:{id}", self.provider())
            }
            ProviderPin::Imdb(id) => format!("imdb:{id}"),
        }
    }

    /// Reads a stored `"provider:id"` back. `None` for a provider Beam does not
    /// pin by, or an id that provider never issues.
    pub fn parse(stored: &str) -> Option<Self> {
        let (provider, native) = stored.split_once(':')?;
        match provider {
            "tmdb" => positive(native).map(ProviderPin::Tmdb),
            "anilist" => positive(native).map(ProviderPin::Anilist),
            "tvdb" => positive(native).map(ProviderPin::Tvdb),
            "imdb" => is_imdb_id(native).then(|| ProviderPin::Imdb(native.to_string())),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderPin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_ref_string())
    }
}

/// A provider's numeric id: all digits, and not zero (no provider issues `0`).
pub(crate) fn positive(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|id| *id > 0)
}

/// Whether `text` is an IMDb title id: `tt` and seven to ten digits.
pub(crate) fn is_imdb_id(text: &str) -> bool {
    text.strip_prefix("tt").is_some_and(|digits| {
        (7..=10).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_stored_pin_reads_back_as_itself() {
        for pin in [
            ProviderPin::Tmdb(603),
            ProviderPin::Anilist(5114),
            ProviderPin::Imdb("tt0133093".to_string()),
            ProviderPin::Tvdb(81189),
        ] {
            assert_eq!(ProviderPin::parse(&pin.to_ref_string()), Some(pin));
        }
    }

    #[test]
    fn what_no_provider_issues_is_not_a_pin() {
        for stored in [
            "tmdb:0",
            "tmdb:",
            "tmdb:-3",
            "tmdb:6a",
            "imdb:0133093",
            "imdb:tt123",
            "imdb:tt01330931234",
            "trakt:12",
            "603",
        ] {
            assert_eq!(ProviderPin::parse(stored), None, "{stored:?}");
        }
    }

    proptest! {
        #[test]
        fn parsing_never_panics_and_a_parsed_pin_round_trips(stored in "\\PC{0,24}") {
            if let Some(pin) = ProviderPin::parse(&stored) {
                prop_assert_eq!(ProviderPin::parse(&pin.to_ref_string()), Some(pin));
            }
        }
    }
}
