//! JSON-RPC: the trusted reference node, and the send paths under test.
//!
//! Errors never carry a URL: endpoint URLs often embed API keys.

use std::{str::FromStr, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use crate::config::{Method, PathConfig};

fn client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(300))
        .build()
        .context("building HTTP client")
}

/// Strip the URL from a reqwest error before it reaches logs or ClickHouse.
fn describe(error: reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_decode() {
        "decode"
    } else if error.is_body() {
        "body"
    } else {
        "request"
    };
    format!("{kind}: {}", error.without_url())
}

#[derive(Clone)]
pub struct Reference {
    http: reqwest::Client,
    url: String,
}

#[derive(Debug, Clone)]
pub struct Blockhash {
    pub hash: Hash,
    pub last_valid_block_height: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub slot: u64,
    /// `None` when the transaction succeeded.
    pub err: Option<String>,
    /// confirmed or finalized; processed statuses are not reported.
    pub confirmed: bool,
}

impl Reference {
    pub fn new(url: String, timeout: Duration) -> Result<Self> {
        Ok(Self {
            http: client(timeout)?,
            url,
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("reference {method}: {}", describe(e)))?;
        let status = response.status();
        if !status.is_success() {
            bail!("reference {method}: HTTP {}", status.as_u16());
        }
        let value: Value = response
            .json()
            .await
            .map_err(|e| anyhow!("reference {method}: {}", describe(e)))?;
        if let Some(error) = value.get("error") {
            bail!("reference {method}: {}", rpc_error(error));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("reference {method}: response has no result"))
    }

    pub async fn latest_blockhash(&self) -> Result<Blockhash> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Inner {
            blockhash: String,
            last_valid_block_height: u64,
        }
        #[derive(Deserialize)]
        struct Outer {
            value: Inner,
        }
        let result = self
            .call("getLatestBlockhash", json!([{"commitment": "confirmed"}]))
            .await?;
        let outer: Outer = serde_json::from_value(result).context("getLatestBlockhash shape")?;
        Ok(Blockhash {
            hash: Hash::from_str(&outer.value.blockhash).context("blockhash")?,
            last_valid_block_height: outer.value.last_valid_block_height,
        })
    }

    pub async fn processed_slot(&self) -> Result<u64> {
        let result = self
            .call("getSlot", json!([{"commitment": "processed"}]))
            .await?;
        result
            .as_u64()
            .ok_or_else(|| anyhow!("getSlot: not a number"))
    }

    pub async fn block_height(&self) -> Result<u64> {
        let result = self
            .call("getBlockHeight", json!([{"commitment": "confirmed"}]))
            .await?;
        result
            .as_u64()
            .ok_or_else(|| anyhow!("getBlockHeight: not a number"))
    }

    pub async fn balance(&self, account: &Pubkey) -> Result<u64> {
        let result = self
            .call(
                "getBalance",
                json!([account.to_string(), {"commitment": "confirmed"}]),
            )
            .await?;
        result
            .get("value")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("getBalance: no value"))
    }

    /// Statuses in the order of `signatures`; `None` while a signature is unknown.
    pub async fn statuses(&self, signatures: &[Signature]) -> Result<Vec<Option<Status>>> {
        let sigs: Vec<String> = signatures.iter().map(ToString::to_string).collect();
        let result = self
            .call(
                "getSignatureStatuses",
                json!([sigs, {"searchTransactionHistory": false}]),
            )
            .await?;
        parse_statuses(&result, signatures.len())
    }

    /// Signatures of a confirmed block in block order: a transaction's position in
    /// this list is its index in the block. `None` while the block is not available.
    pub async fn block_signatures(&self, slot: u64) -> Result<Option<Vec<String>>> {
        let params = json!([slot, {
            "commitment": "confirmed",
            "transactionDetails": "signatures",
            "rewards": false,
            "maxSupportedTransactionVersion": 0,
        }]);
        match self.call("getBlock", params).await {
            Ok(result) => {
                let sigs = result
                    .get("signatures")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow!("getBlock: no signatures"))?
                    .iter()
                    .map(|s| s.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| anyhow!("getBlock: signature is not a string"))?;
                Ok(Some(sigs))
            }
            // -32004 block not available yet, -32007 skipped, -32009 missing in long-term storage.
            Err(error) if is_block_unavailable(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn is_block_unavailable(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    ["-32004", "-32007", "-32009"]
        .iter()
        .any(|c| text.contains(c))
}

fn rpc_error(error: &Value) -> String {
    let code = error.get("code").and_then(Value::as_i64);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message");
    let message: String = message.chars().take(200).collect();
    match code {
        Some(code) => format!("{code} {message}"),
        None => message,
    }
}

pub fn parse_statuses(result: &Value, expected: usize) -> Result<Vec<Option<Status>>> {
    let values = result
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("getSignatureStatuses: no value array"))?;
    if values.len() != expected {
        bail!(
            "getSignatureStatuses: {} statuses for {expected} signatures",
            values.len()
        );
    }
    values
        .iter()
        .map(|v| {
            if v.is_null() {
                return Ok(None);
            }
            let slot = v
                .get("slot")
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("status without slot"))?;
            let err = match v.get("err") {
                None | Some(Value::Null) => None,
                Some(e) => Some(e.to_string().chars().take(200).collect()),
            };
            let confirmed = matches!(
                v.get("confirmationStatus").and_then(Value::as_str),
                Some("confirmed" | "finalized")
            );
            Ok(Some(Status {
                slot,
                err,
                confirmed,
            }))
        })
        .collect()
}

/// How a path answered a send. The copy can still land after any of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendStatus {
    Accepted,
    RpcError(String),
    HttpError(u16),
    Timeout,
    Transport(String),
}

impl SendStatus {
    pub fn label(&self) -> &'static str {
        match self {
            SendStatus::Accepted => "accepted",
            SendStatus::RpcError(_) => "rpc_error",
            SendStatus::HttpError(_) => "http_error",
            SendStatus::Timeout => "timeout",
            SendStatus::Transport(_) => "transport",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            SendStatus::Accepted | SendStatus::Timeout => String::new(),
            SendStatus::RpcError(e) | SendStatus::Transport(e) => e.clone(),
            SendStatus::HttpError(code) => format!("HTTP {code}"),
        }
    }
}

pub struct Path {
    pub name: String,
    pub method: Method,
    http: reqwest::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    options: serde_json::Map<String, Value>,
}

impl Path {
    pub fn new(config: &PathConfig, timeout: Duration) -> Result<Self> {
        let what = format!("path {} URL", config.name);
        let url = crate::config::env_url(&config.url_env, &what)?;
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, env) in &config.headers_env {
            let value = crate::config::env(env, &format!("path {} header {name}", config.name))?;
            headers.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .with_context(|| format!("path {}: bad header name {name}", config.name))?,
                reqwest::header::HeaderValue::from_str(&value).map_err(|_| {
                    anyhow!("path {}: header {name} value is not valid", config.name)
                })?,
            );
        }
        let mut options = config.send_options.clone().unwrap_or_else(|| {
            let mut m = serde_json::Map::new();
            m.insert("skipPreflight".into(), Value::Bool(true));
            m.insert("maxRetries".into(), json!(0));
            m
        });
        options.insert("encoding".into(), Value::String("base64".into()));
        Ok(Self {
            name: config.name.clone(),
            method: config.method,
            http: client(timeout)?,
            url,
            headers,
            options,
        })
    }

    pub fn body(&self, wire: &[u8]) -> Value {
        let tx = base64::engine::general_purpose::STANDARD.encode(wire);
        let params = match self.method {
            Method::SendTransaction => json!([tx, self.options]),
            Method::SendBundle => json!([[tx], {"encoding": "base64"}]),
        };
        json!({"jsonrpc": "2.0", "id": 1, "method": self.method.as_str(), "params": params})
    }

    /// Open (or keep open) the connection before a round, so no copy pays for a TLS
    /// handshake the others skipped. Any HTTP answer will do; errors are ignored.
    pub async fn warm(&self) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "getHealth"});
        let _ = self
            .http
            .post(&self.url)
            .headers(self.headers.clone())
            .json(&body)
            .send()
            .await
            .map(|r| r.bytes());
    }

    /// Send a prepared body. Returns the outcome and, if any response arrived, how
    /// long it took.
    pub async fn send(&self, body: &Value) -> (SendStatus, Option<Duration>) {
        let start = std::time::Instant::now();
        let response = self
            .http
            .post(&self.url)
            .headers(self.headers.clone())
            .json(body)
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) if e.is_timeout() => return (SendStatus::Timeout, None),
            Err(e) => return (SendStatus::Transport(describe(e)), None),
        };
        let elapsed = start.elapsed();
        let status = response.status();
        if !status.is_success() {
            return (SendStatus::HttpError(status.as_u16()), Some(elapsed));
        }
        match response.json::<Value>().await {
            Ok(value) => (classify(&value), Some(elapsed)),
            Err(e) if e.is_timeout() => (SendStatus::Timeout, Some(elapsed)),
            Err(_) => (
                SendStatus::RpcError("unreadable response".into()),
                Some(elapsed),
            ),
        }
    }
}

pub fn classify(value: &Value) -> SendStatus {
    if let Some(error) = value.get("error") {
        return SendStatus::RpcError(rpc_error(error));
    }
    match value.get("result") {
        Some(Value::Null) | None => SendStatus::RpcError("response has no result".into()),
        Some(_) => SendStatus::Accepted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_keep_order_and_read_errors() {
        let result = json!({"context": {"slot": 10}, "value": [
            null,
            {"slot": 9, "err": null, "confirmationStatus": "confirmed", "confirmations": 0},
            {"slot": 9, "err": {"InstructionError": [3, {"Custom": 1}]}, "confirmationStatus": "processed"},
        ]});
        let s = parse_statuses(&result, 3).unwrap();
        assert_eq!(s[0], None);
        assert_eq!(
            s[1],
            Some(Status {
                slot: 9,
                err: None,
                confirmed: true
            })
        );
        let third = s[2].clone().unwrap();
        assert!(!third.confirmed);
        assert!(third.err.unwrap().contains("InstructionError"));
        assert!(parse_statuses(&result, 2).is_err());
    }

    #[test]
    fn classifies_send_answers() {
        assert_eq!(classify(&json!({"result": "5sig"})), SendStatus::Accepted);
        assert_eq!(
            classify(&json!({"result": "bundle-id-uuid"})),
            SendStatus::Accepted
        );
        assert_eq!(
            classify(&json!({"error": {"code": -32002, "message": "tip too low"}})),
            SendStatus::RpcError("-32002 tip too low".into())
        );
        assert!(matches!(
            classify(&json!({"result": null})),
            SendStatus::RpcError(_)
        ));
    }

    #[test]
    fn bodies_match_each_method() {
        let mut config: PathConfig = yaml_serde::from_str(
            "name: p\nurl_env: SEND_BENCH_TEST_URL_BODIES\nmethod: sendBundle\ntip: {lamports: 1, accounts: [96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5]}\n",
        )
        .unwrap();
        // No other test reads this variable.
        std::env::set_var("SEND_BENCH_TEST_URL_BODIES", "https://example.com/x");
        let bundle = Path::new(&config, Duration::from_secs(1)).unwrap();
        let body = bundle.body(&[1, 2, 3]);
        assert_eq!(body["method"], "sendBundle");
        assert_eq!(body["params"][0][0], "AQID");
        assert_eq!(body["params"][1]["encoding"], "base64");

        config.method = Method::SendTransaction;
        let plain = Path::new(&config, Duration::from_secs(1)).unwrap();
        let body = plain.body(&[1, 2, 3]);
        assert_eq!(body["params"][0], "AQID");
        assert_eq!(body["params"][1]["skipPreflight"], true);
        assert_eq!(body["params"][1]["maxRetries"], 0);
        assert_eq!(body["params"][1]["encoding"], "base64");
    }
}
