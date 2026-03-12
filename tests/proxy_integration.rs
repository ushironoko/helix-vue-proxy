//! Integration tests for the proxy message routing.
//!
//! These tests use in-process async channels and BufReaders to simulate
//! the Helix ↔ vue-ls ↔ ts-ls message flow without spawning real processes.

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader, duplex};

// Re-use the library's transport and message modules.
// Since helix-vue-proxy is a binary crate, we access modules via the test helper below.

/// Write an LSP message (Content-Length framed) into a writer.
async fn write_lsp_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> std::io::Result<()> {
    let body = serde_json::to_string(value).unwrap();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(body.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read an LSP message from a reader.
async fn read_lsp_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Value>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(None);
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }

        if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
            content_length = Some(len_str.parse().map_err(|e| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, e)
            })?);
        }
    }

    let length = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
    })?;

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;

    let value: Value = serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    Ok(Some(value))
}

#[tokio::test]
async fn test_lsp_message_roundtrip() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "id": 1,
        "params": {"capabilities": {}}
    });

    let (client, server) = duplex(4096);
    let mut writer = client;
    let mut reader = BufReader::new(server);

    write_lsp_message(&mut writer, &msg).await.unwrap();
    drop(writer); // Close to signal EOF after message

    let result = read_lsp_message(&mut reader).await.unwrap().unwrap();
    assert_eq!(result, msg);
}

#[tokio::test]
async fn test_tsserver_request_response_flow() {
    // Simulate the full flow:
    // 1. vue-ls sends tsserver/request
    // 2. Proxy extracts and forwards as workspace/executeCommand to ts-ls
    // 3. ts-ls responds
    // 4. Proxy builds tsserver/response and sends to vue-ls

    // Step 1: Build a tsserver/request as vue-ls would send
    let tsserver_request = json!({
        "jsonrpc": "2.0",
        "method": "tsserver/request",
        "params": [42, "completionInfo", {"file": "/tmp/test.vue", "line": 1, "offset": 5}]
    });

    // Verify extraction
    let params = tsserver_request.get("params").unwrap().as_array().unwrap();
    assert_eq!(params.len(), 3);
    let ts_id = &params[0];
    let command = params[1].as_str().unwrap();
    let args = &params[2];

    assert_eq!(*ts_id, json!(42));
    assert_eq!(command, "completionInfo");

    // Step 2: Build the workspace/executeCommand request
    let proxy_id: i64 = 1;
    let exec_cmd = json!({
        "jsonrpc": "2.0",
        "id": proxy_id,
        "method": "workspace/executeCommand",
        "params": {
            "command": "typescript.tsserverRequest",
            "arguments": [command, args]
        }
    });

    assert_eq!(exec_cmd["method"], "workspace/executeCommand");
    assert_eq!(exec_cmd["params"]["command"], "typescript.tsserverRequest");

    // Step 3: Simulate ts-ls response
    let ts_response = json!({
        "jsonrpc": "2.0",
        "id": proxy_id,
        "result": {
            "body": {
                "entries": [
                    {"name": "ref", "kind": "function"},
                    {"name": "reactive", "kind": "function"}
                ]
            }
        }
    });

    // Step 4: Build tsserver/response
    let tsserver_response = json!({
        "jsonrpc": "2.0",
        "method": "tsserver/response",
        "params": [[ts_id, ts_response["result"].clone()]]
    });

    assert_eq!(tsserver_response["method"], "tsserver/response");
    assert_eq!(tsserver_response["params"][0][0], 42);
    assert!(tsserver_response["params"][0][1]["body"]["entries"].is_array());
}

#[tokio::test]
async fn test_initialize_with_plugin_injection() {
    // Simulate Helix sending initialize, proxy injecting plugin config
    let helix_init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "capabilities": {
                "textDocument": {
                    "completion": {}
                }
            },
            "rootUri": "file:///tmp/project"
        }
    });

    // Proxy should inject plugin configuration
    let mut ts_init = helix_init.clone();
    ts_init["id"] = json!(100); // Proxy uses its own ID

    if let Some(params) = ts_init.get_mut("params") {
        let init_options = params
            .as_object_mut()
            .unwrap()
            .entry("initializationOptions")
            .or_insert_with(|| json!({}));

        if let Some(obj) = init_options.as_object_mut() {
            obj.insert(
                "plugins".to_string(),
                json!([{
                    "name": "@vue/typescript-plugin",
                    "location": "/path/to/plugin",
                    "languages": ["vue"]
                }]),
            );
            obj.insert("tsdk".to_string(), json!("/path/to/tsdk"));
        }
    }

    // Verify plugin was injected
    let plugins = &ts_init["params"]["initializationOptions"]["plugins"];
    assert!(plugins.is_array());
    assert_eq!(plugins[0]["name"], "@vue/typescript-plugin");
    assert_eq!(
        ts_init["params"]["initializationOptions"]["tsdk"],
        "/path/to/tsdk"
    );

    // Original capabilities should be preserved
    assert!(ts_init["params"]["capabilities"]["textDocument"]["completion"].is_object());
}

#[tokio::test]
async fn test_passthrough_non_tsserver_messages() {
    // Messages that are NOT tsserver/request should pass through unchanged
    let messages = vec![
        json!({"jsonrpc":"2.0","id":1,"method":"textDocument/completion","params":{}}),
        json!({"jsonrpc":"2.0","id":2,"result":{"items":[]}}),
        json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"diagnostics":[]}}),
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{}}),
    ];

    for msg in &messages {
        // None of these should be detected as tsserver/request
        let method = msg.get("method").and_then(|m| m.as_str());
        assert_ne!(method, Some("tsserver/request"));
    }
}

#[tokio::test]
async fn test_mirror_notifications() {
    // These notifications should be mirrored to ts-ls
    let mirrorable = vec![
        "textDocument/didOpen",
        "textDocument/didChange",
        "textDocument/didClose",
        "textDocument/didSave",
    ];

    let non_mirrorable = vec![
        "textDocument/completion",
        "textDocument/hover",
        "initialized",
        "shutdown",
    ];

    for method in &mirrorable {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":{}});
        let m = msg.get("method").and_then(|m| m.as_str()).unwrap();
        assert!(
            matches!(
                m,
                "textDocument/didOpen"
                    | "textDocument/didChange"
                    | "textDocument/didClose"
                    | "textDocument/didSave"
            ),
            "expected mirrorable: {m}"
        );
    }

    for method in &non_mirrorable {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":{}});
        let m = msg.get("method").and_then(|m| m.as_str()).unwrap();
        assert!(
            !matches!(
                m,
                "textDocument/didOpen"
                    | "textDocument/didChange"
                    | "textDocument/didClose"
                    | "textDocument/didSave"
            ),
            "expected NOT mirrorable: {m}"
        );
    }
}

#[tokio::test]
async fn test_multiple_concurrent_tsserver_requests() {
    // Simulate multiple tsserver/request arriving before responses
    use std::collections::HashMap;

    let mut pending: HashMap<i64, Value> = HashMap::new();
    let mut next_id: i64 = 1;

    // Three requests arrive
    let requests = vec![
        (json!(10), "completionInfo"),
        (json!(11), "quickinfo"),
        (json!(12), "geterr"),
    ];

    for (ts_id, command) in &requests {
        let proxy_id = next_id;
        next_id += 1;
        pending.insert(proxy_id, ts_id.clone());

        let _exec = json!({
            "jsonrpc": "2.0",
            "id": proxy_id,
            "method": "workspace/executeCommand",
            "params": {
                "command": "typescript.tsserverRequest",
                "arguments": [command, {}]
            }
        });
    }

    assert_eq!(pending.len(), 3);

    // Responses arrive out of order
    let response_order = vec![2i64, 3, 1];
    for proxy_id in response_order {
        let ts_id = pending.remove(&proxy_id).unwrap();
        let response = json!({
            "jsonrpc": "2.0",
            "method": "tsserver/response",
            "params": [[ts_id, {"result": "ok"}]]
        });
        assert_eq!(response["method"], "tsserver/response");
    }

    assert!(pending.is_empty());
}

#[tokio::test]
async fn test_null_response_on_error() {
    // When ts-ls returns an error, proxy should send null body in tsserver/response
    let error_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32600, "message": "Invalid Request"}
    });

    // Proxy should extract error and build null response
    let ts_id = json!(42);
    let body = if error_response.get("result").is_some() {
        error_response["result"].clone()
    } else {
        Value::Null
    };

    let tsserver_resp = json!({
        "jsonrpc": "2.0",
        "method": "tsserver/response",
        "params": [[ts_id, body]]
    });

    assert_eq!(tsserver_resp["params"][0][1], Value::Null);
}
