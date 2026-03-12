use serde_json::{Value, json};

/// Classification of an LSP JSON-RPC message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    Request,
    Response,
    Notification,
}

/// Classify a JSON-RPC message.
pub fn classify(value: &Value) -> MessageKind {
    if value.get("method").is_some() && value.get("id").is_some() {
        MessageKind::Request
    } else if value.get("id").is_some() {
        MessageKind::Response
    } else {
        MessageKind::Notification
    }
}

/// Check if a message is a `tsserver/request` notification.
pub fn is_tsserver_request(value: &Value) -> bool {
    value.get("method").and_then(|m| m.as_str()) == Some("tsserver/request")
}

/// Extract `(id, command, args)` from a `tsserver/request` notification.
///
/// Handles both formats:
/// - `params: [id, command, args]`
/// - `params: [[id, command, args]]` (Neovim double-wrap)
pub fn extract_tsserver_request(value: &Value) -> Option<(Value, String, Value)> {
    let params = value.get("params")?;
    let arr = params.as_array()?;

    // Try unwrapping double-nested: [[id, command, args]]
    let items = if arr.len() == 1 && arr[0].is_array() {
        arr[0].as_array()?
    } else {
        arr
    };

    if items.len() < 3 {
        return None;
    }

    let id = items[0].clone();
    let command = items[1].as_str()?.to_string();
    let args = items[2].clone();

    Some((id, command, args))
}

/// Build a `tsserver/response` notification to send back to vue-language-server.
pub fn build_tsserver_response(id: Value, body: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "tsserver/response",
        "params": [[id, body]]
    })
}

/// Build a `workspace/executeCommand` request for typescript-language-server.
pub fn build_execute_command_request(proxy_id: i64, command: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": proxy_id,
        "method": "workspace/executeCommand",
        "params": {
            "command": "typescript.tsserverRequest",
            "arguments": [command, args]
        }
    })
}

/// Check if a notification is one that should be mirrored to typescript-language-server.
pub fn is_mirrorable_notification(value: &Value) -> bool {
    let method = match value.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return false,
    };

    matches!(
        method,
        "textDocument/didOpen"
            | "textDocument/didChange"
            | "textDocument/didClose"
            | "textDocument/didSave"
    )
}

/// Check if a request is an `initialize` request.
pub fn is_initialize_request(value: &Value) -> bool {
    value.get("method").and_then(|m| m.as_str()) == Some("initialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_request() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        assert_eq!(classify(&msg), MessageKind::Request);
    }

    #[test]
    fn test_classify_response() {
        let msg = json!({"jsonrpc":"2.0","id":1,"result":{}});
        assert_eq!(classify(&msg), MessageKind::Response);
    }

    #[test]
    fn test_classify_notification() {
        let msg = json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{}});
        assert_eq!(classify(&msg), MessageKind::Notification);
    }

    #[test]
    fn test_is_tsserver_request() {
        let msg = json!({"jsonrpc":"2.0","method":"tsserver/request","params":[1,"geterr",{}]});
        assert!(is_tsserver_request(&msg));

        let msg = json!({"jsonrpc":"2.0","method":"textDocument/hover","params":{}});
        assert!(!is_tsserver_request(&msg));
    }

    #[test]
    fn test_extract_tsserver_request_flat() {
        let msg = json!({
            "jsonrpc":"2.0",
            "method":"tsserver/request",
            "params": [42, "geterr", {"files": ["/tmp/a.vue"]}]
        });

        let (id, command, args) = extract_tsserver_request(&msg).unwrap();
        assert_eq!(id, json!(42));
        assert_eq!(command, "geterr");
        assert_eq!(args, json!({"files": ["/tmp/a.vue"]}));
    }

    #[test]
    fn test_extract_tsserver_request_nested() {
        // Neovim-style double-wrapped params
        let msg = json!({
            "jsonrpc":"2.0",
            "method":"tsserver/request",
            "params": [[42, "geterr", {"files": ["/tmp/a.vue"]}]]
        });

        let (id, command, args) = extract_tsserver_request(&msg).unwrap();
        assert_eq!(id, json!(42));
        assert_eq!(command, "geterr");
        assert_eq!(args, json!({"files": ["/tmp/a.vue"]}));
    }

    #[test]
    fn test_build_tsserver_response() {
        let resp = build_tsserver_response(json!(42), json!({"body": "ok"}));
        assert_eq!(resp["method"], "tsserver/response");
        assert_eq!(resp["params"], json!([[42, {"body": "ok"}]]));
    }

    #[test]
    fn test_build_execute_command_request() {
        let req = build_execute_command_request(1, "geterr", json!({"files": []}));
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "workspace/executeCommand");
        assert_eq!(req["params"]["command"], "typescript.tsserverRequest");
        assert_eq!(req["params"]["arguments"], json!(["geterr", {"files": []}]));
    }

    #[test]
    fn test_is_mirrorable_notification() {
        for method in &[
            "textDocument/didOpen",
            "textDocument/didChange",
            "textDocument/didClose",
            "textDocument/didSave",
        ] {
            let msg = json!({"jsonrpc":"2.0","method":method,"params":{}});
            assert!(is_mirrorable_notification(&msg), "expected mirrorable: {method}");
        }

        let msg = json!({"jsonrpc":"2.0","method":"textDocument/hover","params":{}});
        assert!(!is_mirrorable_notification(&msg));
    }

    #[test]
    fn test_is_initialize_request() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        assert!(is_initialize_request(&msg));

        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialized","params":{}});
        assert!(!is_initialize_request(&msg));
    }
}
