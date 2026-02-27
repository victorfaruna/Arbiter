use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::exchange::ExchangeConnector;
use crate::types::*;

/// Executes arbitrage trades based on detected opportunities
pub struct OrderExecutor {
    connectors: Vec<Arc<dyn ExchangeConnector>>,
    config: Config,
    /// Trade history
    trades: Arc<Mutex<Vec<TradeResult>>>,
    /// Channel to broadcast executed trades
    trade_tx: mpsc::UnboundedSender<TradeResult>,
    /// Counters
    total_trades: Arc<AtomicU64>,
    total_profit: Arc<Mutex<Decimal>>,
    daily_loss: Arc<Mutex<Decimal>>,
    /// Last trade timestamp for cooldown
    last_trade_at: Arc<Mutex<Option<chrono::DateTime<Utc>>>>,
}

impl OrderExecutor {
    pub fn new(
        connectors: Vec<Arc<dyn ExchangeConnector>>,
        config: Config,
        trade_tx: mpsc::UnboundedSender<TradeResult>,
    ) -> Self {
        Self {
            connectors,
            config,
            trades: Arc::new(Mutex::new(Vec::new())),
            trade_tx,
            total_trades: Arc::new(AtomicU64::new(0)),
            total_profit: Arc::new(Mutex::new(Decimal::ZERO)),
            daily_loss: Arc::new(Mutex::new(Decimal::ZERO)),
            last_trade_at: Arc::new(Mutex::new(None)),
        }
    }

    /// Start listening for opportunities and execute trades
    pub async fn start(&self, mut opportunity_rx: mpsc::UnboundedReceiver<ArbitrageOpportunity>) {
        info!("Order executor started (simulation={})", self.config.engine.simulation_mode);

        while let Some(opp) = opportunity_rx.recv().await {
            if !opp.is_actionable {
                continue;
            }

            // Check risk limits
            if let Err(reason) = self.check_risk_limits(&opp).await {
                warn!("Skipping opportunity {}: {}", opp.id, reason);
                continue;
            }

            // Check cooldown
            if let Some(last) = *self.last_trade_at.lock().await {
                let elapsed = (Utc::now() - last).num_milliseconds() as u64;
                if elapsed < self.config.risk.trade_cooldown_ms {
                    continue;
                }
            }

            // Execute the trade
            let result = self.execute_trade(&opp).await;
            match &result {
                Ok(trade) => {
                    info!(
                        "Trade executed: {} | Buy {} @ {} on {} | Sell @ {} on {} | Profit: {}",
                        trade.id,
                        trade.pair,
                        trade.buy_price,
                        trade.buy_exchange,
                        trade.sell_price,
                        trade.sell_exchange,
                        trade.net_profit,
                    );

                    // Update counters
                    self.total_trades.fetch_add(1, Ordering::Relaxed);
                    *self.total_profit.lock().await += trade.net_profit;
                    if trade.net_profit < Decimal::ZERO {
                        *self.daily_loss.lock().await += trade.net_profit.abs();
                    }
                    *self.last_trade_at.lock().await = Some(Utc::now());

                    // Store and broadcast
                    self.trades.lock().await.push(trade.clone());
                    let _ = self.trade_tx.send(trade.clone());
                }
                Err(e) => {
                    error!("Trade execution failed for opportunity {}: {}", opp.id, e);
                }
            }
        }
    }

    /// Validate risk limits before executing
    async fn check_risk_limits(&self, opp: &ArbitrageOpportunity) -> Result<(), String> {
        let daily_loss = *self.daily_loss.lock().await;
        if daily_loss >= self.config.risk.max_daily_loss {
            return Err(format!(
                "Daily loss limit reached: {} >= {}",
                daily_loss, self.config.risk.max_daily_loss
            ));
        }

        if opp.quantity > self.config.risk.max_position {
            return Err(format!(
                "Position too large: {} > max {}",
                opp.quantity, self.config.risk.max_position
            ));
        }

        Ok(())
    }

    /// Execute a buy on one exchange and a sell on another
    async fn execute_trade(
        &self,
        opp: &ArbitrageOpportunity,
    ) -> Result<TradeResult, String> {
        let trade_id = Uuid::new_v4().to_string();

        if self.config.engine.simulation_mode {
            // Simulation mode — don't place real orders
            let buy_fee = self.get_fee(opp.buy_exchange);
            let sell_fee = self.get_fee(opp.sell_exchange);

            let gross_profit = opp.quantity * (opp.sell_price - opp.buy_price);
            let fees = opp.quantity * opp.buy_price * (buy_fee / dec!(100))
                + opp.quantity * opp.sell_price * (sell_fee / dec!(100));
            let net_profit = gross_profit - fees;

            return Ok(TradeResult {
                id: trade_id,
                opportunity_id: opp.id.clone(),
                pair: opp.pair.clone(),
                buy_exchange: opp.buy_exchange,
                sell_exchange: opp.sell_exchange,
                buy_price: opp.buy_price,
                sell_price: opp.sell_price,
                quantity: opp.quantity,
                gross_profit,
                fees,
                net_profit,
                status: TradeStatus::Filled,
                executed_at: Utc::now(),
            });
        }

        // Live mode — execute simultaneously on both exchanges
        let buy_connector = self
            .connectors
            .iter()
            .find(|c| c.exchange() == opp.buy_exchange)
            .ok_or("Buy exchange connector not found")?;

        let sell_connector = self
            .connectors
            .iter()
            .find(|c| c.exchange() == opp.sell_exchange)
            .ok_or("Sell exchange connector not found")?;

        let order_type = if self.config.trading.order_type == "limit" {
            OrderType::Limit
        } else {
            OrderType::Market
        };

        // Execute both orders concurrently
        let buy_future = buy_connector.place_order(
            &opp.pair,
            OrderSide::Buy,
            order_type,
            opp.quantity,
            Some(opp.buy_price),
        );

        let sell_future = sell_connector.place_order(
            &opp.pair,
            OrderSide::Sell,
            order_type,
            opp.quantity,
            Some(opp.sell_price),
        );

        let (buy_result, sell_result) = tokio::join!(buy_future, sell_future);

        let status = match (&buy_result, &sell_result) {
            (Ok(_), Ok(_)) => TradeStatus::Filled,
            (Ok(_), Err(_)) | (Err(_), Ok(_)) => TradeStatus::PartialFill,
            (Err(_), Err(_)) => TradeStatus::Failed,
        };

        if let (Err(ref e1), Err(ref e2)) = (&buy_result, &sell_result) {
            return Err(format!("Both orders failed: buy={}, sell={}", e1, e2));
        }

        let buy_fee = buy_connector.fee_pct();
        let sell_fee = sell_connector.fee_pct();
        let gross_profit = opp.quantity * (opp.sell_price - opp.buy_price);
        let fees = opp.quantity * opp.buy_price * (buy_fee / dec!(100))
            + opp.quantity * opp.sell_price * (sell_fee / dec!(100));

        Ok(TradeResult {
            id: trade_id,
            opportunity_id: opp.id.clone(),
            pair: opp.pair.clone(),
            buy_exchange: opp.buy_exchange,
            sell_exchange: opp.sell_exchange,
            buy_price: opp.buy_price,
            sell_price: opp.sell_price,
            quantity: opp.quantity,
            gross_profit,
            fees,
            net_profit: gross_profit - fees,
            status,
            executed_at: Utc::now(),
        })
    }

    fn get_fee(&self, exchange: Exchange) -> Decimal {
        self.connectors
            .iter()
            .find(|c| c.exchange() == exchange)
            .map(|c| c.fee_pct())
            .unwrap_or(dec!(0.1))
    }

    /// Get all executed trades
    pub async fn get_trades(&self) -> Vec<TradeResult> {
        self.trades.lock().await.clone()
    }

    /// Get total profit
    pub async fn get_total_profit(&self) -> Decimal {
        *self.total_profit.lock().await
    }

    /// Get total trade count
    pub fn get_trade_count(&self) -> u64 {
        self.total_trades.load(Ordering::Relaxed)
    }
}
