//! OpenAPI 3.1 document generated from the operation registry.
//!
//! Each operation's JSON schemas carry local `$defs`; they are hoisted into
//! `components/schemas` and references rewritten, so the document is valid and
//! code generators (the TypeScript client of the app) can use it.

use ancilo_core::Registry;
use serde_json::{Map, Value, json};

fn rewrite_refs(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(Value::String(r)) = map.get_mut("$ref")
                && let Some(name) = r.strip_prefix("#/$defs/")
            {
                *r = format!("#/components/schemas/{name}");
            }
            for (_, child) in map.iter_mut() {
                rewrite_refs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(rewrite_refs),
        _ => {}
    }
}

fn hoist(mut schema: Value, components: &mut Map<String, Value>) -> Value {
    if let Value::Object(map) = &mut schema {
        map.remove("$schema");
        if let Some(Value::Object(defs)) = map.remove("$defs") {
            for (name, mut def) in defs {
                rewrite_refs(&mut def);
                components.entry(name).or_insert(def);
            }
        }
    }
    rewrite_refs(&mut schema);
    schema
}

pub fn openapi(registry: &Registry, version: &str) -> Value {
    let mut components = Map::new();
    let mut paths = Map::new();
    for spec in registry.specs() {
        let input = hoist(spec.input_schema.clone(), &mut components);
        let output = hoist(spec.output_schema.clone(), &mut components);
        let error = json!({"$ref": "#/components/schemas/Error"});
        paths.insert(
            format!("/api/v1/ops/{}", spec.name),
            json!({
                "post": {
                    "operationId": spec.name,
                    "summary": spec.summary,
                    "description": spec.description,
                    "x-ancilo-permission": spec.permission,
                    "x-ancilo-consequential": spec.consequential,
                    "requestBody": {"required": true, "content": {"application/json": {"schema": input}}},
                    "responses": {
                        "200": {"description": "OK", "content": {"application/json": {"schema": output}}},
                        "default": {"description": "Error", "content": {"application/json": {"schema": error}}}
                    }
                }
            }),
        );
    }
    components.insert(
        "Error".into(),
        json!({
            "type": "object",
            "required": ["error"],
            "properties": {"error": {"type": "object", "required": ["code", "message"],
                "properties": {"code": {"type": "string"}, "message": {"type": "string"}}}}
        }),
    );
    json!({
        "openapi": "3.1.0",
        "info": {"title": "Ancilo control API", "version": version},
        "servers": [{"url": "http://127.0.0.1:7424"}],
        "security": [{"bearer": []}],
        "paths": paths,
        "components": {
            "schemas": components,
            "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}
        }
    })
}
