use crate::config::Config;
use crate::store::Store;
use crate::tools::ToolInfo;
use serde_json::{json, Value};
use std::time::Duration;

// Teambition 业务 MCP 客户端。Streamable HTTP + JSON-RPC。
pub struct Tb {
    pub cfg: Config,
    pub store: Store,
    pub tools: Vec<ToolInfo>,
    http: reqwest::blocking::Client,
    session: Option<String>,
    next_id: u64,
}

// SSE 响应或纯 JSON 都能解析。
pub fn parse_sse_json(text: &str) -> Option<Value> {
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return Some(v);
        }
    }
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.trim();
            if rest.is_empty() || rest == "[DONE]" {
                continue;
            }
            data.push_str(rest);
            data.push('\n');
        }
    }
    if data.trim().is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(data.trim()).ok()
}

impl Tb {
    pub fn new(store: Store) -> Result<Tb, String> {
        let cfg = store.load_config();
        if cfg.token().is_none() {
            return Err(format!(
                "未登录。先运行 tb login 打开 Teambition 页面创建 User Token，或直接 tb login --token <令牌>。页面: {}",
                crate::oauth::TOKEN_PAGE
            ));
        }
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        Ok(Tb {
            cfg,
            store,
            tools: Vec::new(),
            http,
            session: None,
            next_id: 1,
        })
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let token = self.cfg.access_token.clone().unwrap_or_default();
        let mut req = self
            .http
            .post(&self.cfg.mcp_url)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&body);
        if let Some(s) = &self.session {
            req = req.header("mcp-session-id", s.clone());
        }
        let resp = req.send().map_err(|e| format!("请求失败: {e}"))?;
        let status = resp.status();
        if let Some(s) = resp.headers().get("mcp-session-id") {
            if let Ok(v) = s.to_str() {
                self.session = Some(v.to_string());
            }
        }
        let text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;
        if !status.is_success() {
            let cut = &text[..text.len().min(300)];
            return Err(format!("HTTP {}: {}", status.as_u16(), cut));
        }
        let v = parse_sse_json(&text)
            .ok_or_else(|| format!("无法解析响应: {}", &text[..text.len().min(300)]))?;
        if let Some(err) = v.get("error") {
            return Err(format!("MCP 错误: {}", err));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    // 通知。发了不等结果。失败也忽略。
    fn notify(&self, method: &str) {
        let body = json!({"jsonrpc":"2.0","method":method,"params":{}});
        let token = self.cfg.access_token.clone().unwrap_or_default();
        let _ = self
            .http
            .post(&self.cfg.mcp_url)
            .bearer_auth(token)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&body)
            .send();
    }

    pub fn connect(&mut self) -> Result<(), String> {
        let _ = self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tba", "version": "0.1.0"}
            }),
        )?;
        self.notify("notifications/initialized");
        Ok(())
    }

    // refresh=false 时优先读本地缓存。
    pub fn discover(&mut self, refresh: bool) -> Result<(), String> {
        self.connect()?;
        if !refresh {
            let cached = self.store.load_tools_cache();
            if !cached.is_empty() {
                self.tools = cached;
                return Ok(());
            }
        }
        let res = self.rpc("tools/list", json!({}))?;
        let list = res
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        self.tools = list
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(|v| v.as_str())?.to_string();
                if name.is_empty() {
                    return None;
                }
                Some(ToolInfo {
                    name,
                    description: t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or_else(|| {
                        json!({"type": "object", "properties": {}})
                    }),
                })
            })
            .collect();
        let _ = self.store.save_tools_cache(&self.tools);
        Ok(())
    }

    pub fn tool_schema(&self, name: &str) -> Value {
        self.tools
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.input_schema.clone())
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
    }

    pub fn call(&mut self, tool: &str, args: Value) -> Result<Value, String> {
        let res = self.rpc("tools/call", json!({"name": tool, "arguments": args}))?;
        if res.get("isError") == Some(&json!(true)) {
            return Err(format!("工具 {tool} 返回错误: {}", res));
        }
        if let Some(arr) = res.get("content").and_then(|c| c.as_array()) {
            for item in arr {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                        if let Ok(v) = serde_json::from_str::<Value>(t) {
                            return Ok(v);
                        }
                        return Ok(Value::String(t.to_string()));
                    }
                }
            }
        }
        Ok(res)
    }

    // 按 schema 自动挑参数名后调用。
    pub fn call_smart(
        &mut self,
        tool: &str,
        pairs: &[(&str, Value)],
    ) -> Result<Value, String> {
        let schema = self.tool_schema(tool);
        let args = crate::tools::build_args(&schema, pairs);
        self.call(tool, args)
    }
}
