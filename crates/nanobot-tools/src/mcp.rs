//! A minimal MCP client over stdio.
//!
//! A harness that wants third-party tools should not have to grow its own
//! transport: MCP is newline-delimited JSON-RPC 2.0 over a child process's
//! stdio, and that is the whole protocol. This client speaks it — `initialize`,
//! `tools/list`, `tools/call` — and nothing more, so a product can consume any
//! MCP server without depending on a vendor SDK.
//!
//! It is deliberately synchronous and blocking: a tool call is a round trip,
//! and the caller decides whether to run it on a job (see [`crate::job`]).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

/// One tool an MCP server advertises.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    /// Tool name.
    pub name: String,
    /// Human/model-readable description.
    pub description: Option<String>,
    /// JSON Schema for the arguments.
    pub input_schema: Value,
}

/// An error from the MCP transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpError {
    /// The child could not be spawned.
    Spawn(String),
    /// I/O failed.
    Io(String),
    /// The server returned a JSON-RPC error.
    Rpc { code: i64, message: String },
    /// The response was not the expected shape.
    Protocol(String),
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(message) | Self::Io(message) | Self::Protocol(message) => {
                write!(f, "{message}")
            }
            Self::Rpc { code, message } => write!(f, "MCP error {code}: {message}"),
        }
    }
}

impl std::error::Error for McpError {}

/// A connected MCP server over stdio.
pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `command` (with `args` and extra `env`) and connect to its stdio.
    ///
    /// # Errors
    ///
    /// [`McpError::Spawn`] when the process cannot start.
    pub fn spawn(command: &str, args: &[&str], env: &[(String, String)]) -> Result<Self, McpError> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().map_err(|e| McpError::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Spawn("child stdin was not piped".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Spawn("child stdout was not piped".to_string()))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// Perform the MCP handshake.
    ///
    /// # Errors
    ///
    /// Propagates transport and protocol errors.
    pub fn initialize(&mut self, client_name: &str, version: &str) -> Result<Value, McpError> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": client_name, "version": version },
        });
        let result = self.request("initialize", params)?;
        // The spec requires the client to acknowledge before using the server.
        self.notify("notifications/initialized", json!({}))?;
        Ok(result)
    }

    /// List the server's tools.
    ///
    /// # Errors
    ///
    /// Propagates transport and protocol errors.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", json!({}))?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Protocol("tools/list returned no tools array".to_string()))?;
        Ok(tools
            .iter()
            .map(|tool| McpTool {
                name: tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            })
            .collect())
    }

    /// Call a tool by name with `arguments`.
    ///
    /// # Errors
    ///
    /// Propagates transport and protocol errors.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        // Responses arrive in request order on stdio; skip notifications.
        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| McpError::Io(e.to_string()))?;
            if read == 0 {
                return Err(McpError::Io("server closed the stream".to_string()));
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: Value =
                serde_json::from_str(line).map_err(|e| McpError::Protocol(e.to_string()))?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                // A notification or another request's response; keep reading.
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(McpError::Rpc {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                });
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::Protocol("response carried no result".to_string()));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(message).map_err(|e| McpError::Io(e.to_string()))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .map_err(|e| McpError::Io(e.to_string()))?;
        self.stdin.flush().map_err(|e| McpError::Io(e.to_string()))
    }
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-shot MCP server: reads one JSON-RPC request per line and answers
    /// it, switching on the method and echoing the request's id back. Enough to
    /// exercise the client without a real MCP implementation.
    fn server_script(error_for_tools_call: bool) -> String {
        let tools_call = if error_for_tools_call {
            r#"printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"no such tool"}}\n' "$id" ;;"#
        } else {
            r#"printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"hi"}],"isError":false}}\n' "$id" ;;"#
        };
        r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}}\n' "$id" ;;
    *'"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"say it","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"tools/call"'*)
      __TOOLS_CALL__
  esac
done
"#
        .replace("__TOOLS_CALL__", tools_call)
    }

    #[test]
    fn initialize_and_list_tools_round_trip() {
        let script = server_script(false);
        let mut client = McpClient::spawn("sh", &["-c", &script], &[]).expect("spawns");
        let init = client
            .initialize("fixture-client", "0.1.0")
            .expect("initialize");
        assert_eq!(init["serverInfo"]["name"], "fixture");
        let tools = client.list_tools().expect("lists tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description.as_deref(), Some("say it"));
    }

    #[test]
    fn a_tool_call_returns_the_result() {
        let script = server_script(false);
        let mut client = McpClient::spawn("sh", &["-c", &script], &[]).expect("spawns");
        client.initialize("c", "0").expect("initialize");
        let result = client
            .call_tool("echo", json!({"text": "hi"}))
            .expect("call");
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], "hi");
    }

    #[test]
    fn an_rpc_error_is_surfaced() {
        let script = server_script(true);
        let mut client = McpClient::spawn("sh", &["-c", &script], &[]).expect("spawns");
        client.initialize("c", "0").expect("initialize");
        let err = client.call_tool("nope", json!({})).unwrap_err();
        assert!(matches!(err, McpError::Rpc { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_binary_is_a_spawn_error() {
        let err = McpClient::spawn("definitely-not-a-real-binary-xyz", &[], &[]).unwrap_err();
        assert!(matches!(err, McpError::Spawn(_)), "{err:?}");
    }
}
