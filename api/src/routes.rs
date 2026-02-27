use actix_web::{web, HttpResponse};
use serde::Deserialize;
use std::sync::Arc;

use crate::state::AppState;

/// GET /api/prices — current prices across all exchanges
pub async fn get_prices(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let prices: Vec<_> = state.prices.iter().map(|entry| entry.value().clone()).collect();
    HttpResponse::Ok().json(prices)
}

/// GET /api/opportunities — recent arbitrage opportunities
pub async fn get_opportunities(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let opps = state.opportunities.lock().await;
    let list: Vec<_> = opps.iter().cloned().collect();
    HttpResponse::Ok().json(list)
}

/// GET /api/trades — trade history
pub async fn get_trades(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let trades = state.trades.lock().await;
    HttpResponse::Ok().json(trades.clone())
}

/// GET /api/status — engine status
pub async fn get_status(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let status = state.get_status().await;
    HttpResponse::Ok().json(status)
}

/// GET /api/portfolio — balances across all exchanges
pub async fn get_portfolio(state: web::Data<Arc<AppState>>) -> HttpResponse {
    // In a real implementation, this would query each exchange connector
    // For now, return simulated balances
    let config = state.config.read().await;
    let mut balances = Vec::new();

    for (name, cfg) in &config.exchanges {
        if cfg.enabled {
            let exchange = match name.as_str() {
                "bybit" => arb_core::types::Exchange::Bybit,
                "bitget" => arb_core::types::Exchange::Bitget,
                _ => continue,
            };

            balances.push(arb_core::types::ExchangeBalance {
                exchange,
                asset: "BTC".to_string(),
                free: rust_decimal::Decimal::new(5, 2), // 0.05 BTC
                locked: rust_decimal::Decimal::ZERO,
                total: rust_decimal::Decimal::new(5, 2),
            });
            balances.push(arb_core::types::ExchangeBalance {
                exchange,
                asset: "USDT".to_string(),
                free: rust_decimal::Decimal::new(5000, 0),
                locked: rust_decimal::Decimal::ZERO,
                total: rust_decimal::Decimal::new(5000, 0),
            });
        }
    }

    HttpResponse::Ok().json(balances)
}

#[derive(Deserialize)]
pub struct ConfigUpdate {
    pub min_spread_pct: Option<f64>,
    pub max_trade_qty: Option<f64>,
    pub simulation_mode: Option<bool>,
    pub scan_interval_ms: Option<u64>,
}

/// POST /api/config — update engine configuration
pub async fn update_config(
    state: web::Data<Arc<AppState>>,
    body: web::Json<ConfigUpdate>,
) -> HttpResponse {
    let mut config = state.config.write().await;

    if let Some(spread) = body.min_spread_pct {
        config.engine.min_spread_pct = rust_decimal::Decimal::from_f64_retain(spread)
            .unwrap_or(config.engine.min_spread_pct);
    }
    if let Some(qty) = body.max_trade_qty {
        config.trading.max_trade_qty = rust_decimal::Decimal::from_f64_retain(qty)
            .unwrap_or(config.trading.max_trade_qty);
    }
    if let Some(sim) = body.simulation_mode {
        config.engine.simulation_mode = sim;
    }
    if let Some(interval) = body.scan_interval_ms {
        config.engine.scan_interval_ms = interval;
    }

    HttpResponse::Ok().json(serde_json::json!({
        "status": "updated",
        "config": {
            "min_spread_pct": config.engine.min_spread_pct.to_string(),
            "max_trade_qty": config.trading.max_trade_qty.to_string(),
            "simulation_mode": config.engine.simulation_mode,
            "scan_interval_ms": config.engine.scan_interval_ms,
        }
    }))
}

/// Configure all routes
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api")
            .route("/prices", web::get().to(get_prices))
            .route("/opportunities", web::get().to(get_opportunities))
            .route("/trades", web::get().to(get_trades))
            .route("/status", web::get().to(get_status))
            .route("/portfolio", web::get().to(get_portfolio))
            .route("/config", web::post().to(update_config)),
    );
}
