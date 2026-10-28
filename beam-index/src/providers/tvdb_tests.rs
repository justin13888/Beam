//! Tests for the TheTVDB adapter: the mapping of its documents, and the
//! request flow against a local listener standing in for TheTVDB.

use super::*;

use std::sync::Mutex;

use beam_domain::services::TestClock;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn record(value: serde_json::Value) -> WireRecord {
    serde_json::from_value(value).expect("a record document")
}

fn episodes(value: serde_json::Value) -> Vec<WireEpisode> {
    serde_json::from_value(value).expect("an episode list")
}

mod mapping {
    use super::*;

    #[test]
    fn a_status_decides_the_error_before_the_body_is_read() {
        assert!(classify(200, None).is_ok());
        assert!(classify(204, None).is_ok());
        assert!(matches!(
            classify(404, None),
            Err(EnrichmentError::NotFound)
        ));
        assert!(matches!(
            classify(429, Some(Duration::from_secs(7))),
            Err(EnrichmentError::RateLimited { retry_after: Some(after) }) if after.as_secs() == 7
        ));
        for status in [401, 403] {
            assert!(matches!(
                classify(status, None),
                Err(EnrichmentError::Provider(message)) if message.contains("auth error")
            ));
        }
        assert!(matches!(
            classify(502, None),
            Err(EnrichmentError::Provider(message)) if message.contains("HTTP 502")
        ));
    }

    #[test]
    fn a_search_result_is_titled_in_english_beside_its_own_name() {
        let results: Vec<WireSearchResult> = serde_json::from_value(serde_json::json!([
            {"tvdb_id": "424536", "name": "Sousou no Frieren", "year": "2023",
             "translations": {"eng": "Frieren: Beyond Journey's End", "jpn": "葬送のフリーレン"}},
            {"tvdb_id": "121361", "name": "Game of Thrones", "year": "2011",
             "translations": {"eng": "Game of Thrones"}},
            {"tvdb_id": "81189", "name": "Breaking Bad"},
            {"tvdb_id": "series-1", "name": "Not numeric"},
            {"name": "No id"},
            {"tvdb_id": "7", "name": "  "}
        ]))
        .unwrap();
        let hits: Vec<SearchHit> = results.iter().filter_map(map_search_result).collect();
        assert_eq!(
            hits,
            [
                SearchHit {
                    external_ref: ExternalMediaRef::new("tvdb", "424536"),
                    title: "Frieren: Beyond Journey's End".to_string(),
                    original_title: Some("Sousou no Frieren".to_string()),
                    year: Some(2023),
                },
                SearchHit {
                    external_ref: ExternalMediaRef::new("tvdb", "121361"),
                    title: "Game of Thrones".to_string(),
                    original_title: None,
                    year: Some(2011),
                },
                SearchHit {
                    external_ref: ExternalMediaRef::new("tvdb", "81189"),
                    title: "Breaking Bad".to_string(),
                    original_title: None,
                    year: None,
                },
            ]
        );
    }

    #[test]
    fn a_series_record_maps_its_ids_art_rating_and_translations() {
        let show = map_show(&record(serde_json::json!({
            "id": 121361,
            "name": "Game of Thrones",
            "image": "https://artworks.thetvdb.com/banners/posters/121361-1.jpg",
            "firstAired": "2011-04-17",
            "genres": [{"name": "Drama"}, {"name": " "}, {"name": "Fantasy"}],
            "remoteIds": [
                {"id": "tt0944947", "type": 2, "sourceName": "IMDB"},
                {"id": "1399", "type": 12, "sourceName": "TheMovieDB.com"},
                {"id": "77", "sourceName": "TV.com"}
            ],
            "contentRatings": [
                {"name": "15", "country": "gbr"},
                {"name": "tv-ma", "country": "usa"}
            ],
            "artworks": [
                {"image": "https://artworks.thetvdb.com/low.jpg", "type": 3, "score": 10.0},
                {"image": "https://artworks.thetvdb.com/high.jpg", "type": 3, "score": 90.0},
                {"image": "https://artworks.thetvdb.com/banner.jpg", "type": 1, "score": 100.0}
            ],
            "translations": {
                "nameTranslations": [{"name": "Le Trône de fer", "language": "fra"}],
                "overviewTranslations": [
                    {"overview": "Sept familles nobles...", "language": "fra"},
                    {"overview": "Seven noble families...", "language": "eng"}
                ]
            }
        })));

        assert_eq!(show.tvdb_id, Some(121361));
        assert_eq!(show.tmdb_id, Some(1399));
        assert_eq!(show.imdb_id.as_deref(), Some("tt0944947"));
        assert_eq!(show.anilist_id, None);
        assert_eq!(show.title, "Game of Thrones");
        assert_eq!(show.original_title, None, "no English name: its own stands");
        assert_eq!(show.description.as_deref(), Some("Seven noble families..."));
        assert_eq!(show.year, Some(2011), "read from the first aired date");
        assert_eq!(
            show.poster_url.as_deref(),
            Some("https://artworks.thetvdb.com/banners/posters/121361-1.jpg")
        );
        assert_eq!(
            show.backdrop_url.as_deref(),
            Some("https://artworks.thetvdb.com/high.jpg"),
            "the best-scored background, not a banner"
        );
        assert_eq!(show.content_rating.as_deref(), Some("TV-MA"));
        assert_eq!(show.genres, ["Drama", "Fantasy"]);
        assert_eq!(show.rating, None);
        assert_eq!(show.credits, None, "the stored credits are left alone");
    }

    #[test]
    fn a_bare_series_record_maps_to_its_id_and_name() {
        let show = map_show(&record(serde_json::json!({
            "id": 5, "name": "Bare", "overview": "Its own overview."
        })));
        assert_eq!(show.tvdb_id, Some(5));
        assert_eq!(show.title, "Bare");
        assert_eq!(show.description.as_deref(), Some("Its own overview."));
        assert_eq!(
            (
                show.tmdb_id,
                show.imdb_id,
                show.backdrop_url,
                show.content_rating
            ),
            (None, None, None, None)
        );
    }

    #[test]
    fn a_movie_record_maps_its_release_runtime_and_backdrop() {
        let movie = map_movie(&record(serde_json::json!({
            "id": 12,
            "name": "Kimi no Na wa.",
            "runtime": 106,
            "first_release": {"date": "2016-08-26"},
            "remoteIds": [{"id": "372058", "sourceName": "TheMovieDB.com"}],
            "artworks": [
                {"image": "https://artworks.thetvdb.com/series-bg.jpg", "type": 3},
                {"image": "https://artworks.thetvdb.com/movie-bg.jpg", "type": 15}
            ],
            "translations": {"nameTranslations": [{"name": "Your Name.", "language": "eng"}]}
        })));
        assert_eq!(movie.tmdb_id, Some(372058));
        assert_eq!(movie.title, "Your Name.");
        assert_eq!(movie.original_title.as_deref(), Some("Kimi no Na wa."));
        assert_eq!(
            movie.release_date,
            chrono::NaiveDate::from_ymd_opt(2016, 8, 26)
        );
        assert_eq!(movie.year, Some(2016));
        assert_eq!(movie.runtime_mins, Some(106));
        assert_eq!(
            movie.backdrop_url.as_deref(),
            Some("https://artworks.thetvdb.com/movie-bg.jpg")
        );
        assert_eq!(movie.collection, None);
    }

    #[test]
    fn a_season_is_its_episodes_with_their_absolute_numbers_and_specials_apart() {
        let listed = episodes(serde_json::json!([
            {"seasonNumber": 1, "number": 2, "absoluteNumber": 2, "name": "Second",
             "aired": "2023-09-29", "runtime": 24, "image": "https://artworks.thetvdb.com/e2.jpg"},
            {"seasonNumber": 1, "number": 1, "absoluteNumber": 1, "name": "First",
             "overview": "It begins.", "aired": "2023-09-29"},
            {"seasonNumber": 1, "number": 0, "name": "Unnumbered"},
            {"seasonNumber": 0, "number": 1, "name": "Recap", "aired": "2023-12-01"},
            {"seasonNumber": 2, "number": 1, "absoluteNumber": 29, "aired": "not a date"}
        ]));

        let season = map_season(&listed, 1).expect("season 1 has episodes");
        assert_eq!(season.season_number, 1);
        assert_eq!(
            season.air_date,
            chrono::NaiveDate::from_ymd_opt(2023, 9, 29)
        );
        assert_eq!(season.poster_url, None);
        let numbers: Vec<(u32, Option<u32>, Option<&str>)> = season
            .episodes
            .iter()
            .map(|e| (e.episode_number, e.absolute_number, e.title.as_deref()))
            .collect();
        assert_eq!(
            numbers,
            [(2, Some(2), Some("Second")), (1, Some(1), Some("First"))],
            "an episode numbered 0 is no episode"
        );
        assert_eq!(season.episodes[0].runtime_mins, Some(24));
        assert_eq!(
            season.episodes[1].description.as_deref(),
            Some("It begins.")
        );

        let specials = map_season(&listed, 0).expect("the specials");
        assert_eq!(specials.episodes[0].title.as_deref(), Some("Recap"));

        let second = map_season(&listed, 2).expect("season 2");
        assert_eq!(second.episodes[0].absolute_number, Some(29));
        assert_eq!(second.episodes[0].title, None);
        assert_eq!(second.air_date, None);

        assert!(map_season(&listed, 3).is_none());
    }

    #[test]
    fn only_a_tvdb_id_is_resolved_here() {
        assert_eq!(
            native_id(&ExternalMediaRef::new("tvdb", "81189")).unwrap(),
            81189
        );
        assert!(matches!(
            native_id(&ExternalMediaRef::new("tmdb", "1399")),
            Err(EnrichmentError::NotConfigured)
        ));
        assert!(matches!(
            native_id(&ExternalMediaRef::new("tvdb", "abc")),
            Err(EnrichmentError::Provider(_))
        ));
    }

    #[test]
    fn no_api_key_is_no_provider() {
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
        for api_key in [None, Some(String::new()), Some("   ".to_string())] {
            let built = build_client(
                TvdbWiringConfig {
                    api_key,
                    pin: Some("1234".to_string()),
                },
                clock.clone(),
            )
            .unwrap();
            assert!(built.is_none());
        }
        let built = build_client(
            TvdbWiringConfig {
                api_key: Some("key".to_string()),
                pin: None,
            },
            clock,
        )
        .unwrap()
        .expect("a key configures it");
        assert_eq!(built.available_providers(), ["tvdb"]);
    }

    #[test]
    fn the_credentials_never_reach_a_log() {
        let config = TvdbWiringConfig {
            api_key: Some("secret-key".to_string()),
            pin: Some("secret-pin".to_string()),
        };
        let shown = format!("{config:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}

// ── The request flow, against a local stand-in for TheTVDB ────────────────

/// One request the stand-in received.
#[derive(Debug, Clone)]
struct Received {
    method: String,
    target: String,
    authorization: Option<String>,
    body: String,
}

/// The stand-in's answer: status, extra headers, body.
type Answer = (u16, Vec<(&'static str, String)>, String);

type Routes = Arc<dyn Fn(&Received) -> Answer + Send + Sync>;

/// A listener answering every connection by `routes`, and what it was asked.
async fn stand_in(routes: Routes) -> (Url, Arc<Mutex<Vec<Received>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/v4/", listener.local_addr().unwrap())).unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = received.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
                let mut lines = head.lines();
                let mut request_line = lines.next().unwrap().split(' ');
                let method = request_line.next().unwrap().to_string();
                let target = request_line.next().unwrap().to_string();
                let header = |name: &str| {
                    head.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_string())
                    })
                };
                let length: usize = header("content-length")
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                while raw.len() < head_end + length {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                }
                let request = Received {
                    method,
                    target,
                    authorization: header("authorization"),
                    body: String::from_utf8_lossy(&raw[head_end..]).to_string(),
                };
                log.lock().unwrap().push(request.clone());
                let (status, headers, body) = routes(&request);
                let mut response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(&body);
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (base, received)
}

fn ok(body: serde_json::Value) -> Answer {
    (200, Vec::new(), body.to_string())
}

fn login(token: &str) -> Answer {
    ok(serde_json::json!({"status": "success", "data": {"token": token}}))
}

fn provider(base: Url, clock: Arc<TestClock>) -> TvdbEnrichmentProvider {
    TvdbEnrichmentProvider::new(
        base,
        "the-key".to_string(),
        Some("the-pin".to_string()),
        clock,
    )
    .unwrap()
}

fn targets(received: &Mutex<Vec<Received>>) -> Vec<String> {
    received
        .lock()
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect()
}

#[tokio::test]
async fn a_search_logs_in_and_asks_with_the_bearer_token() {
    let (base, received) = stand_in(Arc::new(|request: &Received| {
        match request.target.as_str() {
            "/v4/login" => login("token-1"),
            _ => ok(serde_json::json!({"data": [
                {"tvdb_id": "424536", "name": "Sousou no Frieren", "year": "2023"}
            ]})),
        }
    }))
    .await;
    let provider = provider(base, Arc::new(TestClock::new()));

    let query = MediaQuery {
        title: "Frieren & co".to_string(),
        year: Some(2023),
    };
    let hits = provider.search_shows(&query).await.unwrap();
    assert_eq!(hits[0].external_ref.as_str(), "tvdb:424536");
    assert_eq!(hits[0].popularity, None);
    let movies = provider.search_movies(&query).await.unwrap();
    assert_eq!(movies.len(), 1);

    assert_eq!(
        targets(&received),
        [
            "POST /v4/login",
            "GET /v4/search?query=Frieren+%26+co&type=series&limit=20",
            "GET /v4/search?query=Frieren+%26+co&type=movie&limit=20",
        ],
        "one login serves every later request"
    );
    let log = received.lock().unwrap();
    let credentials: serde_json::Value = serde_json::from_str(&log[0].body).unwrap();
    assert_eq!(
        credentials,
        serde_json::json!({"apikey": "the-key", "pin": "the-pin"})
    );
    assert_eq!(log[1].authorization.as_deref(), Some("Bearer token-1"));
    assert_eq!(log[2].authorization.as_deref(), Some("Bearer token-1"));
}

#[tokio::test]
async fn a_refused_token_is_replaced_once_and_the_request_repeated() {
    let logins = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = logins.clone();
    let (base, received) = stand_in(Arc::new(move |request: &Received| {
        if request.target == "/v4/login" {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            return login(&format!("token-{n}"));
        }
        match request.authorization.as_deref() {
            Some("Bearer token-2") => {
                ok(serde_json::json!({"data": {"id": 81189, "name": "Breaking Bad"}}))
            }
            _ => (401, Vec::new(), "{}".to_string()),
        }
    }))
    .await;
    let provider = provider(base, Arc::new(TestClock::new()));

    let show = provider
        .show_enrichment(&ExternalMediaRef::new("tvdb", "81189"))
        .await
        .unwrap();
    assert_eq!(show.title, "Breaking Bad");
    assert_eq!(
        targets(&received),
        [
            "POST /v4/login",
            "GET /v4/series/81189/extended?meta=translations",
            "POST /v4/login",
            "GET /v4/series/81189/extended?meta=translations",
        ]
    );
}

#[tokio::test]
async fn a_token_refused_twice_is_an_auth_error_not_a_loop() {
    let (base, received) = stand_in(Arc::new(|request: &Received| {
        match request.target.as_str() {
            "/v4/login" => login("token"),
            _ => (401, Vec::new(), "{}".to_string()),
        }
    }))
    .await;
    let provider = provider(base, Arc::new(TestClock::new()));
    let err = provider
        .movie_enrichment(&ExternalMediaRef::new("tvdb", "12"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EnrichmentError::Provider(m) if m.contains("auth error")),
        "{err:?}"
    );
    assert_eq!(received.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn a_refused_api_key_is_an_auth_error() {
    let (base, _received) =
        stand_in(Arc::new(|_: &Received| (401, Vec::new(), "{}".to_string()))).await;
    let provider = provider(base, Arc::new(TestClock::new()));
    let err = provider
        .search_shows(&MediaQuery {
            title: "x".to_string(),
            year: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, EnrichmentError::Provider(m) if m.contains("API key")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_rate_limit_carries_its_retry_after_and_a_missing_title_is_not_found() {
    let (base, _received) = stand_in(Arc::new(|request: &Received| {
        match request.target.as_str() {
            "/v4/login" => login("token"),
            target if target.starts_with("/v4/search") => (
                429,
                vec![("retry-after", "5".to_string())],
                "{}".to_string(),
            ),
            _ => (404, Vec::new(), "{}".to_string()),
        }
    }))
    .await;
    let provider = provider(base, Arc::new(TestClock::new()));
    let query = MediaQuery {
        title: "x".to_string(),
        year: None,
    };
    assert!(matches!(
        provider.search_shows(&query).await,
        Err(EnrichmentError::RateLimited { retry_after: Some(after) }) if after.as_secs() == 5
    ));
    assert!(matches!(
        provider
            .show_enrichment(&ExternalMediaRef::new("tvdb", "1"))
            .await,
        Err(EnrichmentError::NotFound)
    ));
    // Not TheTVDB's id: nothing is asked.
    assert!(matches!(
        provider
            .season_enrichment(&ExternalMediaRef::new("tmdb", "1"), 1)
            .await,
        Err(EnrichmentError::NotConfigured)
    ));
}

#[tokio::test]
async fn a_series_episodes_are_listed_across_pages_once_for_all_its_seasons() {
    let (base, received) = stand_in(Arc::new(|request: &Received| {
        match request.target.as_str() {
            "/v4/login" => login("token"),
            "/v4/series/81797/episodes/default/eng?page=0" => ok(serde_json::json!({
                "data": {"episodes": [
                    {"seasonNumber": 0, "number": 1, "name": "Special"},
                    {"seasonNumber": 1, "number": 1, "absoluteNumber": 1, "name": "One"}
                ]},
                "links": {"next": "https://api4.thetvdb.com/v4/series/81797/episodes/default/eng?page=1"}
            })),
            "/v4/series/81797/episodes/default/eng?page=1" => ok(serde_json::json!({
                "data": {"episodes": [
                    {"seasonNumber": 2, "number": 1, "absoluteNumber": 2, "name": "Two"}
                ]},
                "links": {"next": null}
            })),
            _ => (404, Vec::new(), "{}".to_string()),
        }
    }))
    .await;
    let clock = Arc::new(TestClock::new());
    let provider = provider(base, clock.clone());
    let series = ExternalMediaRef::new("tvdb", "81797");
    let listings = || {
        targets(&received)
            .into_iter()
            .filter(|target| target.contains("/episodes/"))
            .count()
    };

    let specials = provider.season_enrichment(&series, 0).await.unwrap();
    assert_eq!(specials.episodes[0].title.as_deref(), Some("Special"));
    let second = provider.season_enrichment(&series, 2).await.unwrap();
    assert_eq!(second.episodes[0].absolute_number, Some(2));
    assert!(matches!(
        provider.season_enrichment(&series, 3).await,
        Err(EnrichmentError::NotFound)
    ));
    assert_eq!(listings(), 2, "both pages, once, for every season");

    provider.invalidate(&series).await;
    provider.season_enrichment(&series, 1).await.unwrap();
    assert_eq!(listings(), 4, "a refresh lists the episodes again");

    clock.advance(EPISODE_CACHE_TTL);
    provider.season_enrichment(&series, 1).await.unwrap();
    assert_eq!(listings(), 6, "a stale list is listed again");
}
