//! Kodi-style `.nfo` files: what a library already says about its titles
//! (issue #184).
//!
//! Media managers (Kodi, tinyMediaManager, Jellyfin, Emby) write an NFO beside
//! the media: `movie.nfo` or `<file>.nfo` for a film, `tvshow.nfo` in a series
//! folder, `<file>.nfo` for an episode. It is XML -- `<movie>`, `<tvshow>`,
//! one `<episodedetails>` per episode of a multi-episode file -- or just a
//! provider URL, or XML followed by one. What it says is read here, purely,
//! from its bytes: the indexer finds and reads the file (read-only, at most
//! [`MAX_NFO_BYTES`]).
//!
//! The parser is defensive rather than strict. A document type declaration is
//! refused outright (no entity is ever expanded), the tree is capped at
//! [`MAX_NFO_NODES`] nodes, and bytes that are not UTF-8 are an error rather
//! than a lossy guess. Anything else it does not understand is ignored.

use std::sync::LazyLock;

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use roxmltree::{Document, Node, ParsingOptions};
use thiserror::Error;

use crate::models::pin::{ProviderPin, is_imdb_id, positive};

/// The largest NFO the indexer reads. A real one is a few kilobytes; anything
/// past this is not an NFO worth trusting, and reading it would let a file in
/// a library root decide how much memory a scan takes.
pub const MAX_NFO_BYTES: u64 = 1024 * 1024;

/// The most XML nodes an NFO may parse into. A generous Kodi movie NFO with a
/// full cast list is a few hundred.
pub const MAX_NFO_NODES: u32 = 10_000;

/// Why an NFO could not be read.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NfoError {
    #[error("the NFO is not UTF-8")]
    NotUtf8,
    #[error("the NFO declares a document type, which is never expanded")]
    Doctype,
    #[error("the NFO is not well-formed XML: {0}")]
    Malformed(String),
}

/// What the NFO's root element says it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NfoKind {
    /// `<movie>`.
    Movie,
    /// `<tvshow>`: a series, not a file.
    TvShow,
    /// One or more `<episodedetails>`: the episodes one file holds.
    Episodes,
}

/// The provider ids an NFO names, each read from the most trusted place that
/// names it: a `<uniqueid>` (the `default="true"` one first), then a legacy
/// element (`<id>`, `<tmdbid>`, ...), then a provider URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderIds {
    pub tmdb: Option<u32>,
    pub imdb: Option<String>,
    pub tvdb: Option<u32>,
    pub anilist: Option<u32>,
}

impl ProviderIds {
    /// The id a title is pinned to: TMDB, then AniList -- the two providers
    /// Beam's enrichment resolves an id with -- then IMDb, then TheTVDB, which
    /// it can only check a match against (decision D184-2).
    pub fn pin(&self) -> Option<ProviderPin> {
        let ProviderIds {
            tmdb,
            imdb,
            tvdb,
            anilist,
        } = self;
        tmdb.map(ProviderPin::Tmdb)
            .or(anilist.map(ProviderPin::Anilist))
            .or(imdb.clone().map(ProviderPin::Imdb))
            .or(tvdb.map(ProviderPin::Tvdb))
    }

    /// Fill each id `self` lacks from `other`.
    fn fill_from(&mut self, other: ProviderIds) {
        let ProviderIds {
            tmdb,
            imdb,
            tvdb,
            anilist,
        } = other;
        self.tmdb = self.tmdb.or(tmdb);
        self.imdb = self.imdb.take().or(imdb);
        self.tvdb = self.tvdb.or(tvdb);
        self.anilist = self.anilist.or(anilist);
    }
}

/// One `<episodedetails>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NfoEpisode {
    pub title: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub aired: Option<NaiveDate>,
}

/// What an NFO says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nfo {
    /// What the root element describes. `None` for an NFO that is only a
    /// provider URL, unless the URL itself says (a TMDB `/movie/` or `/tv/`
    /// URL): such an NFO describes whatever the file it sits beside is.
    pub kind: Option<NfoKind>,
    /// `<title>`: of the movie or show, or of the first episode.
    pub title: Option<String>,
    pub original_title: Option<String>,
    /// `<year>`, else the year of `<premiered>` or `<aired>`.
    pub year: Option<u32>,
    /// `<edition>` (`Director's Cut`).
    pub edition: Option<String>,
    /// An episode NFO's `<showtitle>`.
    pub show_title: Option<String>,
    /// Every `<episodedetails>`, in file order.
    pub episodes: Vec<NfoEpisode>,
    /// The provider ids of the movie or show. An episode NFO's ids name the
    /// episode, never its show, so they are not read.
    pub ids: ProviderIds,
}

/// Read an NFO from its bytes.
pub fn parse_nfo(bytes: &[u8]) -> Result<Nfo, NfoError> {
    let text = std::str::from_utf8(bytes).map_err(|_| NfoError::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).trim();
    if contains_ignore_ascii_case(text, "<!doctype") || contains_ignore_ascii_case(text, "<!entity")
    {
        return Err(NfoError::Doctype);
    }
    let body = strip_xml_declaration(text);
    if !body.starts_with('<') {
        // Only a URL (or prose around one): Kodi's "URL NFO".
        return Ok(url_only(body));
    }
    // XML, possibly followed by a URL on its own line -- which a lone `&` in
    // its query string would make malformed XML. Only up to the last `>` is
    // parsed; what follows is read for URLs. Wrapping the XML in one element
    // lets a multi-episode NFO's several `<episodedetails>` roots parse.
    let end = body.rfind('>').map_or(body.len(), |i| i + 1);
    let (xml, trailing) = body.split_at(end);
    let wrapped = format!("<nfo>{xml}</nfo>");
    let doc = Document::parse_with_options(
        &wrapped,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NFO_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|err| NfoError::Malformed(err.to_string()))?;
    Ok(from_document(&doc, trailing))
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// `text` without a leading `<?xml ...?>` declaration.
fn strip_xml_declaration(text: &str) -> &str {
    match text.strip_prefix("<?xml") {
        Some(rest) => rest
            .find("?>")
            .map_or(text, |end| rest[end + 2..].trim_start()),
        None => text,
    }
}

fn url_only(text: &str) -> Nfo {
    let (ids, kind) = ids_in_urls(text);
    Nfo {
        kind,
        ids,
        ..Nfo::default()
    }
}

fn from_document(doc: &Document<'_>, trailing: &str) -> Nfo {
    let wrapper = doc.root_element();
    let roots: Vec<Node<'_, '_>> = wrapper.children().filter(Node::is_element).collect();
    // Text between or around the root elements, and after the last: where an
    // XML+URL NFO puts its URL.
    let loose_text: String = wrapper
        .children()
        .filter(Node::is_text)
        .filter_map(|n| n.text())
        .chain(std::iter::once(trailing))
        .collect::<Vec<_>>()
        .join("\n");
    let (url_ids, url_kind) = ids_in_urls(&loose_text);

    let kind = roots.iter().find_map(|root| match root.tag_name().name() {
        "movie" => Some(NfoKind::Movie),
        "tvshow" => Some(NfoKind::TvShow),
        "episodedetails" => Some(NfoKind::Episodes),
        _ => None,
    });
    let Some(kind) = kind else {
        return Nfo {
            kind: url_kind,
            ids: url_ids,
            ..Nfo::default()
        };
    };

    let mut nfo = Nfo {
        kind: Some(kind),
        ..Nfo::default()
    };
    match kind {
        NfoKind::Movie | NfoKind::TvShow => {
            let name = if kind == NfoKind::Movie {
                "movie"
            } else {
                "tvshow"
            };
            let root = roots
                .iter()
                .find(|r| r.tag_name().name() == name)
                .expect("the kind was read from this root");
            nfo.title = child_text(root, "title");
            nfo.original_title = child_text(root, "originaltitle");
            nfo.year = year_of(root);
            nfo.edition = child_text(root, "edition");
            nfo.ids = element_ids(root, kind);
            nfo.ids.fill_from(url_ids);
        }
        NfoKind::Episodes => {
            for root in roots
                .iter()
                .filter(|r| r.tag_name().name() == "episodedetails")
            {
                let episode = NfoEpisode {
                    title: child_text(root, "title"),
                    season: child_text(root, "season").as_deref().and_then(number),
                    episode: child_text(root, "episode").as_deref().and_then(number),
                    aired: child_text(root, "aired").as_deref().and_then(date),
                };
                if nfo.show_title.is_none() {
                    nfo.show_title = child_text(root, "showtitle");
                }
                // No `year`: an episode's air year is not its show's.
                nfo.episodes.push(episode);
            }
            nfo.title = nfo.episodes.first().and_then(|e| e.title.clone());
        }
    }
    nfo
}

/// The trimmed, non-empty text of `node`'s first child element named `name`.
fn child_text(node: &Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// A season or episode number: all digits.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// The earliest and latest release years Beam believes an NFO about.
const YEARS: std::ops::RangeInclusive<u32> = 1870..=2100;

fn year_of(node: &Node<'_, '_>) -> Option<u32> {
    child_text(node, "year")
        .as_deref()
        .and_then(number)
        .or_else(|| {
            ["premiered", "aired"].iter().find_map(|name| {
                child_text(node, name)
                    .as_deref()
                    .and_then(date)
                    .map(|d| d.year().unsigned_abs())
            })
        })
        .filter(|year| YEARS.contains(year))
}

/// The ids a `<movie>` or `<tvshow>` element names.
fn element_ids(root: &Node<'_, '_>, kind: NfoKind) -> ProviderIds {
    let mut default_ids = ProviderIds::default();
    let mut other_ids = ProviderIds::default();
    for unique in root
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "uniqueid")
    {
        let Some(value) = unique.text().map(str::trim) else {
            continue;
        };
        let target = if unique.attribute("default") == Some("true") {
            &mut default_ids
        } else {
            &mut other_ids
        };
        let provider = unique.attribute("type").unwrap_or("").to_ascii_lowercase();
        set_id(target, &provider, value);
    }

    // Legacy elements, written before `<uniqueid>` existed. A bare `<id>` is
    // an IMDb id when it looks like one; otherwise it is the id of the
    // scraper that wrote it -- TMDB's for a movie, TheTVDB's for a show.
    let mut legacy = ProviderIds::default();
    if let Some(id) = child_text(root, "id") {
        let provider = if is_imdb_id(&id) {
            "imdb"
        } else if kind == NfoKind::Movie {
            "tmdb"
        } else {
            "tvdb"
        };
        set_id(&mut legacy, provider, &id);
    }
    for (element, provider) in [
        ("tmdbid", "tmdb"),
        ("imdbid", "imdb"),
        ("tvdbid", "tvdb"),
        ("anilistid", "anilist"),
    ] {
        if let Some(id) = child_text(root, element) {
            set_id(&mut legacy, provider, &id);
        }
    }

    let mut ids = default_ids;
    ids.fill_from(other_ids);
    ids.fill_from(legacy);
    ids
}

/// Record `value` as `provider`'s id in `ids`, unless it already has one or
/// the provider never issues such an id.
fn set_id(ids: &mut ProviderIds, provider: &str, value: &str) {
    match provider {
        "tmdb" => ids.tmdb = ids.tmdb.or(positive(value)),
        "tvdb" => ids.tvdb = ids.tvdb.or(positive(value)),
        "anilist" => ids.anilist = ids.anilist.or(positive(value)),
        "imdb" if ids.imdb.is_none() && is_imdb_id(value) => ids.imdb = Some(value.to_string()),
        _ => {}
    }
}

static TMDB_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)themoviedb\.org/(movie|tv)/(\d+)").expect("valid regex"));
static IMDB_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)imdb\.com/title/(tt\d{7,10})\b").expect("valid regex"));
static TVDB_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)thetvdb\.com/(?:\S*?[?&]id=|dereferrer/series/)(\d+)").expect("valid regex")
});
static ANILIST_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)anilist\.co/anime/(\d+)").expect("valid regex"));

/// The ids the provider URLs in `text` name, and what kind of title a TMDB
/// URL says they are.
fn ids_in_urls(text: &str) -> (ProviderIds, Option<NfoKind>) {
    let mut ids = ProviderIds::default();
    let mut kind = None;
    if let Some(caps) = TMDB_URL.captures(text) {
        ids.tmdb = positive(&caps[2]);
        kind = Some(if caps[1].eq_ignore_ascii_case("movie") {
            NfoKind::Movie
        } else {
            NfoKind::TvShow
        });
    }
    if let Some(caps) = IMDB_URL.captures(text) {
        ids.imdb = Some(caps[1].to_string());
    }
    if let Some(caps) = TVDB_URL.captures(text) {
        ids.tvdb = positive(&caps[1]);
    }
    if let Some(caps) = ANILIST_URL.captures(text) {
        ids.anilist = positive(&caps[1]);
    }
    (ids, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parse(text: &str) -> Nfo {
        parse_nfo(text.as_bytes()).expect("parses")
    }

    #[test]
    fn a_movie_nfo_names_its_title_year_and_default_uniqueid() {
        let nfo = parse(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes" ?>
<movie>
    <title>The Matrix</title>
    <originaltitle>The Matrix</originaltitle>
    <year>1999</year>
    <uniqueid type="imdb">tt0133093</uniqueid>
    <uniqueid type="tmdb" default="true">603</uniqueid>
    <uniqueid type="tmdb">999</uniqueid>
    <actor><name>Keanu Reeves</name></actor>
</movie>"#,
        );
        assert_eq!(nfo.kind, Some(NfoKind::Movie));
        assert_eq!(nfo.title.as_deref(), Some("The Matrix"));
        assert_eq!(nfo.year, Some(1999));
        assert_eq!(
            nfo.ids,
            ProviderIds {
                tmdb: Some(603),
                imdb: Some("tt0133093".to_string()),
                tvdb: None,
                anilist: None,
            },
            "the default uniqueid wins over another of its type"
        );
        assert_eq!(nfo.ids.pin(), Some(ProviderPin::Tmdb(603)));
    }

    #[test]
    fn legacy_id_elements_are_read_when_no_uniqueid_names_the_provider() {
        let movie = parse("<movie><title>Heat</title><id>tt0113277</id></movie>");
        assert_eq!(movie.ids.imdb.as_deref(), Some("tt0113277"));
        assert_eq!(
            movie.ids.pin(),
            Some(ProviderPin::Imdb("tt0113277".to_string()))
        );

        let numeric = parse("<movie><id>949</id><imdbid>tt0113277</imdbid></movie>");
        assert_eq!(
            numeric.ids.tmdb,
            Some(949),
            "a movie's numeric id is TMDB's"
        );

        let show = parse("<tvshow><title>Breaking Bad</title><id>81189</id></tvshow>");
        assert_eq!(show.kind, Some(NfoKind::TvShow));
        assert_eq!(
            show.ids.tvdb,
            Some(81189),
            "a show's numeric id is TheTVDB's"
        );

        let both =
            parse(r#"<movie><uniqueid type="tmdb">603</uniqueid><tmdbid>1</tmdbid></movie>"#);
        assert_eq!(
            both.ids.tmdb,
            Some(603),
            "a uniqueid beats a legacy element"
        );
    }

    #[test]
    fn a_tvshow_nfo_premiered_date_gives_its_year() {
        let nfo = parse(
            r#"<tvshow><title>The Wire</title><premiered>2002-06-02</premiered>
               <uniqueid type="tvdb" default="true">79126</uniqueid></tvshow>"#,
        );
        assert_eq!(nfo.year, Some(2002));
        assert_eq!(nfo.ids.pin(), Some(ProviderPin::Tvdb(79126)));
    }

    #[test]
    fn a_multi_episode_nfo_lists_every_episode_and_pins_nothing() {
        let nfo = parse(
            r#"<episodedetails>
    <title>Pilot</title><showtitle>Lost</showtitle>
    <season>1</season><episode>1</episode><aired>2004-09-22</aired>
    <uniqueid type="tvdb" default="true">127131</uniqueid>
</episodedetails>
<episodedetails>
    <title>Pilot (2)</title><showtitle>Lost</showtitle>
    <season>1</season><episode>2</episode>
</episodedetails>"#,
        );
        assert_eq!(nfo.kind, Some(NfoKind::Episodes));
        assert_eq!(nfo.show_title.as_deref(), Some("Lost"));
        assert_eq!(nfo.title.as_deref(), Some("Pilot"));
        assert_eq!(
            nfo.episodes
                .iter()
                .map(|e| (e.season, e.episode))
                .collect::<Vec<_>>(),
            vec![(Some(1), Some(1)), (Some(1), Some(2))]
        );
        assert_eq!(nfo.episodes[0].aired, NaiveDate::from_ymd_opt(2004, 9, 22));
        assert_eq!(nfo.ids.pin(), None, "an episode's ids never pin its show");
    }

    #[test]
    fn xml_followed_by_a_url_reads_both() {
        let nfo = parse(
            "<movie><title>Alien</title></movie>\n\
             https://www.themoviedb.org/movie/348-alien?language=en&foo=bar\n",
        );
        assert_eq!(nfo.title.as_deref(), Some("Alien"));
        assert_eq!(nfo.ids.tmdb, Some(348));
    }

    #[test]
    fn a_url_only_nfo_names_ids_and_what_a_tmdb_url_says_it_is() {
        let movie = parse("https://www.themoviedb.org/movie/603");
        assert_eq!(movie.kind, Some(NfoKind::Movie));
        assert_eq!(movie.ids.pin(), Some(ProviderPin::Tmdb(603)));

        let show = parse("https://www.themoviedb.org/tv/1399-game-of-thrones");
        assert_eq!(show.kind, Some(NfoKind::TvShow));

        let imdb = parse("http://www.imdb.com/title/tt0133093/");
        assert_eq!(imdb.kind, None, "an IMDb URL does not say movie or show");
        assert_eq!(imdb.ids.imdb.as_deref(), Some("tt0133093"));

        let tvdb = parse("https://thetvdb.com/?tab=series&id=81189");
        assert_eq!(tvdb.ids.tvdb, Some(81189));

        let anilist = parse("https://anilist.co/anime/5114/Fullmetal-Alchemist-Brotherhood/");
        assert_eq!(anilist.ids.pin(), Some(ProviderPin::Anilist(5114)));
    }

    #[test]
    fn a_byte_order_mark_is_skipped() {
        let bytes = [
            b"\xEF\xBB\xBF".as_slice(),
            b"<movie><title>Up</title></movie>".as_slice(),
        ]
        .concat();
        assert_eq!(parse_nfo(&bytes).unwrap().title.as_deref(), Some("Up"));
    }

    #[test]
    fn what_cannot_be_trusted_is_an_error_not_a_guess() {
        assert_eq!(parse_nfo(b"<movie>\xff</movie>"), Err(NfoError::NotUtf8));
        assert!(matches!(
            parse_nfo(b"<movie><title>Up</movie>"),
            Err(NfoError::Malformed(_))
        ));
    }

    #[test]
    fn a_billion_laughs_is_refused_before_anything_is_expanded() {
        let laughs = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
 <!ENTITY lol "lol">
 <!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
 <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
 <!ENTITY lol9 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
]>
<movie><title>&lol9;</title></movie>"#;
        let started = std::time::Instant::now();
        assert_eq!(parse_nfo(laughs.as_bytes()), Err(NfoError::Doctype));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn more_nodes_than_the_cap_is_malformed() {
        let actors = "<actor/>".repeat(MAX_NFO_NODES as usize);
        let nfo = format!("<movie><title>Crowd</title>{actors}</movie>");
        assert!(matches!(
            parse_nfo(nfo.as_bytes()),
            Err(NfoError::Malformed(_))
        ));
    }

    #[test]
    fn an_unknown_root_names_nothing() {
        let nfo = parse("<musicvideo><title>Song</title></musicvideo>");
        assert_eq!(nfo.kind, None);
        assert_eq!(nfo.title, None);
    }

    #[test]
    fn a_year_outside_what_a_film_could_have_is_dropped() {
        assert_eq!(parse("<movie><year>0</year></movie>").year, None);
        assert_eq!(parse("<movie><year>99999</year></movie>").year, None);
    }

    proptest! {
        #[test]
        fn parsing_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_nfo(&bytes);
        }

        #[test]
        fn parsing_xml_shaped_text_never_panics(
            body in "(<(movie|tvshow|episodedetails|title|year|uniqueid|id)>|</(movie|tvshow|episodedetails|title|year|uniqueid|id)>|[a-z0-9 &;:/.?=-]{0,8}|<!--|-->|<\\?xml ?\\?>){0,24}",
        ) {
            let _ = parse_nfo(body.as_bytes());
        }

        #[test]
        fn a_parsed_pin_is_a_stored_pin(body in "\\PC{0,80}") {
            if let Ok(nfo) = parse_nfo(body.as_bytes())
                && let Some(pin) = nfo.ids.pin()
            {
                prop_assert_eq!(ProviderPin::parse(&pin.to_ref_string()), Some(pin));
            }
        }
    }
}
