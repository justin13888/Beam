//! The `/v1` wire conventions, asserted against the document the server
//! exports (issue #190).
//!
//! Every generated client -- the web SPA's types, `beam-client-core`, and the
//! Android and Apple apps above it -- is derived from this document, so a shape
//! it gets wrong is a shape every client inherits. These rules are what "the
//! `/v1` contract is consistent" means, checked rather than hoped for:
//!
//! * **R1** every enum value, and every `const`, is `snake_case`
//!   (`^[a-z][a-z0-9_]*$`), so no client has to guess whether a value is
//!   `H264`, `Known` or `next_up`. A problem document's `type` is a URI by
//!   RFC 9457, not a value from a set; `taxonomy_tests` governs it;
//! * **R2** no schema is named with a `Dto` or `Response` suffix -- the name
//!   is the thing on the wire, not the layer that produced it;
//! * **R2b** no two enums carry the same set of values. Two schemas meaning
//!   one thing (`AdminLogLevelDto` and `AdminEventLevelDto` were both
//!   `info`/`warning`/`error`) become two generated types a client has to
//!   convert between for nothing;
//! * **R4** every identifier -- a value named `id` or `*_id` -- is a UUID
//!   (`format: uuid`), in a body and in a path or query parameter alike. The
//!   one schema outside it is `ExternalIdentifiers`, which carries *other*
//!   systems' identifiers (an IMDb `tt...`, a numeric TMDB id) in their own
//!   shapes: Beam does not mint them and cannot make them UUIDs;
//! * **R5** a value is named `*_at` if and only if it is an instant
//!   (`format: date-time`); a value naming a day (`date`, `*_date`, `*_on`,
//!   `*_aired`) is a calendar date (`format: date`), not a date-time at a
//!   made-up midnight; and no value is an epoch -- nothing is named
//!   `timestamp`, `*_timestamp`, `*_time`, `*_epoch` or `*_unix`;
//! * **R6** a number measuring a duration, runtime, size, position or length
//!   says its unit: its name ends in `_secs`, `_mins`, `_ms`, `_days`,
//!   `_bytes` or `_count`. `duration` and `runtime` in one schema meaning
//!   seconds and minutes is the ambiguity this closes.
//!
//! R4-R6 read the payload vocabulary: every property of a component schema,
//! however deeply nested, and every path and query parameter. Headers are
//! transport, and so is the `text/event-stream` frame Kynos describes around an
//! SSE payload -- its `id` is the event-stream field of that name, not a Beam
//! identifier; the payload inside it is a component and is read.
//!
//! There are no exceptions: a schema that cannot comply is changed, not
//! listed here. The rules are pure functions over a JSON document, so each is
//! also shown to catch a violation in a small document built for the purpose;
//! a rule that could not fail would pass the real document for free.
//!
//! Hermetic: it builds the router's description. Nothing to start, nothing to
//! reach (NFR-201).

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kynos::openapi::SpecVersion;
    use serde_json::{Value, json};

    use crate::routes::create_router;

    /// The document every client is generated from.
    fn exported_document() -> Value {
        serde_json::to_value(
            create_router()
                .openapi_as(SpecVersion::V3_2)
                .expect("the router exports"),
        )
        .expect("the document serializes")
    }

    /// One JSON-pointer reference token.
    fn token(key: &str) -> String {
        key.replace('~', "~0").replace('/', "~1")
    }

    /// The values a rule over enumerations reads.
    #[derive(Default)]
    struct Enumerations {
        /// Every `enum`, beside the pointer it was found at.
        enums: Vec<(String, Vec<Value>)>,
        /// Every `const`, beside the pointer of the schema carrying it.
        consts: Vec<(String, Value)>,
    }

    /// Every `enum` and `const` in the document, anywhere -- components,
    /// parameters, inline properties.
    fn enumerations(document: &Value) -> Enumerations {
        fn walk(node: &Value, pointer: &str, found: &mut Enumerations) {
            match node {
                Value::Object(object) => {
                    if let Some(Value::Array(values)) = object.get("enum") {
                        found.enums.push((pointer.to_owned(), values.clone()));
                    }
                    if let Some(value) = object.get("const") {
                        found.consts.push((pointer.to_owned(), value.clone()));
                    }
                    for (key, child) in object {
                        walk(child, &format!("{pointer}/{}", token(key)), found);
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        walk(child, &format!("{pointer}/{index}"), found);
                    }
                }
                _ => {}
            }
        }

        let mut found = Enumerations::default();
        walk(document, "", &mut found);
        found
    }

    fn is_snake_case(value: &str) -> bool {
        let mut chars = value.chars();
        chars.next().is_some_and(|first| first.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    /// The media type of an RFC 9457 problem document. Its `type` is a URI by
    /// that RFC -- a `const` per declared problem -- and `taxonomy_tests`
    /// governs its shape; it is an identifier, not a value from a set.
    const PROBLEM_JSON: &str = "/content/application~1problem+json/";

    /// R1: every enum value and `const` that is not a `snake_case` string.
    /// A problem document's `type` URI is not a value R1 reads (see
    /// [`PROBLEM_JSON`]).
    fn r1_violations(document: &Value) -> Vec<String> {
        let Enumerations { enums, consts } = enumerations(document);
        let values = enums
            .into_iter()
            .flat_map(|(pointer, values)| values.into_iter().map(move |v| (pointer.clone(), v)))
            .chain(
                consts
                    .into_iter()
                    .filter(|(pointer, _)| !pointer.contains(PROBLEM_JSON)),
            );
        values
            .filter_map(|(pointer, value)| match &value {
                Value::String(text) if is_snake_case(text) => None,
                Value::Null => None,
                other => Some(format!("{pointer}: {other}")),
            })
            .collect()
    }

    /// R2: every component schema whose name ends in `Dto` or `Response`.
    fn r2_violations(document: &Value) -> Vec<String> {
        document
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|schemas| schemas.keys())
            .filter(|name| name.ends_with("Dto") || name.ends_with("Response"))
            .cloned()
            .collect()
    }

    /// R2b: every set of enum values that more than one enum carries, with
    /// the pointers that carry it. Order is ignored, and so is a `null` a
    /// nullable enum adds -- neither makes two enums mean different things.
    fn r2b_violations(document: &Value) -> Vec<String> {
        let mut by_values: BTreeMap<Vec<String>, Vec<String>> = BTreeMap::new();
        for (pointer, values) in enumerations(document).enums {
            let mut key: Vec<String> = values
                .iter()
                .filter(|value| !value.is_null())
                .map(Value::to_string)
                .collect();
            key.sort();
            key.dedup();
            by_values.entry(key).or_default().push(pointer);
        }
        by_values
            .into_iter()
            .filter(|(_, pointers)| pointers.len() > 1)
            .map(|(values, pointers)| format!("[{}] at {pointers:?}", values.join(", ")))
            .collect()
    }

    /// One named value of the payload vocabulary.
    struct Named {
        pointer: String,
        name: String,
        /// The component schema it belongs to; `None` for a parameter.
        component: Option<String>,
        schema: Value,
    }

    /// Every named value R4-R6 read: each property of each component schema,
    /// however deeply nested (inline objects, array items, composed
    /// branches), and each path and query parameter of each operation.
    fn named_values(document: &Value) -> Vec<Named> {
        fn properties(node: &Value, pointer: &str, component: &str, found: &mut Vec<Named>) {
            match node {
                Value::Object(object) => {
                    if let Some(Value::Object(props)) = object.get("properties") {
                        for (name, schema) in props {
                            found.push(Named {
                                pointer: format!("{pointer}/properties/{}", token(name)),
                                name: name.clone(),
                                component: Some(component.to_owned()),
                                schema: schema.clone(),
                            });
                        }
                    }
                    for (key, child) in object {
                        properties(
                            child,
                            &format!("{pointer}/{}", token(key)),
                            component,
                            found,
                        );
                    }
                }
                Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        properties(child, &format!("{pointer}/{index}"), component, found);
                    }
                }
                _ => {}
            }
        }

        let mut found = Vec::new();
        for (component, schema) in document
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let pointer = format!("/components/schemas/{}", token(component));
            properties(schema, &pointer, component, &mut found);
        }
        for (path, item) in document
            .get("paths")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            for (method, operation) in item.as_object().into_iter().flatten() {
                let parameters = operation.get("parameters").and_then(Value::as_array);
                for (index, parameter) in parameters.into_iter().flatten().enumerate() {
                    let located = parameter
                        .get("in")
                        .and_then(Value::as_str)
                        .is_some_and(|location| location == "path" || location == "query");
                    if let (true, Some(name), Some(schema)) = (
                        located,
                        parameter.get("name").and_then(Value::as_str),
                        parameter.get("schema"),
                    ) {
                        found.push(Named {
                            pointer: format!("/paths/{}/{method}/parameters/{index}", token(path)),
                            name: name.to_owned(),
                            component: None,
                            schema: schema.clone(),
                        });
                    }
                }
            }
        }
        found
    }

    /// The concrete schemas a value can take: `$ref`s followed, `anyOf`,
    /// `oneOf` and `allOf` branches opened, and the `null` of a nullable value
    /// dropped. A nullable UUID is a UUID.
    fn leaves(document: &Value, schema: &Value) -> Vec<Value> {
        fn collect(document: &Value, schema: &Value, depth: u8, found: &mut Vec<Value>) {
            // A cycle is a recursive type, which no scalar rule reads through.
            if depth > 16 {
                return;
            }
            if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
                if let Some(resolved) = target
                    .strip_prefix('#')
                    .and_then(|pointer| document.pointer(pointer))
                {
                    collect(document, resolved, depth + 1, found);
                }
                return;
            }
            let mut composed = false;
            for keyword in ["anyOf", "oneOf", "allOf"] {
                for branch in schema
                    .get(keyword)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    composed = true;
                    collect(document, branch, depth + 1, found);
                }
            }
            if composed || schema.get("type") == Some(&json!("null")) {
                return;
            }
            found.push(schema.clone());
        }

        let mut found = Vec::new();
        collect(document, schema, 0, &mut found);
        found
    }

    fn has_type(leaf: &Value, wanted: &str) -> bool {
        match leaf.get("type") {
            Some(Value::String(one)) => one == wanted,
            Some(Value::Array(many)) => many.iter().any(|t| t == wanted),
            _ => false,
        }
    }

    fn format_of(leaf: &Value) -> Option<&str> {
        leaf.get("format").and_then(Value::as_str)
    }

    /// Whether every concrete shape of `named` has `format`.
    fn always_formatted(document: &Value, named: &Named, format: &str) -> bool {
        let leaves = leaves(document, &named.schema);
        !leaves.is_empty() && leaves.iter().all(|leaf| format_of(leaf) == Some(format))
    }

    fn sometimes_formatted(document: &Value, named: &Named, format: &str) -> bool {
        leaves(document, &named.schema)
            .iter()
            .any(|leaf| format_of(leaf) == Some(format))
    }

    /// The schema whose properties are other systems' identifiers, in their
    /// own shapes (see R4 above).
    const EXTERNAL_IDENTIFIERS: &str = "ExternalIdentifiers";

    /// R4: every `id` or `*_id` that is not always `format: uuid`.
    fn r4_violations(document: &Value) -> Vec<String> {
        named_values(document)
            .into_iter()
            .filter(|named| named.name == "id" || named.name.ends_with("_id"))
            .filter(|named| named.component.as_deref() != Some(EXTERNAL_IDENTIFIERS))
            .filter(|named| !always_formatted(document, named, "uuid"))
            .map(|named| named.pointer)
            .collect()
    }

    /// Names that say a value is a day.
    fn names_a_day(name: &str) -> bool {
        name == "date"
            || name.ends_with("_date")
            || name.ends_with("_on")
            || name.ends_with("_aired")
    }

    /// Names an epoch travels under.
    fn names_an_epoch(name: &str) -> bool {
        name == "timestamp"
            || ["_timestamp", "_time", "_epoch", "_unix"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
    }

    /// R5: every `*_at` that is not always an instant, every instant not named
    /// `*_at`, every day not a `format: date`, and every epoch-named value.
    fn r5_violations(document: &Value) -> Vec<String> {
        let mut violations = Vec::new();
        for named in named_values(document) {
            let Named { pointer, name, .. } = &named;
            if name.ends_with("_at") && !always_formatted(document, &named, "date-time") {
                violations.push(format!("{pointer}: named *_at but not a date-time"));
            }
            if !name.ends_with("_at") && sometimes_formatted(document, &named, "date-time") {
                violations.push(format!("{pointer}: a date-time not named *_at"));
            }
            if names_a_day(name) && !always_formatted(document, &named, "date") {
                violations.push(format!("{pointer}: names a day but is not a date"));
            }
            if names_an_epoch(name) {
                violations.push(format!("{pointer}: named as an epoch"));
            }
        }
        violations
    }

    /// The quantities R6 wants a unit on.
    const QUANTITIES: [&str; 5] = ["duration", "runtime", "size", "position", "length"];
    /// The suffixes that name a unit.
    const UNIT_SUFFIXES: [&str; 6] = ["_secs", "_mins", "_ms", "_days", "_bytes", "_count"];

    /// R6: every number measuring one of [`QUANTITIES`] whose name ends in
    /// none of [`UNIT_SUFFIXES`].
    fn r6_violations(document: &Value) -> Vec<String> {
        named_values(document)
            .into_iter()
            .filter(|named| named.name.split('_').any(|word| QUANTITIES.contains(&word)))
            .filter(|named| {
                leaves(document, &named.schema)
                    .iter()
                    .any(|leaf| has_type(leaf, "integer") || has_type(leaf, "number"))
            })
            .filter(|named| {
                !UNIT_SUFFIXES
                    .iter()
                    .any(|suffix| named.name.ends_with(suffix))
            })
            .map(|named| named.pointer)
            .collect()
    }

    /// The pointers of every named value -- what the vacuity guards below
    /// check a known conforming value is among.
    fn named_pointers(document: &Value) -> Vec<String> {
        named_values(document)
            .into_iter()
            .map(|named| named.pointer)
            .collect()
    }

    #[test]
    fn r1_every_enum_value_and_const_is_snake_case() {
        let document = exported_document();
        let found = enumerations(&document);
        assert!(
            found
                .enums
                .iter()
                .any(|(pointer, _)| pointer == "/components/schemas/LogLevel"),
            "the walk did not find LogLevel's enum, so it is not reading the document"
        );
        let violations = r1_violations(&document);
        assert!(
            violations.is_empty(),
            "enum and const values must match ^[a-z][a-z0-9_]*$:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn r2_no_schema_name_carries_a_dto_or_response_suffix() {
        let document = exported_document();
        assert!(
            document
                .pointer("/components/schemas")
                .and_then(Value::as_object)
                .is_some_and(|schemas| !schemas.is_empty()),
            "the document names no schemas, so the rule would pass vacuously"
        );
        let violations = r2_violations(&document);
        assert!(
            violations.is_empty(),
            "schema names must not end in Dto or Response: {violations:?}"
        );
    }

    #[test]
    fn r2b_no_two_enums_share_a_value_set() {
        let violations = r2b_violations(&exported_document());
        assert!(
            violations.is_empty(),
            "two enums with one value set are one enum:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn r4_every_identifier_is_a_uuid() {
        let document = exported_document();
        let named = named_pointers(&document);
        for known in [
            "/components/schemas/Library/properties/id",
            "/paths/~1v1~1media~1{id}/get/parameters/0",
        ] {
            assert!(
                named.iter().any(|pointer| pointer == known),
                "the walk did not reach {known}, so it is not reading the document"
            );
        }
        let violations = r4_violations(&document);
        assert!(
            violations.is_empty(),
            "id and *_id must be format uuid:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn r5_instants_are_named_at_and_days_are_dates() {
        let document = exported_document();
        let named = named_pointers(&document);
        for known in [
            "/components/schemas/SessionSummary/properties/created_at",
            "/components/schemas/MovieMetadata/properties/release_date",
        ] {
            assert!(
                named.iter().any(|pointer| pointer == known),
                "the walk did not reach {known}, so it is not reading the document"
            );
        }
        let violations = r5_violations(&document);
        assert!(
            violations.is_empty(),
            "*_at if and only if date-time; days are dates; no epochs:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn r6_every_measured_number_names_its_unit() {
        let document = exported_document();
        let named = named_pointers(&document);
        for known in [
            "/components/schemas/MediaSource/properties/duration_secs",
            "/components/schemas/MovieMetadata/properties/runtime_mins",
        ] {
            assert!(
                named.iter().any(|pointer| pointer == known),
                "the walk did not reach {known}, so it is not reading the document"
            );
        }
        let violations = r6_violations(&document);
        assert!(
            violations.is_empty(),
            "a duration, runtime, size, position or length names its unit ({}):\n{}",
            UNIT_SUFFIXES.join(", "),
            violations.join("\n")
        );
    }

    /// Each rule, shown to fire on the violation it exists for and to stay
    /// quiet on the shape it allows.
    #[test]
    fn each_rule_catches_the_violation_it_names() {
        let violating = json!({
            "components": { "schemas": {
                "OutputVideoCodec": { "type": "string", "enum": ["H264", "av1"] },
                "FileIndexStatus": { "type": "string", "enum": ["known", "2nd", "not-found"] },
                "AdminLogLevelDto": { "type": "string", "enum": ["info", "warning", "error"] },
                "AdminEventLevel": { "type": "string", "enum": ["error", "info", "warning"] },
                "MeResponse": { "type": "object" },
                "MovieTag": { "type": "string", "const": "Movie" },
            }},
            "paths": { "/v1/things": { "get": {
                "parameters": [
                    { "name": "level", "in": "query",
                      "schema": { "type": ["string", "null"], "enum": ["warning", "info", "error", null] } }
                ],
                "responses": { "404": { "content": { "application/problem+json": { "schema": {
                    "properties": { "type": {
                        "const": "https://beam.justinchung.net/reference/errors/#thing-not-found"
                    }}
                }}}}}
            }}}
        });

        let mut r1 = r1_violations(&violating);
        r1.sort();
        assert_eq!(
            r1,
            vec![
                "/components/schemas/FileIndexStatus: \"2nd\"".to_owned(),
                "/components/schemas/FileIndexStatus: \"not-found\"".to_owned(),
                "/components/schemas/MovieTag: \"Movie\"".to_owned(),
                "/components/schemas/OutputVideoCodec: \"H264\"".to_owned(),
            ],
            "PascalCase, a leading digit, a hyphen and a PascalCase const are each caught; \
             null is not a value, and a problem type is a URI"
        );

        let mut r2 = r2_violations(&violating);
        r2.sort();
        assert_eq!(r2, vec!["AdminLogLevelDto", "MeResponse"]);

        let r2b = r2b_violations(&violating);
        assert_eq!(r2b.len(), 1, "one shared set: {r2b:?}");
        for pointer in [
            "/components/schemas/AdminLogLevelDto",
            "/components/schemas/AdminEventLevel",
            "/paths/~1v1~1things/get/parameters/0/schema",
        ] {
            assert!(
                r2b[0].contains(pointer),
                "{pointer} shares the set regardless of order or nullability: {r2b:?}"
            );
        }

        let conforming = json!({
            "components": { "schemas": {
                "LogLevel": { "type": "string", "enum": ["info", "warning", "error"] },
                "HealthState": { "type": "string", "enum": ["healthy", "degraded"] },
                "CurrentUser": { "type": "object" },
                "MovieTag": { "type": "string", "const": "movie" },
            }}
        });
        assert!(r1_violations(&conforming).is_empty());
        assert!(r2_violations(&conforming).is_empty());
        assert!(r2b_violations(&conforming).is_empty());
    }

    #[test]
    fn r4_catches_an_identifier_that_is_not_a_uuid() {
        let violating = json!({
            "components": { "schemas": {
                "Library": { "type": "object", "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                }},
                "Movie": { "type": "object", "properties": {
                    "file_id": { "type": ["string", "null"] },
                    "parts": { "type": "array", "items": { "type": "object", "properties": {
                        "library_id": { "type": "integer" },
                    }}},
                }},
                "ExternalIdentifiers": { "type": "object", "properties": {
                    "imdb_id": { "type": ["string", "null"] },
                    "tmdb_id": { "type": ["integer", "null"] },
                }},
            }},
            "paths": { "/v1/libraries/{id}": { "get": { "parameters": [
                { "name": "id", "in": "path", "schema": { "type": "string" } },
                { "name": "X-Request-Id", "in": "header", "schema": { "type": "string" } },
            ]}}}
        });
        let mut r4 = r4_violations(&violating);
        r4.sort();
        assert_eq!(
            r4,
            vec![
                "/components/schemas/Library/properties/id".to_owned(),
                "/components/schemas/Movie/properties/file_id".to_owned(),
                "/components/schemas/Movie/properties/parts/items/properties/library_id".to_owned(),
                "/paths/~1v1~1libraries~1{id}/get/parameters/0".to_owned(),
            ],
            "a body id, a nullable *_id, a nested *_id and a path id are caught; \
             ExternalIdentifiers and headers are not read"
        );

        let conforming = json!({
            "components": { "schemas": {
                "LibraryId": { "type": "string", "format": "uuid" },
                "Library": { "type": "object", "properties": {
                    "id": { "$ref": "#/components/schemas/LibraryId" },
                }},
                "Movie": { "type": "object", "properties": {
                    "file_id": { "type": ["string", "null"], "format": "uuid" },
                    "season_id": { "anyOf": [
                        { "$ref": "#/components/schemas/LibraryId" },
                        { "type": "null" },
                    ]},
                }},
            }},
            "paths": { "/v1/libraries/{id}": { "get": { "parameters": [
                { "name": "id", "in": "path", "schema": { "type": "string", "format": "uuid" } },
            ]}}}
        });
        assert!(r4_violations(&conforming).is_empty());
    }

    #[test]
    fn r5_catches_instants_days_and_epochs_named_wrong() {
        let violating = json!({
            "components": { "schemas": {
                "SessionSummary": { "type": "object", "properties": {
                    "created_at": { "type": "integer", "format": "int64" },
                    "last_active": { "type": "string", "format": "date-time" },
                }},
                "AdminEvent": { "type": "object", "properties": {
                    "timestamp": { "type": "string", "format": "date-time" },
                }},
                "Movie": { "type": "object", "properties": {
                    "release_date": { "type": ["string", "null"], "format": "date-time" },
                    "air_date": { "type": ["string", "null"] },
                }},
            }},
        });
        let mut r5 = r5_violations(&violating);
        r5.sort();
        assert_eq!(
            r5,
            vec![
                "/components/schemas/AdminEvent/properties/timestamp: a date-time not named *_at"
                    .to_owned(),
                "/components/schemas/AdminEvent/properties/timestamp: named as an epoch".to_owned(),
                "/components/schemas/Movie/properties/air_date: names a day but is not a date"
                    .to_owned(),
                "/components/schemas/Movie/properties/release_date: a date-time not named *_at"
                    .to_owned(),
                "/components/schemas/Movie/properties/release_date: names a day but is not a date"
                    .to_owned(),
                "/components/schemas/SessionSummary/properties/created_at: named *_at but not a \
                 date-time"
                    .to_owned(),
                "/components/schemas/SessionSummary/properties/last_active: a date-time not \
                 named *_at"
                    .to_owned(),
            ]
        );

        let conforming = json!({
            "components": { "schemas": {
                "SessionSummary": { "type": "object", "properties": {
                    "created_at": { "type": "string", "format": "date-time" },
                    "last_active_at": { "anyOf": [
                        { "type": "string", "format": "date-time" }, { "type": "null" },
                    ]},
                }},
                "Movie": { "type": "object", "properties": {
                    "release_date": { "type": ["string", "null"], "format": "date" },
                    "generated_on": { "type": "string", "format": "date" },
                }},
            }},
            "paths": { "/v1/report": { "get": { "parameters": [
                { "name": "from", "in": "query", "schema": { "type": "string", "format": "date" } },
            ]}}}
        });
        assert!(r5_violations(&conforming).is_empty());
    }

    #[test]
    fn r6_catches_a_measured_number_with_no_unit() {
        let violating = json!({
            "components": { "schemas": {
                "Library": { "type": "object", "properties": {
                    "size": { "type": "integer" },
                }},
                "Movie": { "type": "object", "properties": {
                    "runtime": { "type": ["integer", "null"] },
                    "duration": { "type": ["number", "null"] },
                    "episode_runtime": { "type": ["integer", "null"] },
                }},
            }},
            "paths": { "/v1/things": { "get": { "parameters": [
                { "name": "min_length", "in": "query", "schema": { "type": "integer" } },
            ]}}}
        });
        let mut r6 = r6_violations(&violating);
        r6.sort();
        assert_eq!(
            r6,
            vec![
                "/components/schemas/Library/properties/size".to_owned(),
                "/components/schemas/Movie/properties/duration".to_owned(),
                "/components/schemas/Movie/properties/episode_runtime".to_owned(),
                "/components/schemas/Movie/properties/runtime".to_owned(),
                "/paths/~1v1~1things/get/parameters/0".to_owned(),
            ]
        );

        let conforming = json!({
            "components": { "schemas": {
                "SizeBucket": { "type": "string", "enum": ["under_100_gib"] },
                "Library": { "type": "object", "properties": {
                    "file_count": { "type": "integer" },
                    // A bucket is a label, not a number: its unit is in its values.
                    "total_size": { "$ref": "#/components/schemas/SizeBucket" },
                }},
                "Movie": { "type": "object", "properties": {
                    "runtime_mins": { "type": ["integer", "null"] },
                    "duration_secs": { "type": ["number", "null"] },
                    "size_bytes": { "type": "integer" },
                    "position_secs": { "type": "number" },
                    "retention_days": { "type": "integer" },
                }},
            }},
        });
        assert!(r6_violations(&conforming).is_empty());
    }
}
