use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::bridge::TsBridge;
use crate::message;
use crate::transport;

/// Spawn a task that drains a child process's stderr and logs each line.
fn drain_stderr(name: &'static str, stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    tracing::debug!("{name} stderr: {}", line.trim_end());
                }
                Err(e) => {
                    tracing::warn!("{name} stderr read error: {e}");
                    break;
                }
            }
        }
    });
}

/// Build a JSON-RPC response for a ts-ls request that the proxy handles locally.
fn build_response(id: &Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

/// Run the LSP proxy.
///
/// Spawns vue-language-server and typescript-language-server as child processes,
/// then routes messages between Helix (stdin/stdout), vue-ls, and ts-ls.
pub async fn run(
    vue_server_path: &str,
    ts_server_path: &str,
    plugin_path: &str,
    tsdk: &str,
) -> anyhow::Result<()> {
    // Spawn vue-language-server
    let mut vue_child = Command::new(vue_server_path)
        .arg("--stdio")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn vue-language-server ({vue_server_path}): {e}"))?;

    tracing::info!("spawned vue-language-server (pid={})", vue_child.id().unwrap_or(0));

    // Spawn typescript-language-server
    let mut ts_child = Command::new(ts_server_path)
        .arg("--stdio")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn typescript-language-server ({ts_server_path}): {e}"))?;

    tracing::info!("spawned typescript-language-server (pid={})", ts_child.id().unwrap_or(0));

    // Drain child stderr to our tracing log
    if let Some(vue_stderr) = vue_child.stderr.take() {
        drain_stderr("vue-ls", vue_stderr);
    }
    if let Some(ts_stderr) = ts_child.stderr.take() {
        drain_stderr("ts-ls", ts_stderr);
    }

    let mut vue_stdin = vue_child.stdin.take().expect("vue-ls stdin");
    let vue_stdout = vue_child.stdout.take().expect("vue-ls stdout");
    let mut ts_stdin = ts_child.stdin.take().expect("ts-ls stdin");
    let ts_stdout = ts_child.stdout.take().expect("ts-ls stdout");

    let helix_stdin = tokio::io::stdin();
    let mut helix_stdout = tokio::io::stdout();

    let mut vue_reader = BufReader::new(vue_stdout);
    let mut ts_reader = BufReader::new(ts_stdout);
    let mut helix_reader = BufReader::new(helix_stdin);

    let mut bridge = TsBridge::new();
    let plugin_path = plugin_path.to_string();
    let tsdk = tsdk.to_string();

    // Track whether we've sent initialize to ts-ls
    let mut ts_init_sent = false;

    loop {
        tokio::select! {
            // Messages from Helix (stdin)
            msg = transport::read_message(&mut helix_reader) => {
                match msg {
                    Ok(Some(msg)) => {
                        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("(response)");
                        let id = msg.get("id");
                        tracing::debug!("helix -> vue-ls: method={method} id={id:?}");

                        // Forward everything to vue-ls
                        if let Err(e) = transport::write_message(&mut vue_stdin, &msg).await {
                            tracing::error!("failed to write to vue-ls stdin: {e}");
                            break;
                        }

                        // Mirror initialize to ts-ls
                        if message::is_initialize_request(&msg) && !ts_init_sent {
                            tracing::info!("mirroring initialize to ts-ls");
                            if let Err(e) = bridge.send_initialize(&msg, &plugin_path, &tsdk, &mut ts_stdin).await {
                                tracing::error!("failed to send initialize to ts-ls: {e}");
                            } else {
                                ts_init_sent = true;
                            }
                        }

                        // Mirror didOpen/Change/Close/Save to ts-ls
                        if message::is_mirrorable_notification(&msg) {
                            tracing::debug!("mirroring {method} to ts-ls");
                            if let Err(e) = bridge.mirror_notification(&msg, &mut ts_stdin).await {
                                tracing::warn!("failed to mirror {method} to ts-ls: {e}");
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::info!("helix stdin closed, shutting down");
                        break;
                    }
                    Err(e) => {
                        tracing::error!("error reading from helix stdin: {e}");
                        break;
                    }
                }
            }

            // Messages from vue-language-server
            msg = transport::read_message(&mut vue_reader) => {
                match msg {
                    Ok(Some(msg)) => {
                        if message::is_tsserver_request(&msg) {
                            // Intercept tsserver/request
                            if let Some((id, command, args)) = message::extract_tsserver_request(&msg) {
                                tracing::info!("intercepted tsserver/request: command={command} id={id}");
                                if let Err(e) = bridge.forward_tsserver_request(id.clone(), command.clone(), args, &mut ts_stdin).await {
                                    tracing::error!("failed to forward tsserver/request ({command}): {e}");
                                    // Send null response back to vue-ls so it doesn't hang
                                    let null_resp = message::build_tsserver_response(id, Value::Null);
                                    let _ = transport::write_message(&mut vue_stdin, &null_resp).await;
                                }
                            } else {
                                tracing::warn!("failed to extract tsserver/request params: {msg}");
                            }
                        } else {
                            // Forward everything else to Helix
                            let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("(response)");
                            let id = msg.get("id");
                            tracing::debug!("vue-ls -> helix: method={method} id={id:?}");
                            if let Err(e) = transport::write_message(&mut helix_stdout, &msg).await {
                                tracing::error!("failed to write to helix stdout: {e}");
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::error!("vue-ls stdout closed unexpectedly, shutting down");
                        break;
                    }
                    Err(e) => {
                        tracing::error!("error reading from vue-ls stdout: {e}");
                        break;
                    }
                }
            }

            // Messages from typescript-language-server
            msg = transport::read_message(&mut ts_reader) => {
                match msg {
                    Ok(Some(msg)) => {
                        let method = msg.get("method").and_then(|m| m.as_str());
                        let msg_id = msg.get("id");
                        tracing::debug!("ts-ls message: method={method:?} id={msg_id:?}");

                        // Check if this is a response to our initialize
                        if !bridge.is_initialized() && is_initialize_response(&msg) {
                            tracing::info!("ts-ls initialize response received");
                            // Send initialized notification
                            let initialized = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "initialized",
                                "params": {}
                            });
                            if let Err(e) = transport::write_message(&mut ts_stdin, &initialized).await {
                                tracing::error!("failed to send initialized to ts-ls: {e}");
                            }
                            if let Err(e) = bridge.mark_initialized(&mut ts_stdin).await {
                                tracing::error!("failed to drain init queue: {e}");
                            }
                            continue;
                        }

                        // Handle requests FROM ts-ls that need a response
                        if let (Some(method_str), Some(id)) = (method, msg_id) {
                            let response = match method_str {
                                "workspace/configuration" => {
                                    // ts-ls asks for workspace config. Return empty configs
                                    // for each requested section.
                                    let items = msg.get("params")
                                        .and_then(|p| p.get("items"))
                                        .and_then(|i| i.as_array())
                                        .map(|arr| arr.len())
                                        .unwrap_or(1);
                                    let results: Vec<Value> = (0..items)
                                        .map(|_| serde_json::json!({}))
                                        .collect();
                                    tracing::debug!("responding to ts-ls workspace/configuration with {items} empty configs");
                                    Some(build_response(id, Value::Array(results)))
                                }
                                "window/workDoneProgress/create" => {
                                    tracing::debug!("responding to ts-ls window/workDoneProgress/create");
                                    Some(build_response(id, Value::Null))
                                }
                                "client/registerCapability" => {
                                    tracing::debug!("responding to ts-ls client/registerCapability");
                                    Some(build_response(id, Value::Null))
                                }
                                "window/showMessageRequest" => {
                                    tracing::debug!("responding to ts-ls window/showMessageRequest");
                                    Some(build_response(id, Value::Null))
                                }
                                _ => {
                                    tracing::warn!("unhandled ts-ls request: {method_str} id={id}");
                                    // Send null result for unknown requests to prevent ts-ls from hanging
                                    Some(build_response(id, Value::Null))
                                }
                            };

                            if let Some(resp) = response {
                                if let Err(e) = transport::write_message(&mut ts_stdin, &resp).await {
                                    tracing::error!("failed to respond to ts-ls {method_str}: {e}");
                                }
                            }
                            continue;
                        }

                        // Check if this matches a pending tsserver request
                        if let Some(response) = bridge.handle_ts_response(&msg) {
                            tracing::info!("sending tsserver/response to vue-ls: {}", truncate_log(&response));
                            if let Err(e) = transport::write_message(&mut vue_stdin, &response).await {
                                tracing::error!("failed to send tsserver/response to vue-ls: {e}");
                            }
                        }
                        // Drop notifications from ts-ls (diagnostics, progress, etc.)
                    }
                    Ok(None) => {
                        tracing::warn!("ts-ls stdout closed");
                        // Drain pending requests with null responses
                        for response in bridge.drain_pending() {
                            let _ = transport::write_message(&mut vue_stdin, &response).await;
                        }
                        // Don't break — vue-ls can still work without ts-ls
                    }
                    Err(e) => {
                        tracing::error!("error reading from ts-ls stdout: {e}");
                    }
                }
            }
        }
    }

    // Cleanup: kill child processes
    tracing::info!("shutting down child processes");
    let _ = vue_child.kill().await;
    let _ = ts_child.kill().await;

    Ok(())
}

fn is_initialize_response(msg: &Value) -> bool {
    msg.get("result")
        .and_then(|r| r.get("capabilities"))
        .is_some()
}

/// Truncate a JSON value for logging (avoid huge log entries).
fn truncate_log(value: &Value) -> String {
    let s = value.to_string();
    if s.len() > 500 {
        format!("{}... ({} bytes total)", &s[..500], s.len())
    } else {
        s
    }
}
