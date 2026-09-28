//! The `/v1` wire conventions, asserted against the document the server
//! exports (issue #190).
//!
//! Every generated client -- the web SPA's types, `beam-client-core`, and the
//! Android and Apple apps above it -- is derived from this document, so a shape
//! it gets wrong is a shape every client inherits. These rules are what "the
//! `/v1` contract is consistent" means, checked rather than hoped for:
//!
//! * **R1** every enum value is `snake_case` (`^[a-z][a-z0-9_]*$`), so no
//!   client has to guess whether a value is `H264`, `Known` or `next_up`;
//! * **R2** no schema is named with a `Dto` or `Response` suffix -- the name
//!   is the thing on the wire, not the layer that produced it;
//! * **R2b** no two enums carry the same set of values. Two schemas meaning
//!   one thing (`AdminLogLevelDto` and `AdminEventLevelDto` were both
//!   `info`/`warning`/`error`) become two generated types a client has to
//!   convert between for nothing.
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

    /// Every `enum` in the document, anywhere -- components, parameters,
    /// inline properties -- beside the JSON pointer it was found at.
    fn enums(document: &Value) -> Vec<(String, Vec<Value>)> {
        fn walk(node: &Value, pointer: &str, found: &mut Vec<(String, Vec<Value>)>) {
            match node {
                Value::Object(object) => {
                    if let Some(Value::Array(values)) = object.get("enum") {
                        found.push((pointer.to_owned(), values.clone()));
                    }
                    for (key, child) in object {
                        let token = key.replace('~', "~0").replace('/', "~1");
                        walk(child, &format!("{pointer}/{token}"), found);
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

        let mut found = Vec::new();
        walk(document, "", &mut found);
        found
    }

    fn is_snake_case(value: &str) -> bool {
        let mut chars = value.chars();
        chars.next().is_some_and(|first| first.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    /// R1: every enum value that is not a `snake_case` string.
    fn r1_violations(document: &Value) -> Vec<String> {
        enums(document)
            .into_iter()
            .flat_map(|(pointer, values)| {
                values.into_iter().filter_map(move |value| match &value {
                    Value::String(text) if is_snake_case(text) => None,
                    Value::Null => None,
                    other => Some(format!("{pointer}: {other}")),
                })
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
        for (pointer, values) in enums(document) {
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

    #[test]
    fn r1_every_enum_value_is_snake_case() {
        let document = exported_document();
        assert!(
            !enums(&document).is_empty(),
            "the walk found no enums at all, so it is not reading the document"
        );
        let violations = r1_violations(&document);
        assert!(
            violations.is_empty(),
            "enum values must match ^[a-z][a-z0-9_]*$:\n{}",
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
            }},
            "paths": { "/v1/things": { "get": { "parameters": [
                { "name": "level", "in": "query",
                  "schema": { "type": ["string", "null"], "enum": ["warning", "info", "error", null] } }
            ]}}}
        });

        let mut r1 = r1_violations(&violating);
        r1.sort();
        assert_eq!(
            r1,
            vec![
                "/components/schemas/FileIndexStatus: \"2nd\"".to_owned(),
                "/components/schemas/FileIndexStatus: \"not-found\"".to_owned(),
                "/components/schemas/OutputVideoCodec: \"H264\"".to_owned(),
            ],
            "PascalCase, a leading digit and a hyphen are each caught; null is not a value"
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
            }}
        });
        assert!(r1_violations(&conforming).is_empty());
        assert!(r2_violations(&conforming).is_empty());
        assert!(r2b_violations(&conforming).is_empty());
    }
}
