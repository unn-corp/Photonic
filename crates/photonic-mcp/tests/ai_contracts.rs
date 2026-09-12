//! Contracts seen by catalog discovery, native MCP clients, and legacy text
//! parsers. These exercise real registry entries and handler output together.

use photonic_mcp::catalog::{
    all_tools, cached_tools, compact_tool_list, search_actions, tool_schema,
};
use photonic_mcp::dispatch::dispatch_tool;
use photonic_mcp::protocol::{ContentItem, ToolErrorData, ToolResult};
use photonic_mcp::schema_gen::{tool_definition, ToolBehavior};
use photonic_mcp::server::AppState;
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use std::collections::HashSet;

fn legacy_data(result: &ToolResult) -> Value {
    result
        .content
        .iter()
        .skip(1)
        .find_map(|item| match item {
            ContentItem::Text { text } => serde_json::from_str(text).ok(),
            _ => None,
        })
        .expect("the legacy JSON text block remains available")
}

#[test]
fn discovery_preserves_complete_definitions() {
    for name in [
        "create_shape",
        "build_shape_from_points",
        "batch_set_keyframes",
        "create_sequence",
        "get_job_status",
    ] {
        let result = search_actions(name, 1);
        let mut hit = result["actions"][0].clone();
        assert!(hit["score"].as_i64().unwrap() > 0);
        hit.as_object_mut().unwrap().remove("score");
        assert_eq!(&hit, tool_schema(name).unwrap(), "schema lost for {name}");
    }
    let points = search_actions("build_shape_from_points", 1);
    let item = &points["actions"][0]["inputSchema"]["properties"]["points"]["items"];
    assert_eq!(item["minItems"], 2);
    assert_eq!(item["maxItems"], 2);
    assert_eq!(item["items"]["type"], "number");

    let keyframes = search_actions("batch_set_keyframes", 1);
    let item = &keyframes["actions"][0]["inputSchema"]["properties"]["ops"]["items"];
    assert_eq!(
        item["properties"]["target"]["enum"],
        json!(["clip_transform", "clip_effect"])
    );
    assert!(item["required"]
        .as_array()
        .unwrap()
        .contains(&json!("target")));
}

#[test]
fn registry_lookup_is_borrowed_and_owned_copies_are_isolated() {
    assert!(std::ptr::eq(cached_tools(), cached_tools()));
    let tool = tool_schema("create_sequence").unwrap();
    assert!(std::ptr::eq(tool, tool_schema("create_sequence").unwrap()));
    assert!(tool_schema("CREATE_SEQUENCE").is_none());
    assert!(tool_schema("missing_action").is_none());
    let mut owned = all_tools();
    owned[0]["name"] = json!("changed by caller");
    assert_ne!(cached_tools()[0]["name"], "changed by caller");
}

#[test]
fn compact_discovery_and_search_are_bounded_and_deterministic() {
    let compact = compact_tool_list();
    let definitions = compact.as_array().unwrap();
    let names: HashSet<_> = definitions
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), definitions.len());
    for name in ["search_actions", "get_action_schema", "execute_action"] {
        assert!(names.contains(name));
        assert_eq!(
            definitions
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap(),
            tool_schema(name).unwrap()
        );
    }
    assert_eq!(search_actions("   ", 50)["count"], 0);
    assert_eq!(search_actions("clip", 0)["count"], 1);
    assert_eq!(search_actions("clip", usize::MAX)["count"], 50);
    assert_eq!(search_actions("CLIP", 8), search_actions("CLIP", 8));
    assert_eq!(
        search_actions("CLIP", 8)["actions"],
        search_actions("clip", 8)["actions"]
    );
}

#[test]
fn native_data_matches_legacy_json_for_every_json_shape() {
    for data in [
        json!({ "clip_id": "clip-1" }),
        json!([1, 2]),
        json!("text"),
        json!(7),
        json!(true),
        Value::Null,
    ] {
        let result = ToolResult::text("completed").with_data(data.clone());
        assert_eq!(result.content.len(), 2);
        assert_eq!(legacy_data(&result), data);
        assert_eq!(result.structured_content.as_ref(), Some(&data));
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["structuredContent"], data);
        assert!(wire.get("data_content_index").is_none());
        assert!(wire.get("isError").is_none());
    }
}

#[test]
fn text_and_image_content_positions_remain_compatible() {
    let result = ToolResult::text("captured").with_image("base64-png".into());
    assert_eq!(
        result.structured_content,
        Some(json!({ "message": "captured" }))
    );
    assert!(matches!(&result.content[0], ContentItem::Text { text } if text == "captured"));
    assert!(matches!(&result.content[1], ContentItem::Image { data, .. } if data == "base64-png"));
    let result = result.with_data(json!({ "width": 1920, "height": 1080 }));
    assert!(matches!(&result.content[1], ContentItem::Image { .. }));
    assert_eq!(legacy_data(&result), result.structured_content.unwrap());
}

#[test]
fn repeated_data_keeps_one_current_compatibility_block() {
    let result = ToolResult::text("completed")
        .with_data(json!({ "value": 1 }))
        .with_data(json!({ "value": 2 }));
    assert_eq!(result.content.len(), 2);
    assert_eq!(legacy_data(&result), json!({ "value": 2 }));
    assert_eq!(result.structured_content, Some(json!({ "value": 2 })));
}

#[test]
fn error_fields_are_typed_and_domain_diagnostics_survive_merging() {
    let result = ToolResult::error("outside the clip")
        .with_data(json!({ "error_code": "TickOutOfRange", "clip_id": "clip-1" }))
        .with_data(json!({ "min_ticks": 0, "max_ticks": 100 }))
        .with_data(json!({ "error_code": null, "message": ["invalid type"] }));
    assert_eq!(result.content.len(), 2);
    let legacy = legacy_data(&result);
    assert_eq!(result.structured_content.as_ref(), Some(&legacy));
    let error: ToolErrorData = serde_json::from_value(legacy).unwrap();
    assert_eq!(error.error_code, "TickOutOfRange");
    assert_eq!(error.message, "outside the clip");
    assert_eq!(error.details["clip_id"], "clip-1");
    assert_eq!(error.details["max_ticks"], 100);
    assert_eq!(serde_json::to_value(result).unwrap()["isError"], true);

    let generic = ToolResult::error("missing clip");
    let data: ToolErrorData = serde_json::from_value(legacy_data(&generic)).unwrap();
    assert_eq!(data.error_code, "ToolExecutionError");
    assert_eq!(data.message, "missing clip");

    let scalar = ToolResult::error("failed").with_data(json!(["diagnostic"]));
    assert_eq!(legacy_data(&scalar)["details"], json!(["diagnostic"]));
}

#[test]
fn serialization_failure_is_an_explicit_error() {
    struct Broken;
    impl Serialize for Broken {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("fixture refuses serialization"))
        }
    }
    let result = ToolResult::text("cannot report success").with_data(Broken);
    assert_eq!(result.is_error, Some(true));
    let data: ToolErrorData = serde_json::from_value(legacy_data(&result)).unwrap();
    assert_eq!(data.error_code, "SerializationFailed");
    assert!(data.message.contains("fixture refuses serialization"));
}

#[test]
fn annotations_account_for_jobs_files_and_session_state() {
    for name in [
        "search_actions",
        "get_action_schema",
        "list_clips",
        "get_clip",
        "get_job_status",
    ] {
        assert_eq!(
            tool_schema(name).unwrap()["annotations"]["readOnlyHint"],
            true,
            "{name}"
        );
    }
    for name in [
        "play",
        "pause",
        "seek",
        "step",
        "cancel_job",
        "export_sequence",
        "auto_caption",
        "effect_preset_list",
        "effect_favourite_list",
    ] {
        assert_eq!(
            tool_schema(name).unwrap()["annotations"]["readOnlyHint"],
            false,
            "{name}"
        );
    }
    assert_eq!(
        tool_schema("export_sequence").unwrap()["annotations"]["openWorldHint"],
        true
    );
    assert_eq!(
        tool_schema("create_sequence").unwrap()["annotations"]["destructiveHint"],
        false
    );
    // A forwarding tool cannot promise its target's success shape.
    assert!(tool_schema("execute_action")
        .unwrap()
        .get("outputSchema")
        .is_none());
}

#[test]
fn definition_helper_adds_standard_errors_without_weakening_success_schema() {
    let success = json!({ "type": "object", "properties": { "revision": { "type": "integer", "minimum": 0 } }, "required": ["revision"] });
    let definition = tool_definition(
        "inspect_revision",
        "Read the revision.",
        json!({ "type": "object", "additionalProperties": false }),
        success.clone(),
        ToolBehavior::ReadOnly,
    );
    assert_eq!(definition["outputSchema"]["anyOf"][0], success);
    assert_eq!(
        definition["outputSchema"]["anyOf"][1]["required"],
        json!(["error_code", "message"])
    );
    assert_eq!(definition["annotations"]["readOnlyHint"], true);
}

/// Cross-check required output fields against real successful handler payloads.
/// Full JSON Schema validation is performed separately on the generated catalog.
async fn successful_call(state: &AppState, name: &str, arguments: Value) -> Value {
    let result = dispatch_tool(state, name, arguments).await.unwrap();
    assert_ne!(result.is_error, Some(true), "{name}: {result:?}");
    let data = result.structured_content.as_ref().unwrap();
    let schema = &tool_schema(name).unwrap()["outputSchema"]["anyOf"][0];
    for field in schema["required"]
        .as_array()
        .expect("this fixture has an object success schema")
    {
        assert!(
            data.get(field.as_str().unwrap()).is_some(),
            "{name} omitted required {field}: {data}"
        );
    }
    if result.content.len() > 1 {
        assert_eq!(&legacy_data(&result), data, "{name}");
    }
    data.clone()
}

#[tokio::test]
async fn common_and_video_handlers_publish_the_advertised_fields() {
    let state = AppState::headless_for_test();
    let definition = dispatch_tool(
        &state,
        "get_action_schema",
        json!({ "name": "batch_set_keyframes" }),
    )
    .await
    .unwrap();
    assert_eq!(
        definition.structured_content.as_ref(),
        tool_schema("batch_set_keyframes")
    );
    for name in [
        "get_document_info",
        "get_document_state",
        "list_artboards",
        "list_sequences",
        "list_media",
        "list_clips",
    ] {
        successful_call(&state, name, json!({})).await;
    }
    let seq = successful_call(
        &state,
        "create_sequence",
        json!({
            "name": "contract fixture", "frame_rate": { "num": 30, "den": 1 },
            "formats": [{ "name": "Main", "width": 1920, "height": 1080 }]
        }),
    )
    .await;
    let track = successful_call(
        &state,
        "add_track",
        json!({ "sequence_id": seq["sequence_id"], "kind": "video" }),
    )
    .await;
    let clip = successful_call(
        &state,
        "insert_clip",
        json!({
            "track_id": track["track_id"], "source": { "kind": "adjustment" },
            "start_ticks": 0, "duration_ticks": 48000
        }),
    )
    .await;
    successful_call(&state, "get_clip", json!({ "clip_id": clip["clip_id"] })).await;
    let clips = successful_call(
        &state,
        "list_clips",
        json!({ "sequence_id": seq["sequence_id"] }),
    )
    .await;
    assert_eq!(clips["clips"][0]["clip_id"], clip["clip_id"]);
    successful_call(
        &state,
        "split_clip",
        json!({ "clip_id": clip["clip_id"], "at_ticks": 24000 }),
    )
    .await;
    successful_call(&state, "undo", json!({})).await;

    let error = dispatch_tool(
        &state,
        "split_clip",
        json!({ "clip_id": clip["clip_id"], "at_ticks": -1 }),
    )
    .await
    .unwrap();
    assert_eq!(error.is_error, Some(true));
    assert_eq!(
        error.structured_content.as_ref().unwrap()["error_code"],
        "TickOutOfRange"
    );
    assert_eq!(legacy_data(&error), error.structured_content.unwrap());
}
