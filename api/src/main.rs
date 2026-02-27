mod routes;
mod state;
mod ws;

use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use arb_core::exchange::bybit::BybitConnector;
use arb_core::exchange::bitget::BitgetConnector;
use arb_core::exchange::ExchangeConnector;
use arb_core::{ArbitrageDetector, Config, OrderExecutor};

use state::AppState;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("🚀 ArbitrageBot API Server starting...");

    // Load configuration
    let config = Config::load("config.toml");
    let api_port = config.engine.api_port;

    info!(
        "Configuration loaded: simulation={}, min_spread={}%, pairs={:?}",
        config.engine.simulation_mode, config.engine.min_spread_pct, config.trading.pairs
    );

    // Create shared state
    let app_state = Arc::new(AppState::new(config.clone()));

    // Create exchange connectors
    let mut connectors: Vec<Arc<dyn ExchangeConnector>> = Vec::new();

    if let Some(cfg) = config.exchanges.get("bybit") {
        if cfg.enabled {
            let key_mask = if cfg.api_key.len() > 4 {
                format!("{}...{}", &cfg.api_key[..4], &cfg.api_key[cfg.api_key.len()-4..])
            } else {
                "(empty)".to_string()
            };
            info!("Bybit connector enabled | api_key={} | has_secret={}", key_mask, !cfg.api_secret.is_empty());
            connectors.push(Arc::new(BybitConnector::new(cfg.clone())));
        }
    }

    if let Some(cfg) = config.exchanges.get("bitget") {
        if cfg.enabled {
            let key_mask = if cfg.api_key.len() > 4 {
                format!("{}...{}", &cfg.api_key[..4], &cfg.api_key[cfg.api_key.len()-4..])
            } else {
                "(empty)".to_string()
            };
            info!("Bitget connector enabled | api_key={} | has_secret={} | has_passphrase={}", 
                key_mask, !cfg.api_secret.is_empty(), cfg.passphrase.as_ref().map_or(false, |p| !p.is_empty()));
            connectors.push(Arc::new(BitgetConnector::new(cfg.clone())));
        }
    }

    // Create channels for inter-component communication
    let (opp_tx, opp_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ticker_tx, mut ticker_rx) = tokio::sync::mpsc::unbounded_channel();
    let (trade_tx, mut trade_rx) = tokio::sync::mpsc::unbounded_channel();

    // Create the core engine components
    let detector = Arc::new(ArbitrageDetector::new(
        connectors.clone(),
        config.clone(),
        opp_tx.clone(),
        ticker_tx,
    ));

    let executor = Arc::new(OrderExecutor::new(
        connectors.clone(),
        config.clone(),
        trade_tx,
    ));

    // Spawn ticker forwarding to app state
    let state_for_ticker = app_state.clone();
    tokio::spawn(async move {
        while let Some(ticker) = ticker_rx.recv().await {
            state_for_ticker.update_price(ticker).await;
        }
    });

    // Spawn opportunity forwarding to app state + executor
    let state_for_opp = app_state.clone();
    let (opp_to_exec_tx, opp_to_exec_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut opp_rx = opp_rx;
        while let Some(opp) = opp_rx.recv().await {
            state_for_opp.add_opportunity(opp.clone()).await;
            let _ = opp_to_exec_tx.send(opp);
        }
    });

    // Spawn trade forwarding to app state
    let state_for_trade = app_state.clone();
    tokio::spawn(async move {
        while let Some(trade) = trade_rx.recv().await {
            state_for_trade.add_trade(trade).await;
        }
    });

    // Start the arbitrage detector
    let detector_clone = detector.clone();
    tokio::spawn(async move {
        detector_clone.start().await;
    });

    // Start the order executor
    tokio::spawn(async move {
        executor.start(opp_to_exec_rx).await;
    });

    // Mark engine as running
    app_state.engine_running.store(true, Ordering::Relaxed);

    info!("🔥 Engine started — monitoring Bybit & Bitget for arbitrage opportunities");
    info!("📡 API server listening on http://0.0.0.0:{}", api_port);

    // Start HTTP server
    let state_data = app_state.clone();
    HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);

        App::new()
            .wrap(cors)
            .app_data(web::Data::new(state_data.clone()))
            .configure(routes::configure)
            .route("/ws", web::get().to(ws::ws_handler))
    })
    .bind(("0.0.0.0", api_port))?
    .run()
    .await
}
