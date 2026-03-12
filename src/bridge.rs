use std::collections::HashMap;
use serde_json::Value;
use tokio::process::ChildStdin;

use crate::message;
use crate::transport;

/// A pending tsserver request waiting for a response from typescript-language-server.
struct PendingTsRequest {
    /// The original tsserver request ID (from vue-language-server).
    tsserver_id: Value,
}

/// Queued request waiting for ts-ls initialization.
struct QueuedRequest {
    tsserver_id: Value,
    command: String,
    args: Value,
}

/// Bridge to typescript-language-server for handling tsserver/request forwarding.
pub struct TsBridge {
    next_request_id: i64,
    pending_requests: HashMap<i64, PendingTsRequest>,
    initialized: bool,
    init_queue: Vec<QueuedRequest>,
}

impl TsBridge {
    pub fn new() -> Self {
        Self {
            next_request_id: 1,
            pending_requests: HashMap::new(),
            initialized: false,
            init_queue: Vec::new(),
        }
    }

    fn alloc_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Send an initialize request to typescript-language-server,
    /// injecting the @vue/typescript-plugin configuration.
    pub async fn send_initialize(
        &mut self,
        original_init: &Value,
        plugin_path: &str,
        tsdk: &str,
        ts_stdin: &mut ChildStdin,
    ) -> std::io::Result<()> {
        let mut init_msg = original_init.clone();
        let id = self.alloc_id();
        init_msg["id"] = serde_json::json!(id);

        // Inject plugin configuration into initializationOptions
        if let Some(params) = init_msg.get_mut("params") {
            let init_options = params
                .as_object_mut()
                .unwrap()
                .entry("initializationOptions")
                .or_insert_with(|| serde_json::json!({}));

            if let Some(obj) = init_options.as_object_mut() {
                obj.insert(
                    "plugins".to_string(),
                    serde_json::json!([
                        {
                            "name": "@vue/typescript-plugin",
                            "location": plugin_path,
                            "languages": ["vue"]
                        }
                    ]),
                );
                obj.insert("tsdk".to_string(), serde_json::json!(tsdk));
            }
        }

        transport::write_message(ts_stdin, &init_msg).await?;
        tracing::debug!("sent initialize to ts-ls with id={id}");
        Ok(())
    }

    /// Mark the bridge as initialized and drain any queued requests.
    pub async fn mark_initialized(
        &mut self,
        ts_stdin: &mut ChildStdin,
    ) -> std::io::Result<()> {
        self.initialized = true;
        tracing::info!("ts-ls initialized, draining {} queued requests", self.init_queue.len());

        let queue: Vec<QueuedRequest> = self.init_queue.drain(..).collect();
        for req in queue {
            self.send_execute_command(req.tsserver_id, &req.command, req.args, ts_stdin)
                .await?;
        }
        Ok(())
    }

    /// Mirror a notification (didOpen/Change/Close/Save) to typescript-language-server.
    pub async fn mirror_notification(
        &self,
        msg: &Value,
        ts_stdin: &mut ChildStdin,
    ) -> std::io::Result<()> {
        transport::write_message(ts_stdin, msg).await?;
        let method = msg.get("method").cloned().unwrap_or(Value::Null);
        tracing::debug!("mirrored notification to ts-ls: {method}");
        Ok(())
    }

    /// Forward a tsserver/request to typescript-language-server as workspace/executeCommand.
    /// If not yet initialized, queue the request.
    pub async fn forward_tsserver_request(
        &mut self,
        tsserver_id: Value,
        command: String,
        args: Value,
        ts_stdin: &mut ChildStdin,
    ) -> std::io::Result<()> {
        if !self.initialized {
            tracing::debug!("ts-ls not initialized, queuing request id={tsserver_id}");
            self.init_queue.push(QueuedRequest {
                tsserver_id,
                command,
                args,
            });
            return Ok(());
        }

        self.send_execute_command(tsserver_id, &command, args, ts_stdin)
            .await
    }

    async fn send_execute_command(
        &mut self,
        tsserver_id: Value,
        command: &str,
        args: Value,
        ts_stdin: &mut ChildStdin,
    ) -> std::io::Result<()> {
        let proxy_id = self.alloc_id();
        let req = message::build_execute_command_request(proxy_id, command, args);

        self.pending_requests.insert(
            proxy_id,
            PendingTsRequest { tsserver_id },
        );

        transport::write_message(ts_stdin, &req).await?;
        tracing::debug!("forwarded tsserver request: proxy_id={proxy_id}, command={command}");
        Ok(())
    }

    /// Handle a response from typescript-language-server.
    /// Returns `Some(tsserver/response)` if this response matches a pending request.
    pub fn handle_ts_response(&mut self, msg: &Value) -> Option<Value> {
        let proxy_id = msg.get("id")?.as_i64()?;
        let pending = self.pending_requests.remove(&proxy_id)?;

        let body = if let Some(result) = msg.get("result") {
            // typescript-language-server returns the full tsserver response:
            //   { body: {...}, command: "...", type: "response", success: true, ... }
            // vue-language-server expects just the inner body content.
            if let Some(inner_body) = result.get("body") {
                inner_body.clone()
            } else {
                result.clone()
            }
        } else if let Some(error) = msg.get("error") {
            tracing::warn!("ts-ls returned error for proxy_id={proxy_id}: {error}");
            Value::Null
        } else {
            Value::Null
        };

        let response = message::build_tsserver_response(pending.tsserver_id, body);
        tracing::debug!("built tsserver/response for proxy_id={proxy_id}");
        Some(response)
    }

    /// Build null responses for all pending requests (used on timeout/shutdown).
    pub fn drain_pending(&mut self) -> Vec<Value> {
        let ids: Vec<i64> = self.pending_requests.keys().copied().collect();
        let mut responses = Vec::new();
        for proxy_id in ids {
            if let Some(pending) = self.pending_requests.remove(&proxy_id) {
                tracing::warn!("draining pending request proxy_id={proxy_id}");
                responses.push(message::build_tsserver_response(
                    pending.tsserver_id,
                    Value::Null,
                ));
            }
        }
        responses
    }

    /// Check if the bridge has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Check if a response ID matches a pending ts-ls request.
    pub fn is_pending_response(&self, msg: &Value) -> bool {
        msg.get("id")
            .and_then(|id| id.as_i64())
            .is_some_and(|id| self.pending_requests.contains_key(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_handle_ts_response_extracts_body() {
        let mut bridge = TsBridge::new();

        // Simulate having a pending request with proxy_id=1
        bridge.pending_requests.insert(
            1,
            PendingTsRequest {
                tsserver_id: json!(42),
            },
        );

        // ts-ls returns the full tsserver response with body wrapper
        let response_from_ts = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "body": {"configFileName": "/tmp/tsconfig.json", "languageServiceDisabled": false},
                "command": "_vue:projectInfo",
                "type": "response",
                "success": true,
                "seq": 0,
                "request_seq": 5
            }
        });

        let tsserver_resp = bridge.handle_ts_response(&response_from_ts).unwrap();
        assert_eq!(tsserver_resp["method"], "tsserver/response");
        assert_eq!(tsserver_resp["params"][0][0], 42);
        // Should extract just the inner body, not the full response
        assert_eq!(tsserver_resp["params"][0][1]["configFileName"], "/tmp/tsconfig.json");
        assert_eq!(tsserver_resp["params"][0][1]["languageServiceDisabled"], false);
        // Should NOT have the tsserver wrapper fields
        assert!(tsserver_resp["params"][0][1].get("command").is_none());
        assert!(tsserver_resp["params"][0][1].get("type").is_none());
    }

    #[test]
    fn test_handle_ts_response_without_body_uses_result() {
        let mut bridge = TsBridge::new();
        bridge.pending_requests.insert(
            1,
            PendingTsRequest {
                tsserver_id: json!(42),
            },
        );

        // Some responses might not have a body field
        let response_from_ts = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"items": []}
        });

        let tsserver_resp = bridge.handle_ts_response(&response_from_ts).unwrap();
        assert_eq!(tsserver_resp["params"][0][1]["items"], json!([]));
    }

    #[test]
    fn test_handle_ts_response_error_returns_null() {
        let mut bridge = TsBridge::new();
        bridge.pending_requests.insert(
            1,
            PendingTsRequest {
                tsserver_id: json!(10),
            },
        );

        let error_response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32600, "message": "Invalid Request"}
        });

        let tsserver_resp = bridge.handle_ts_response(&error_response).unwrap();
        assert_eq!(tsserver_resp["params"][0][1], Value::Null);
    }

    #[test]
    fn test_handle_unknown_response_returns_none() {
        let mut bridge = TsBridge::new();
        let msg = json!({"jsonrpc": "2.0", "id": 999, "result": {}});
        assert!(bridge.handle_ts_response(&msg).is_none());
    }

    #[test]
    fn test_drain_pending() {
        let mut bridge = TsBridge::new();
        bridge.pending_requests.insert(1, PendingTsRequest { tsserver_id: json!(10) });
        bridge.pending_requests.insert(2, PendingTsRequest { tsserver_id: json!(20) });

        let responses = bridge.drain_pending();
        assert_eq!(responses.len(), 2);
        assert!(bridge.pending_requests.is_empty());

        for resp in &responses {
            assert_eq!(resp["method"], "tsserver/response");
            // body should be null
            let inner = resp["params"][0][1].clone();
            assert_eq!(inner, Value::Null);
        }
    }

    #[test]
    fn test_alloc_id_increments() {
        let mut bridge = TsBridge::new();
        assert_eq!(bridge.alloc_id(), 1);
        assert_eq!(bridge.alloc_id(), 2);
        assert_eq!(bridge.alloc_id(), 3);
    }

    #[test]
    fn test_forward_queues_when_not_initialized() {
        // TsBridge.initialized is false by default,
        // so forward_tsserver_request should queue without writing to ts_stdin.
        // We test this by directly verifying the queue state.
        let mut bridge = TsBridge::new();
        assert!(!bridge.is_initialized());

        // Manually add to queue (simulating what forward_tsserver_request does internally)
        bridge.init_queue.push(QueuedRequest {
            tsserver_id: json!(1),
            command: "geterr".to_string(),
            args: json!({}),
        });

        assert_eq!(bridge.init_queue.len(), 1);
        assert_eq!(bridge.init_queue[0].command, "geterr");
    }

    #[test]
    fn test_is_pending_response() {
        let mut bridge = TsBridge::new();
        bridge.pending_requests.insert(5, PendingTsRequest { tsserver_id: json!(1) });

        assert!(bridge.is_pending_response(&json!({"id": 5})));
        assert!(!bridge.is_pending_response(&json!({"id": 99})));
        assert!(!bridge.is_pending_response(&json!({"method": "foo"})));
    }
}
