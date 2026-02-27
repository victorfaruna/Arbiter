use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::Exchange;

/// Top-level configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub engine: EngineConfig,
    pub exchanges: HashMap<String, ExchangeConfig>,
    pub trading: TradingConfig,
    pub risk: RiskConfig,
}

/// Engine settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub min_spread_pct: Decimal,
    pub scan_interval_ms: u64,
    pub simulation_mode: bool,
    pub api_port: u16,
}

/// Per-exchange configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    pub enabled: bool,
    pub api_key: String,
    pub api_secret: String,
    /// Optional passphrase (some exchanges require this)
    pub passphrase: Option<String>,
    pub fee_pct: Decimal,
}

/// Trading parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    pub pairs: Vec<String>,
    pub max_trade_qty: Decimal,
    pub min_trade_qty: Decimal,
    pub order_type: String,
}

/// Risk management parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_position: Decimal,
    pub max_daily_loss: Decimal,
    pub max_concurrent_trades: u32,
    pub trade_cooldown_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        let mut exchanges = HashMap::new();
        exchanges.insert(
            "bybit".to_string(),
            ExchangeConfig {
                enabled: true,
                api_key: String::new(),
                api_secret: String::new(),
                passphrase: None,
                fee_pct: Decimal::new(1, 3), // 0.1%
            },
        );
        exchanges.insert(
            "bitget".to_string(),
            ExchangeConfig {
                enabled: true,
                api_key: String::new(),
                api_secret: String::new(),
                passphrase: Some(String::new()),
                fee_pct: Decimal::new(1, 3), // 0.1%
            },
        );

        Config {
            engine: EngineConfig {
                min_spread_pct: Decimal::new(1, 3), // 0.1%
                scan_interval_ms: 100,
                simulation_mode: true,
                api_port: 8080,
            },
            exchanges,
            trading: TradingConfig {
                pairs: vec!["BTC/USDT".to_string()],
                max_trade_qty: Decimal::new(1, 2), // 0.01 BTC
                min_trade_qty: Decimal::new(1, 4), // 0.0001 BTC
                order_type: "market".to_string(),
            },
            risk: RiskConfig {
                max_position: Decimal::new(1, 1), // 0.1 BTC
                max_daily_loss: Decimal::new(100, 0), // $100
                max_concurrent_trades: 3,
                trade_cooldown_ms: 1000,
            },
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse config file: {}. Using defaults.", e);
                Self::default()
            }),
            Err(_) => {
                tracing::info!("No config file found at {}. Using defaults.", path);
                Self::default()
            }
        }
    }

    pub fn get_exchange(&self, exchange: &crate::types::Exchange) -> Option<&ExchangeConfig> {
        let key = match exchange {
            Exchange::Bybit => "bybit",
            Exchange::Bitget => "bitget",
        };
        self.exchanges.get(key)
    }
}
