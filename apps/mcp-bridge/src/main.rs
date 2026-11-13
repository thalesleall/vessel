//! `vessel-mcp`: forwards newline-delimited JSON-RPC from stdin to the Vessel app's local MCP
//! endpoint and writes the responses to stdout.
//!
//! Configuration: `--url` / `VESSEL_MCP_URL` (default `http://127.0.0.1:47011/mcp`) and
//! `--token` / `VESSEL_MCP_TOKEN` (copy it from Vessel → Agentes de IA).

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn arg(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == name {
            return args.next();
        }
        if let Some(v) = a.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn error_for(message: &Value, text: &str) -> Option<Value> {
    let id = message.get("id")?.clone();
    Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": text } }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url = arg("--url")
        .or_else(|| std::env::var("VESSEL_MCP_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:47011/mcp".into());
    let Some(token) = arg("--token").or_else(|| std::env::var("VESSEL_MCP_TOKEN").ok()) else {
        eprintln!("vessel-mcp: set VESSEL_MCP_TOKEN (Vessel → Agentes de IA → token)");
        std::process::exit(2);
    };
    let http = reqwest::Client::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Ok(Some(line)) = lines.next_line().await {
        // Some Windows shells prefix piped input with a UTF-8 BOM.
        let line = line.trim_start_matches('\u{feff}').to_string();
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let reply = match http
            .post(&url)
            .bearer_auth(&token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(line)
            .send()
            .await
        {
            Ok(resp) if resp.status().as_u16() == 202 => None,
            Ok(resp) if resp.status().is_success() => resp.text().await.ok(),
            Ok(resp) => error_for(&message, &format!("Vessel MCP returned HTTP {}", resp.status())).map(|v| v.to_string()),
            Err(_) => error_for(&message, "The Vessel app is not running, is locked, or MCP is disabled in Vessel → Agentes de IA.")
                .map(|v| v.to_string()),
        };
        if let Some(reply) = reply {
            let one_line = reply.replace('\n', "");
            if stdout.write_all(one_line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
            {
                break;
            }
            let _ = stdout.flush().await;
        }
    }
}
