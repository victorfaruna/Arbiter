use async_trait::async_trait;
use rust_decimal::Decimal;

use crate::types::{Exchange, ExchangeBalance, OrderSide, OrderType, Ticker, TradingPair};

pub mod bybit;
pub mod bitget;

/// Core trait that all exchange connectors must implement
#[async_trait]
pub trait ExchangeConnector: Send + Sync {
    /// Which exchange this connector represents
    fn exchange(&self) -> Exchange;

    /// Subscribe to real-time ticker updates for a pair.
    async fn subscribe_ticker(
        &self,
        pair: &TradingPair,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Ticker>, ExchangeError>;

    /// Get the current best bid/ask for a pair (REST fallback)
    async fn get_ticker(&self, pair: &TradingPair) -> Result<Ticker, ExchangeError>;

    /// Place an order on this exchange
    async fn place_order(
        &self,
        pair: &TradingPair,
        side: OrderSide,
        order_type: OrderType,
        quantity: Decimal,
        price: Option<Decimal>,
    ) -> Result<String, ExchangeError>; // Returns order ID

    /// Get balances for all assets on this exchange
    async fn get_balances(&self) -> Result<Vec<ExchangeBalance>, ExchangeError>;

    /// Get the trading fee for a pair on this exchange (as a percentage)
    fn fee_pct(&self) -> Decimal;
}

/// Exchange-related errors
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("API error: {0}")]
    Api(String),

    #[error("Rate limit exceeded")]
    RateLimit,

    #[error("Invalid pair: {0}")]
    InvalidPair(String),

    #[error("Insufficient balance: need {needed}, have {available}")]
    InsufficientBalance {
        needed: Decimal,
        available: Decimal,
    },

    #[error("Order failed: {0}")]
    OrderFailed(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("WebSocket error: {0}")]
    WebSocket(String),
}
