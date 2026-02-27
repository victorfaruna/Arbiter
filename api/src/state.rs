use arb_core::types::*;
use arb_core::Config;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};

/// Shared application state accessible from all API handlers
pub struct AppState {
    pub config: RwLock<Config>,
    pub prices: DashMap<(Exchange, String), Ticker>,
    pub opportunities: Mutex<VecDeque<ArbitrageOpportunity>>,
    pub trades: Mutex<Vec<TradeResult>>,
    pub engine_running: AtomicBool,
    pub start_time: Instant,
    pub opportunities_count: AtomicU64,
    pub trades_count: AtomicU64,
    pub total_profit: Mutex<rust_decimal::Decimal>,
    /// WebSocket broadcast: list of senders for connected UI clients
    pub ws_clients: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<String>>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config: RwLock::new(config),
            prices: DashMap::new(),
            opportunities: Mutex::new(VecDeque::with_capacity(1000)),
            trades: Mutex::new(Vec::new()),
            engine_running: AtomicBool::new(false),
            start_time: Instant::now(),
            opportunities_count: AtomicU64::new(0),
            trades_count: AtomicU64::new(0),
            total_profit: Mutex::new(rust_decimal::Decimal::ZERO),
            ws_clients: Mutex::new(Vec::new()),
        }
    }

    /// Broadcast a message to all connected WebSocket clients
    pub async fn broadcast(&self, msg: &WsMessage) {
        let json = match serde_json::to_string(msg) {
            Ok(j) => j,
            Err(_) => return,
        };

        let mut clients = self.ws_clients.lock().await;
        clients.retain(|tx| tx.send(json.clone()).is_ok());
    }

    /// Add a price update
    pub async fn update_price(&self, ticker: Ticker) {
        let key = (ticker.exchange, ticker.pair.to_string());
        self.prices.insert(key, ticker.clone());
        self.broadcast(&WsMessage::Ticker(ticker)).await;
    }

    /// Add a new opportunity
    pub async fn add_opportunity(&self, opp: ArbitrageOpportunity) {
        self.opportunities_count.fetch_add(1, Ordering::Relaxed);
        self.broadcast(&WsMessage::Opportunity(opp.clone())).await;

        let mut opps = self.opportunities.lock().await;
        opps.push_back(opp);
        // Keep only last 1000 opportunities
        while opps.len() > 1000 {
            opps.pop_front();
        }
    }

    /// Add a trade result
    pub async fn add_trade(&self, trade: TradeResult) {
        self.trades_count.fetch_add(1, Ordering::Relaxed);
        *self.total_profit.lock().await += trade.net_profit;
        self.broadcast(&WsMessage::Trade(trade.clone())).await;
        self.trades.lock().await.push(trade);
    }

    /// Get engine status
    pub async fn get_status(&self) -> EngineStatus {
        let config = self.config.read().await;
        let active_exchanges: Vec<Exchange> = config
            .exchanges
            .iter()
            .filter(|(_, cfg)| cfg.enabled)
            .filter_map(|(name, _)| match name.as_str() {
                "bybit" => Some(Exchange::Bybit),
                "bitget" => Some(Exchange::Bitget),
                _ => None,
            })
            .collect();

        let monitored_pairs: Vec<TradingPair> = config
            .trading
            .pairs
            .iter()
            .map(|p| {
                let parts: Vec<&str> = p.split('/').collect();
                TradingPair::new(parts[0], parts[1])
            })
            .collect();

        EngineStatus {
            running: self.engine_running.load(Ordering::Relaxed),
            uptime_secs: self.start_time.elapsed().as_secs(),
            opportunities_found: self.opportunities_count.load(Ordering::Relaxed),
            trades_executed: self.trades_count.load(Ordering::Relaxed),
            total_profit: *self.total_profit.lock().await,
            active_exchanges,
            monitored_pairs,
        }
    }
}
