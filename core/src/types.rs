use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Supported exchanges
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exchange {
    Bybit,
    Bitget,
}

impl fmt::Display for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exchange::Bybit => write!(f, "Bybit"),
            Exchange::Bitget => write!(f, "Bitget"),
        }
    }
}

/// Trading pair
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TradingPair {
    pub base: String,  // e.g. "BTC"
    pub quote: String, // e.g. "USDT"
}

impl TradingPair {
    pub fn new(base: &str, quote: &str) -> Self {
        Self {
            base: base.to_uppercase(),
            quote: quote.to_uppercase(),
        }
    }

    /// Returns the pair symbol for a specific exchange
    pub fn symbol_for(&self, exchange: Exchange) -> String {
        match exchange {
            Exchange::Bybit => format!("{}{}", self.base, self.quote),    // BTCUSDT
            Exchange::Bitget => format!("{}{}", self.base, self.quote),   // BTCUSDT
        }
    }
}

impl fmt::Display for TradingPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.base, self.quote)
    }
}

/// Real-time ticker data from an exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticker {
    pub exchange: Exchange,
    pub pair: TradingPair,
    pub bid: Decimal,       // Best bid price
    pub ask: Decimal,       // Best ask price
    pub last: Decimal,      // Last traded price
    pub volume_24h: Decimal,
    pub timestamp: DateTime<Utc>,
}

impl Ticker {
    pub fn spread(&self) -> Decimal {
        self.ask - self.bid
    }

    pub fn mid_price(&self) -> Decimal {
        (self.bid + self.ask) / Decimal::from(2)
    }
}

/// Order side
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Market,
    Limit,
}

/// An arbitrage opportunity detected by the engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    pub id: String,
    pub pair: TradingPair,
    pub buy_exchange: Exchange,
    pub sell_exchange: Exchange,
    pub buy_price: Decimal,       // Ask on buy exchange
    pub sell_price: Decimal,      // Bid on sell exchange
    pub spread_pct: Decimal,      // Spread as percentage
    pub net_spread_pct: Decimal,  // Spread after fees
    pub potential_profit: Decimal, // Estimated profit in quote currency
    pub quantity: Decimal,
    pub detected_at: DateTime<Utc>,
    pub is_actionable: bool,
}

/// Result of an executed trade
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    pub id: String,
    pub opportunity_id: String,
    pub pair: TradingPair,
    pub buy_exchange: Exchange,
    pub sell_exchange: Exchange,
    pub buy_price: Decimal,
    pub sell_price: Decimal,
    pub quantity: Decimal,
    pub gross_profit: Decimal,
    pub fees: Decimal,
    pub net_profit: Decimal,
    pub status: TradeStatus,
    pub executed_at: DateTime<Utc>,
}

/// Trade execution status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradeStatus {
    Pending,
    PartialFill,
    Filled,
    Failed,
    Cancelled,
}

/// Exchange balance info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeBalance {
    pub exchange: Exchange,
    pub asset: String,
    pub free: Decimal,
    pub locked: Decimal,
    pub total: Decimal,
}

/// Engine status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub running: bool,
    pub uptime_secs: u64,
    pub opportunities_found: u64,
    pub trades_executed: u64,
    pub total_profit: Decimal,
    pub active_exchanges: Vec<Exchange>,
    pub monitored_pairs: Vec<TradingPair>,
}

/// Messages sent over the WebSocket to the UI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsMessage {
    #[serde(rename = "ticker")]
    Ticker(Ticker),
    #[serde(rename = "opportunity")]
    Opportunity(ArbitrageOpportunity),
    #[serde(rename = "trade")]
    Trade(TradeResult),
    #[serde(rename = "status")]
    Status(EngineStatus),
}
