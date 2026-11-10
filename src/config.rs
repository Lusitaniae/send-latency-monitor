//! YAML configuration. Credentials and the keypair are read from the environment,
//! never from YAML.

use std::{collections::HashSet, path::PathBuf, str::FromStr, time::Duration};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use solana_pubkey::Pubkey;

/// Ceilings that catch a typo (an extra zero) before it costs real money.
/// Raising one is a code change, so it gets a review.
pub const MAX_TIP_LAMPORTS: u64 = 10_000_000;
pub const MAX_PRIORITY_LAMPORTS: u64 = 1_000_000;
pub const MAX_PATHS: usize = 16;
/// Each round picks one of this many blocks of `MAX_PATHS` self-transfer amounts
/// at random, so copies from instances that share a fee payer and read the same
/// blockhash still have distinct signatures.
pub const SELF_TRANSFER_BLOCKS: usize = 1_000_000;
/// The largest self-transfer a copy makes. The fee payer must hold it.
pub const MAX_SELF_TRANSFER_LAMPORTS: u64 = (MAX_PATHS * SELF_TRANSFER_BLOCKS) as u64;
/// Base fee per signature. Every probe has exactly one signature.
pub const BASE_FEE_LAMPORTS: u64 = 5_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Holds the daily spend ledger; persist it across restarts.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    pub location: Location,
    pub wallet: Wallet,
    pub reference_rpc: ReferenceRpc,
    pub clickhouse: ClickHouse,
    pub rounds: Rounds,
    pub budget: Budget,
    pub paths: Vec<PathConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// Canonical colo label, e.g. `fra`. Matches the subscriber's region labels.
    pub region: String,
    /// Hosting label, e.g. `tsw`, `aws`, `gcp`.
    pub cloud_provider: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wallet {
    /// Environment variable holding the fee payer's secret key, as base58 or as the
    /// JSON byte array `solana-keygen` writes.
    pub keypair_env: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRpc {
    /// Environment variable holding the trusted RPC URL. It supplies blockhashes,
    /// slots, signature statuses and block order; it is never one of the paths.
    pub url_env: String,
    #[serde(default = "default_request_timeout", with = "humantime_serde")]
    pub request_timeout: Duration,
    #[serde(default = "default_status_poll", with = "humantime_serde")]
    pub status_poll: Duration,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickHouse {
    pub url: String,
    pub database: String,
    pub user_env: String,
    pub password_env: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rounds {
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// Each wait is `interval` plus a uniform draw in `[-jitter, +jitter]`, so rounds
    /// do not line up with the leader schedule.
    #[serde(with = "humantime_serde")]
    pub jitter: Duration,
    /// Requested compute units. The priority fee is charged on this limit.
    #[serde(default = "default_cu_limit")]
    pub cu_limit: u32,
    /// Total priority fee per copy in lamports, converted to a CU price. Every copy
    /// pays the same, so the fee does not decide their order.
    pub priority_lamports: u64,
    /// Optional account every copy write-locks without using it. Copies already
    /// share the fee payer's lock; set this when paths sign with different payers.
    #[serde(default)]
    pub lock_account: Option<String>,
    /// A copy with no status after this is dropped, even if its blockhash is still valid.
    #[serde(default = "default_settle_timeout", with = "humantime_serde")]
    pub settle_timeout: Duration,
    /// Per-request timeout for sends to the paths.
    #[serde(default = "default_send_timeout", with = "humantime_serde")]
    pub send_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Most lamports this sender may spend per UTC day: fees of included copies plus
    /// tips of successful ones. A round starts only if its worst case still fits.
    pub daily_lamports: u64,
    /// Rounds stop while the fee payer holds less than this. At least the largest
    /// self-transfer plus one round, so a copy never fails for lack of funds.
    pub min_balance_lamports: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum Method {
    #[serde(rename = "sendTransaction")]
    SendTransaction,
    #[serde(rename = "sendBundle")]
    SendBundle,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::SendTransaction => "sendTransaction",
            Method::SendBundle => "sendBundle",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathConfig {
    /// Shown as the provider on dashboards: lowercase letters, digits and dashes.
    pub name: String,
    /// Environment variable holding the endpoint URL (URLs often embed API keys).
    pub url_env: String,
    #[serde(default = "default_method")]
    pub method: Method,
    /// Options object sent as the second `sendTransaction` parameter. `encoding` is
    /// always base64. Defaults to `skipPreflight: true, maxRetries: 0`, so the path's
    /// own routing is measured instead of a preflight simulation or RPC rebroadcast.
    #[serde(default)]
    pub send_options: Option<serde_json::Map<String, serde_json::Value>>,
    /// Extra request headers: header name to the environment variable holding its value.
    #[serde(default)]
    pub headers_env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub tip: Option<Tip>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tip {
    pub lamports: u64,
    /// The path's published tip accounts; one is picked at random per copy.
    pub accounts: Vec<String>,
}

fn default_state_dir() -> PathBuf {
    ".send-benchmark-state".into()
}
fn default_request_timeout() -> Duration {
    Duration::from_secs(2)
}
fn default_status_poll() -> Duration {
    Duration::from_millis(400)
}
fn default_cu_limit() -> u32 {
    1_000
}
fn default_settle_timeout() -> Duration {
    Duration::from_secs(90)
}
fn default_send_timeout() -> Duration {
    Duration::from_secs(5)
}
fn default_method() -> Method {
    Method::SendTransaction
}

impl Config {
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config = yaml_serde::from_str(&text)
            .with_context(|| format!("parsing config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        label(&self.location.region, "location.region")?;
        label(&self.location.cloud_provider, "location.cloud_provider")?;
        let r = &self.rounds;
        if r.interval < Duration::from_secs(1) || r.jitter >= r.interval {
            bail!("rounds.interval must be at least 1s and larger than rounds.jitter");
        }
        if r.cu_limit < 450 || r.cu_limit > 1_400_000 {
            bail!("rounds.cu_limit must be between 450 (two compute budget instructions and two transfers) and 1,400,000");
        }
        if r.priority_lamports > MAX_PRIORITY_LAMPORTS {
            bail!("rounds.priority_lamports is above the {MAX_PRIORITY_LAMPORTS}-lamport ceiling");
        }
        if let Some(lock) = &r.lock_account {
            Pubkey::from_str(lock).context("rounds.lock_account must be a base58 pubkey")?;
        }
        if r.settle_timeout < Duration::from_secs(10) || r.send_timeout.is_zero() {
            bail!("rounds.settle_timeout must be at least 10s and rounds.send_timeout positive");
        }
        if self.reference_rpc.request_timeout.is_zero() || self.reference_rpc.status_poll.is_zero()
        {
            bail!("reference_rpc durations must be positive");
        }
        if self.paths.is_empty() || self.paths.len() > MAX_PATHS {
            bail!("configure between 1 and {MAX_PATHS} paths");
        }
        let mut names = HashSet::new();
        for path in &self.paths {
            label(&path.name, "paths[].name")?;
            if !names.insert(path.name.as_str()) {
                bail!("duplicate path name {}", path.name);
            }
            if let Some(tip) = &path.tip {
                if tip.lamports == 0 || tip.lamports > MAX_TIP_LAMPORTS {
                    bail!(
                        "path {}: tip.lamports must be between 1 and {MAX_TIP_LAMPORTS}",
                        path.name
                    );
                }
                if tip.accounts.is_empty() {
                    bail!("path {}: tip.accounts is empty", path.name);
                }
                for account in &tip.accounts {
                    Pubkey::from_str(account).with_context(|| {
                        format!("path {}: tip account {account} is not a pubkey", path.name)
                    })?;
                }
            }
            if path.method == Method::SendBundle && path.tip.is_none() {
                bail!(
                    "path {}: a bundle without a tip is never included; configure tip",
                    path.name
                );
            }
            if let Some(options) = &path.send_options {
                if options.contains_key("encoding") {
                    bail!(
                        "path {}: send_options must not set encoding (always base64)",
                        path.name
                    );
                }
            }
        }
        let worst = self.worst_round_lamports();
        if worst > self.budget.daily_lamports {
            bail!(
                "one round can cost up to {worst} lamports, above budget.daily_lamports ({})",
                self.budget.daily_lamports
            );
        }
        let floor = MAX_SELF_TRANSFER_LAMPORTS + worst;
        if self.budget.min_balance_lamports < floor {
            bail!(
                "budget.min_balance_lamports must be at least {floor}: the largest self-transfer ({MAX_SELF_TRANSFER_LAMPORTS}) plus one round ({worst})"
            );
        }
        Ok(())
    }

    /// Priority fee per copy in micro-lamports per CU, rounded down so the charged
    /// fee never exceeds `priority_lamports`.
    pub fn cu_price_micro_lamports(&self) -> u64 {
        self.rounds.priority_lamports * 1_000_000 / u64::from(self.rounds.cu_limit)
    }

    /// The fee the runtime charges for one included copy.
    pub fn fee_lamports(&self) -> u64 {
        let micro = u128::from(self.cu_price_micro_lamports()) * u128::from(self.rounds.cu_limit);
        BASE_FEE_LAMPORTS + u64::try_from(micro.div_ceil(1_000_000)).unwrap_or(u64::MAX)
    }

    /// Every copy included and every tip paid.
    pub fn worst_round_lamports(&self) -> u64 {
        self.paths
            .iter()
            .map(|p| self.fee_lamports() + p.tip.as_ref().map_or(0, |t| t.lamports))
            .sum()
    }
}

/// Labels end up in ClickHouse keys and dashboard URLs.
fn label(value: &str, field: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok {
        bail!("{field} must be 1-64 lowercase letters, digits, '-' or '_'");
    }
    Ok(())
}

/// Read a required environment variable without echoing its value in errors.
pub fn env(name: &str, what: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Ok(_) => bail!("{what}: environment variable {name} is empty"),
        Err(_) => bail!("{what}: environment variable {name} is not set or not Unicode"),
    }
}

/// An HTTP(S) URL from the environment. Errors never include the URL, since it can
/// embed credentials.
pub fn env_url(name: &str, what: &str) -> Result<String> {
    let value = env(name, what)?;
    let url = reqwest::Url::parse(&value)
        .map_err(|_| anyhow::anyhow!("{what} must be an HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("{what} must be an HTTP(S) URL");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../config.example.yaml");

    #[test]
    fn example_config_is_valid() {
        let config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn fee_never_exceeds_the_configured_priority() {
        let mut config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        for (limit, priority) in [(1_000, 2_000), (1_234, 2_001), (450, 7), (200_000, 999_999)] {
            config.rounds.cu_limit = limit;
            config.rounds.priority_lamports = priority;
            let charged = config.fee_lamports() - BASE_FEE_LAMPORTS;
            assert!(
                charged <= priority,
                "{limit} CU at {priority}: charged {charged}"
            );
            assert!(
                priority - charged <= 1,
                "{limit} CU at {priority}: charged {charged}"
            );
        }
    }

    #[test]
    fn rejects_a_minimum_balance_below_the_largest_self_transfer() {
        let mut config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        let floor = MAX_SELF_TRANSFER_LAMPORTS + config.worst_round_lamports();
        config.budget.min_balance_lamports = floor - 1;
        assert!(config.validate().is_err());
        config.budget.min_balance_lamports = floor;
        config.validate().unwrap();
    }

    #[test]
    fn rejects_a_round_above_the_daily_budget() {
        let mut config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        config.budget.daily_lamports = config.worst_round_lamports() - 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_tips_above_the_ceiling_and_untipped_bundles() {
        let mut config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        let bundle = config
            .paths
            .iter()
            .position(|p| p.method == Method::SendBundle)
            .unwrap();
        config.paths[bundle].tip.as_mut().unwrap().lamports = MAX_TIP_LAMPORTS + 1;
        assert!(config.validate().is_err());
        config.paths[bundle].tip = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_bad_labels_and_duplicates() {
        let mut config: Config = yaml_serde::from_str(EXAMPLE).unwrap();
        config.paths[1].name = config.paths[0].name.clone();
        assert!(config.validate().is_err());
        config.paths[1].name = "Has Space".into();
        assert!(config.validate().is_err());
    }
}
