//! Deterministic HTTP fixture for N4 browser QA. NOT an LLM or semantic-quality evaluation.
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
async fn fixture(Json(input): Json<Value>) -> Json<Value> {
    let data: Value =
        serde_json::from_str(input["messages"][1]["content"].as_str().unwrap_or("{}"))
            .unwrap_or(Value::Null);
    let source = data["source_text"].as_str().unwrap_or("");
    let content = if data["correction_target"].is_null() {
        json!({"subject":"user","predicate":"work:meeting_preference","value":"不参加超过30分钟的会议","polarity":"negative","modality":"asserted","conditions":{"scope:context":"工作"}})
    } else {
        let mut c = data["correction_target"].clone();
        c["value"] = json!("不参加超过60分钟的会议");
        c
    };
    let mut content = content;
    if source.contains("N2B 合成条件") {
        content["conditions"] = json!({"scope:context":"旅行"});
    }
    if source.contains("N2B 合成差异") {
        content["value"] = json!("不参加超过60分钟的会议");
    }
    // Fixed synthetic cases only. This is a protocol fixture, not semantic extraction.
    if data["correction_target"].is_null() && source.contains("N4 合成项目") {
        content = json!({"subject":"project:nova","predicate":"project:priority","value":"先把采集、整理和可追溯复用做顺畅","polarity":"positive","modality":"planned","conditions":{"scope:phase":"本地一期"}});
    }
    if data["correction_target"].is_null() && source.contains("N4 合成学习") {
        content = json!({"subject":"user","predicate":"learning:goal","value":"每周阅读一篇记忆系统论文","polarity":"positive","modality":"planned","conditions":{"scope:context":"研究"}});
    }
    if data["correction_target"].is_null() && source.contains("N4 合成生活") {
        content = json!({"subject":"user","predicate":"life:preference","value":"周末留出陪伴家人的时间","polarity":"positive","modality":"asserted","conditions":{}});
    }
    if data["correction_target"].is_null()
        && let Some(rest) = source.strip_prefix("N4 合成主题 ")
        && let Some(topic) = rest.split_whitespace().next()
        && topic.len() <= 3
        && topic.bytes().all(|c| c.is_ascii_digit())
    {
        content = json!({"subject":format!("project:fixture-{topic}"),"predicate":"project:direction","value":format!("合成主题 {topic} 的固定说明"),"polarity":"positive","modality":"asserted","conditions":{}});
    }
    let units = if !source.contains("合成") || source.contains("合成注入测试") {
        vec![]
    } else {
        vec![
            json!({"origin":"extracted","content":content,"occurred_at":null,"valid_time":{"kind":"unknown"},"quotes":[{"quote":source,"occurrence":0}]}),
        ]
    };
    Json(
        json!({"model":"synthetic-fixture-not-a-real-llm","choices":[{"finish_reason":"stop","message":{"content":json!({"schema_version":"nova.extraction.v0.1","units":units}).to_string()}}]}),
    )
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:31423".into());
    let address: std::net::SocketAddr = bind.parse()?;
    if !address.ip().is_loopback() {
        return Err("loopback required".into());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!(
        "SYNTHETIC HTTP FIXTURE ONLY (not an LLM): http://{}/v1",
        listener.local_addr()?
    );
    axum::serve(
        listener,
        Router::new().route("/v1/chat/completions", post(fixture)),
    )
    .await?;
    Ok(())
}
