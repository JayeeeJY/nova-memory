//! Metadata-only checks of an already selected private local MCP connection.
//! Never calls a memory tool, follows a supplied URL, or grants client authority.
use crate::local_mcp_bridge::{LocalConnection, forward, http_client};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

const PROTOCOL: &str = "2025-11-25";
const MAX_TOOLS: usize = 32;

#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub status: &'static str,
    pub tool_count: usize,
    pub host_status: &'static str,
    pub memory_accessed: bool,
    pub model_calls: u8,
}

fn result(status: &'static str, tool_count: usize) -> Check {
    Check {
        status,
        tool_count,
        host_status: "not_verified",
        memory_accessed: false,
        model_calls: 0,
    }
}

fn valid_tools(reply: &Value) -> Option<usize> {
    if reply["jsonrpc"] != "2.0"
        || reply["id"] != "nova-diagnostic-tools"
        || reply.get("error").is_some()
        || reply["result"].get("nextCursor").is_some()
    {
        return None;
    }
    let tools = reply["result"]["tools"].as_array()?;
    if tools.len() > MAX_TOOLS {
        return None;
    }
    let mut names = std::collections::BTreeSet::new();
    for tool in tools {
        let name = tool["name"].as_str()?;
        if name.is_empty()
            || name.len() > 128
            || !names.insert(name)
            || tool["inputSchema"]["type"] != "object"
            || tool["outputSchema"]["type"] != "object"
        {
            return None;
        }
    }
    for (name, fields) in [
        (
            "save_memory",
            &[
                ("request_id", "string"),
                ("content", "string"),
                ("user_requested_save", "boolean"),
            ][..],
        ),
        (
            "get_memory",
            &[("request_id", "string"), ("memory_id", "string")][..],
        ),
        (
            "search_memory",
            &[("request_id", "string"), ("query", "string")][..],
        ),
    ] {
        let tool = tools.iter().find(|t| t["name"] == name)?;
        let schema = &tool["inputSchema"];
        let required = schema["required"].as_array()?;
        if schema["additionalProperties"] != false
            || required.len() != fields.len()
            || schema["properties"].as_object()?.len()
                != fields.len() + usize::from(name == "search_memory")
            || (name == "search_memory" && schema["properties"]["max_items"]["type"] != "integer")
            || fields.iter().any(|(field, kind)| {
                schema["properties"][*field]["type"] != *kind
                    || !required.iter().any(|r| r == field)
            })
        {
            return None;
        }
    }
    Some(tools.len())
}

pub(crate) async fn check(config: &LocalConnection) -> Check {
    let attempt = async {
        let client = http_client().map_err(|_| "transport_unavailable")?;
        let init = json!({"jsonrpc":"2.0","id":"nova-diagnostic-init","method":"initialize","params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{"name":"nova-local-connection-check","version":"1"}}});
        let reply = forward(&client, config, &init, None)
            .await
            .map_err(|_| "transport_unavailable")?
            .ok_or("server_contract_mismatch")?;
        if reply["jsonrpc"] != "2.0"
            || reply["id"] != "nova-diagnostic-init"
            || reply.get("error").is_some()
            || reply["result"]["protocolVersion"] != PROTOCOL
            || reply["result"]["serverInfo"]["name"] != "nova-memory-automatic"
            || reply["result"]["serverInfo"]["version"] != "0.3.0"
            || !reply["result"]["capabilities"]["tools"].is_object()
        {
            return Err("server_contract_mismatch");
        }
        let initialized = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        forward(&client, config, &initialized, Some(PROTOCOL))
            .await
            .map_err(|_| "transport_unavailable")?;
        let list =
            json!({"jsonrpc":"2.0","id":"nova-diagnostic-tools","method":"tools/list","params":{}});
        let reply = forward(&client, config, &list, Some(PROTOCOL))
            .await
            .map_err(|_| "transport_unavailable")?
            .ok_or("tool_schema_invalid")?;
        valid_tools(&reply).ok_or("tool_schema_invalid")
    };
    match tokio::time::timeout(Duration::from_secs(8), attempt).await {
        Ok(Ok(count)) => result("ready", count),
        Ok(Err(code)) => result(code, 0),
        Err(_) => result("timeout", 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply() -> Value {
        let mut tools = Vec::new();
        for (name, fields) in [
            (
                "save_memory",
                vec![
                    ("request_id", "string"),
                    ("content", "string"),
                    ("user_requested_save", "boolean"),
                ],
            ),
            (
                "get_memory",
                vec![("request_id", "string"), ("memory_id", "string")],
            ),
            (
                "search_memory",
                vec![("request_id", "string"), ("query", "string")],
            ),
        ] {
            let properties: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(key, kind)| (key.to_string(), json!({"type":kind})))
                .collect();
            let mut tool = json!({"name":name,"inputSchema":{"type":"object","additionalProperties":false,"properties":properties,"required":fields.iter().map(|(key,_)|*key).collect::<Vec<_>>()},"outputSchema":{"type":"object"}});
            if name == "search_memory" {
                tool["inputSchema"]["properties"]["max_items"] = json!({"type":"integer"});
            }
            tools.push(tool);
        }
        json!({"jsonrpc":"2.0","id":"nova-diagnostic-tools","result":{"tools":tools}})
    }
    #[test]
    fn detects_the_connected_but_unloadable_schema_failure() {
        let mut data = reply();
        assert_eq!(valid_tools(&data), Some(3));
        data["result"]["tools"][0]["outputSchema"] = json!({});
        assert_eq!(valid_tools(&data), None);
    }
    #[test]
    fn rejects_partial_duplicate_missing_or_wrong_basic_inputs() {
        for mode in 0..10 {
            let mut data = reply();
            match mode {
                0 => data["result"]["nextCursor"] = json!("more"),
                1 => data["result"]["tools"][1]["name"] = json!("save_memory"),
                2 => {
                    data["result"]["tools"].as_array_mut().unwrap().pop();
                }
                3 => {
                    data["result"]["tools"][0]["inputSchema"]["properties"]["user_requested_save"]
                        ["type"] = json!("string")
                }
                4 => data["result"]["tools"][0]["inputSchema"]["required"] = json!([]),
                5 => {
                    data["result"]["tools"][0]["inputSchema"]["additionalProperties"] = json!(true)
                }
                6 => data["error"] = json!({"code":-1}),
                7 => {
                    data["result"]["tools"][2]["inputSchema"]["properties"]["max_items"]["type"] =
                        json!("string")
                }
                8 => data["result"]["tools"][0]["inputSchema"]["required"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("owner_id")),
                _ => {
                    data["result"]["tools"][0]["inputSchema"]["properties"]["owner_id"] =
                        json!({"type":"string"})
                }
            }
            assert_eq!(valid_tools(&data), None, "mode {mode}");
        }
    }

    #[tokio::test]
    async fn probe_uses_only_metadata_and_bounds_failed_or_stalled_services() {
        use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
        use std::sync::{Arc, Mutex};
        for mode in 0..4 {
            let methods = Arc::new(Mutex::new(Vec::new()));
            let observed = methods.clone();
            let app = Router::new().route("/mcp/v03", post(move |Json(input): Json<Value>| {
                let observed = observed.clone();
                async move {
                    let method = input["method"].as_str().unwrap().to_owned();
                    observed.lock().unwrap().push(method.clone());
                    if mode == 3 { tokio::time::sleep(Duration::from_secs(20)).await; }
                    match method.as_str() {
                        "initialize" => Json(json!({"jsonrpc":"2.0","id":input["id"],"result":{"protocolVersion":PROTOCOL,"serverInfo":{"name":if mode==1 {"wrong-server"}else{"nova-memory-automatic"},"version":"0.3.0"},"capabilities":{"tools":{}}}})).into_response(),
                        "tools/list" => {
                            let mut output = reply();
                            if mode == 2 { output["result"]["tools"][0]["outputSchema"] = json!({}); }
                            Json(output).into_response()
                        },
                        _ => StatusCode::ACCEPTED.into_response(),
                    }
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, app).await });
            let config = LocalConnection::new(
                "connection_test".into(),
                format!("http://{address}/mcp/v03"),
                "t".repeat(32),
            );
            let checked = check(&config).await;
            assert_eq!(
                checked.status,
                [
                    "ready",
                    "server_contract_mismatch",
                    "tool_schema_invalid",
                    "timeout"
                ][mode]
            );
            assert!(!checked.memory_accessed);
            assert_eq!(checked.model_calls, 0);
            assert!(methods.lock().unwrap().iter().all(|m| {
                ["initialize", "notifications/initialized", "tools/list"].contains(&m.as_str())
            }));
            if mode == 0 {
                assert_eq!(methods.lock().unwrap().len(), 3);
            }
            task.abort();
        }
    }
}
