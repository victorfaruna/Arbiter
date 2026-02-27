use chrono::Utc;
use dashmap::DashMap;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use crate::config::Config;
use crate::exchange::ExchangeConnector;
use crate::types::*;

/// Maintains latest prices and detects cross-exchange arbitrage opportunities
pub struct ArbitrageDetector {
    /// Latest ticker for each (exchange, pair)
    prices: Arc<DashMap<(Exchange, String), Ticker>>,
    /// Connectors for each exchange
    connectors: Vec<Arc<dyn ExchangeConnector>>,
    /// Configuration
    config: Config,
    /// Channel to send detected opportunities
    opportunity_tx: mpsc::UnboundedSender<ArbitrageOpportunity>,
    /// Channel to broadcast tickers to the API layer
    ticker_tx: mpsc::UnboundedSender<Ticker>,
}

impl ArbitrageDetector {
    pub fn new(
        connectors: Vec<Arc<dyn ExchangeConnector>>,
        config: Config,
        opportunity_tx: mpsc::UnboundedSender<ArbitrageOpportunity>,
        ticker_tx: mpsc::UnboundedSender<Ticker>,
    ) -> Self {
        Self {
            prices: Arc::new(DashMap::new()),
            connectors,
            config,
            opportunity_tx,
            ticker_tx,
        }
    }

    /// Start monitoring all pairs across all exchanges
    pub async fn start(&self) {
        let pairs: Vec<TradingPair> = self
            .config
            .trading
            .pairs
            .iter()
            .map(|p| {
                let parts: Vec<&str> = p.split('/').collect();
                TradingPair::new(parts[0], parts[1])
            })
            .collect();

        for pair in &pairs {
            for connector in &self.connectors {
                let exchange = connector.exchange();
                info!("Subscribing to {} on {}", pair, exchange);

                match connector.subscribe_ticker(pair).await {
                    Ok(mut rx) => {
                        let prices = self.prices.clone();
                        let opp_tx = self.opportunity_tx.clone();
                        let tick_tx = self.ticker_tx.clone();
                        let config = self.config.clone();
                        let all_connectors = self.connectors.clone();
                        let pair_str = pair.to_string();

                        tokio::spawn(async move {
                            while let Some(ticker) = rx.recv().await {
                                // Update latest price
                                let key = (ticker.exchange, pair_str.clone());
                                prices.insert(key, ticker.clone());

                                // Broadcast ticker to API
                                let _ = tick_tx.send(ticker.clone());

                                // Check for arbitrage opportunities
                                Self::check_opportunities(
                                    &prices,
                                    &ticker,
                                    &all_connectors,
                                    &config,
                                    &opp_tx,
                                );
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to subscribe to {} on {}: {}",
                            pair,
                            exchange,
                            e
                        );
                    }
                }
            }
        }
    }

    /// Compare latest ticker against all other exchanges for arbitrage
    fn check_opportunities(
        prices: &DashMap<(Exchange, String), Ticker>,
        incoming: &Ticker,
        connectors: &[Arc<dyn ExchangeConnector>],
        config: &Config,
        opp_tx: &mpsc::UnboundedSender<ArbitrageOpportunity>,
    ) {
        let pair_str = incoming.pair.to_string();
        let exchanges = [Exchange::Bybit, Exchange::Bitget];

        for other_exchange in &exchanges {
            if *other_exchange == incoming.exchange {
                continue;
            }

            let key = (*other_exchange, pair_str.clone());
            if let Some(other_ticker) = prices.get(&key) {
                // Direction 1: Buy on incoming exchange, sell on other
                Self::evaluate_spread(
                    incoming,
                    &other_ticker,
                    connectors,
                    config,
                    opp_tx,
                );

                // Direction 2: Buy on other exchange, sell on incoming
                Self::evaluate_spread(
                    &other_ticker,
                    incoming,
                    connectors,
                    config,
                    opp_tx,
                );
            }
        }
    }

    /// Evaluate a specific buy/sell direction for profitability
    fn evaluate_spread(
        buy_ticker: &Ticker,   // We buy at the ask price here
        sell_ticker: &Ticker,  // We sell at the bid price here
        connectors: &[Arc<dyn ExchangeConnector>],
        config: &Config,
        opp_tx: &mpsc::UnboundedSender<ArbitrageOpportunity>,
    ) {
        let buy_price = buy_ticker.ask;
        let sell_price = sell_ticker.bid;

        if buy_price <= Decimal::ZERO || sell_price <= Decimal::ZERO {
            return;
        }

        // Gross spread percentage
        let spread_pct = ((sell_price - buy_price) / buy_price) * dec!(100);

        // Get fees for both exchanges
        let buy_fee = connectors
            .iter()
            .find(|c| c.exchange() == buy_ticker.exchange)
            .map(|c| c.fee_pct())
            .unwrap_or(dec!(0.1));

        let sell_fee = connectors
            .iter()
            .find(|c| c.exchange() == sell_ticker.exchange)
            .map(|c| c.fee_pct())
            .unwrap_or(dec!(0.1));

        // Net spread after fees on both sides
        let total_fees = buy_fee + sell_fee;
        let net_spread_pct = spread_pct - total_fees;

        // Only report if net spread exceeds minimum threshold
        if net_spread_pct > config.engine.min_spread_pct {
            let quantity = config.trading.max_trade_qty;
            let potential_profit = quantity * (sell_price - buy_price)
                - quantity * buy_price * (buy_fee / dec!(100))
                - quantity * sell_price * (sell_fee / dec!(100));

            let opportunity = ArbitrageOpportunity {
                id: Uuid::new_v4().to_string(),
                pair: buy_ticker.pair.clone(),
                buy_exchange: buy_ticker.exchange,
                sell_exchange: sell_ticker.exchange,
                buy_price,
                sell_price,
                spread_pct,
                net_spread_pct,
                potential_profit,
                quantity,
                detected_at: Utc::now(),
                is_actionable: net_spread_pct > dec!(0),
            };

            debug!(
                "Opportunity: Buy {} @ {} on {}, Sell @ {} on {} | Spread: {}% (net: {}%)",
                opportunity.pair,
                buy_price,
                buy_ticker.exchange,
                sell_price,
                sell_ticker.exchange,
                spread_pct.round_dp(4),
                net_spread_pct.round_dp(4),
            );

            let _ = opp_tx.send(opportunity);
        }
    }

    /// Get all current prices (for API)
    pub fn get_prices(&self) -> Vec<Ticker> {
        self.prices
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }
}
