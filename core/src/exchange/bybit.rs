use async_trait::async_trait;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::config::ExchangeConfig;
use crate::exchange::{ExchangeConnector, ExchangeError};
use crate::types::*;

const BYBIT_WS_URL: &str = "wss://stream.bybit.com/v5/public/spot";
const BYBIT_REST_URL: &str = "https://api.bybit.com";

pub struct BybitConnector {
    config: ExchangeConfig,
    client: reqwest::Client,
}

impl BybitConnector {
    pub fn new(config: ExchangeConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn sign_request(&self, timestamp: i64, query: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let payload = format!("{}{}{}", timestamp, &self.config.api_key, query);
        let mut mac =
            HmacSha256::new_from_slice(self.config.api_secret.as_bytes()).expect("HMAC init");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// Bybit V5 WebSocket response envelope
#[derive(Debug, Deserialize)]
struct BybitWsResponse {
    topic: Option<String>,
    #[serde(rename = "type")]
    msg_type: Option<String>,
    data: Option<BybitTickerData>,
}

/// Bybit V5 spot ticker fields — full names as documented
#[derive(Debug, Deserialize)]
struct BybitTickerData {
    symbol: Option<String>,
    #[serde(rename = "bid1Price")]
    bid1_price: Option<String>,
    #[serde(rename = "ask1Price")]
    ask1_price: Option<String>,
    #[serde(rename = "lastPrice")]
    last_price: Option<String>,
    #[serde(rename = "volume24h")]
    volume_24h: Option<String>,
}

#[async_trait]
impl ExchangeConnector for BybitConnector {
    fn exchange(&self) -> Exchange {
        Exchange::Bybit
    }

    async fn subscribe_ticker(
        &self,
        pair: &TradingPair,
    ) -> Result<mpsc::UnboundedReceiver<Ticker>, ExchangeError> {
        let symbol = pair.symbol_for(Exchange::Bybit);
        let url = BYBIT_WS_URL.to_string();
        let pair_clone = pair.clone();
        let subscribe_msg = serde_json::json!({
            "op": "subscribe",
            "args": [format!("tickers.{}", symbol)]
        });

        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                match connect_async(&url).await {
                    Ok((ws_stream, _)) => {
                        info!("Connected to Bybit WebSocket for {}", symbol);
                        let (mut write, mut read) = ws_stream.split();

                        // Send subscription message
                        let sub_text = serde_json::to_string(&subscribe_msg).unwrap();
                        if let Err(e) = write.send(Message::Text(sub_text.into())).await {
                            error!("Failed to subscribe on Bybit: {}", e);
                            continue; // retry connection instead of killing the loop
                        }

                        // Bybit sends ping frames automatically, we respond with pong.
                        // Also send a heartbeat ping every 20s to be safe.
                        let ping_write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
                        let ping_writer = ping_write.clone();
                        let ping_task = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(tokio::time::Duration::from_secs(20)).await;
                                let ping_msg = serde_json::json!({"op": "ping"});
                                let mut w = ping_writer.lock().await;
                                if w.send(Message::Text(
                                    serde_json::to_string(&ping_msg).unwrap().into(),
                                ))
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                        });

                        let mut msg_count: u64 = 0;
                        // Track last-known bid/ask across delta updates
                        // Bybit sends partial updates — not every message includes bid1Price/ask1Price
                        let mut last_bid = Decimal::ZERO;
                        let mut last_ask = Decimal::ZERO;

                        while let Some(msg) = read.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    // Skip pong/subscription confirmations
                                    if text.contains("\"op\"") || text.contains("\"pong\"") || text.contains("\"ret_msg\"") {
                                        continue;
                                    }

                                    msg_count += 1;

                                    // Parse as raw JSON Value — avoids all field naming issues
                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                        let data = &json["data"];

                                        // On first message, log ALL available field names
                                        if msg_count == 1 {
                                            if let Some(obj) = data.as_object() {
                                                let keys: Vec<&String> = obj.keys().collect();
                                                info!("[Bybit] Data field keys: {:?}", keys);
                                            }
                                        }

                                        let last_str = data["lastPrice"].as_str().unwrap_or("0");
                                        let vol_str = data["volume24h"].as_str().unwrap_or("0");
                                        let last: Decimal = last_str.parse().unwrap_or(Decimal::ZERO);
                                        let vol: Decimal = vol_str.parse().unwrap_or(Decimal::ZERO);

                                        // Try bid/ask fields — update tracked values if present
                                        if let Some(b) = data["bid1Price"].as_str()
                                            .or_else(|| data["bidPrice"].as_str())
                                            .or_else(|| data["bid1"].as_str())
                                        {
                                            if let Ok(v) = b.parse::<Decimal>() {
                                                if v > Decimal::ZERO { last_bid = v; }
                                            }
                                        }
                                        if let Some(a) = data["ask1Price"].as_str()
                                            .or_else(|| data["askPrice"].as_str())
                                            .or_else(|| data["ask1"].as_str())
                                        {
                                            if let Ok(v) = a.parse::<Decimal>() {
                                                if v > Decimal::ZERO { last_ask = v; }
                                            }
                                        }

                                        // Fallback: if we still have no bid/ask, use lastPrice
                                        let bid = if last_bid > Decimal::ZERO { last_bid } else { last };
                                        let ask = if last_ask > Decimal::ZERO { last_ask } else { last };

                                        if msg_count <= 3 {
                                            info!("[Bybit] Parsed: bid={} ask={} last={} vol={}", bid, ask, last, vol);
                                        }

                                        if bid > Decimal::ZERO && ask > Decimal::ZERO {
                                            let ticker = Ticker {
                                                exchange: Exchange::Bybit,
                                                pair: pair_clone.clone(),
                                                bid,
                                                ask,
                                                last,
                                                volume_24h: vol,
                                                timestamp: Utc::now(),
                                            };
                                            if msg_count <= 3 {
                                                info!("[Bybit] ✅ Emitting ticker: {} bid={} ask={}", ticker.pair, ticker.bid, ticker.ask);
                                            }
                                            if tx.send(ticker).is_err() {
                                                ping_task.abort();
                                                return;
                                            }
                                        } else if msg_count <= 5 {
                                            warn!("[Bybit] ⚠️ bid={} ask={} (zero) — waiting for data", bid, ask);
                                        }
                                    }
                                }
                                Ok(Message::Ping(data)) => {
                                    let mut w = ping_write.lock().await;
                                    let _ = w.send(Message::Pong(data)).await;
                                }
                                Err(e) => {
                                    error!("Bybit WS error: {}", e);
                                    break;
                                }
                                _ => {}
                            }
                        }

                        ping_task.abort();
                    }
                    Err(e) => {
                        error!("Failed to connect to Bybit WS: {}", e);
                    }
                }
                warn!("Bybit WS disconnected, reconnecting in 1s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });

        Ok(rx)
    }

    async fn get_ticker(&self, pair: &TradingPair) -> Result<Ticker, ExchangeError> {
        let symbol = pair.symbol_for(Exchange::Bybit);
        let url = format!(
            "{}/v5/market/tickers?category=spot&symbol={}",
            BYBIT_REST_URL, symbol
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ExchangeError::Connection(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))?;

        let item = &data["result"]["list"][0];
        Ok(Ticker {
            exchange: Exchange::Bybit,
            pair: pair.clone(),
            bid: item["bid1Price"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            ask: item["ask1Price"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            last: item["lastPrice"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            volume_24h: item["volume24h"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            timestamp: Utc::now(),
        })
    }

    async fn place_order(
        &self,
        pair: &TradingPair,
        side: OrderSide,
        order_type: OrderType,
        quantity: Decimal,
        price: Option<Decimal>,
    ) -> Result<String, ExchangeError> {
        let symbol = pair.symbol_for(Exchange::Bybit);
        let timestamp = Utc::now().timestamp_millis();

        let mut body = serde_json::json!({
            "category": "spot",
            "symbol": symbol,
            "side": match side { OrderSide::Buy => "Buy", OrderSide::Sell => "Sell" },
            "orderType": match order_type { OrderType::Market => "Market", OrderType::Limit => "Limit" },
            "qty": quantity.to_string(),
        });

        if let Some(p) = price {
            body["price"] = serde_json::Value::String(p.to_string());
            body["timeInForce"] = serde_json::Value::String("GTC".to_string());
        }

        let body_str = serde_json::to_string(&body).unwrap();
        let signature = self.sign_request(timestamp, &body_str);

        let url = format!("{}/v5/order/create", BYBIT_REST_URL);

        let resp = self
            .client
            .post(&url)
            .header("X-BAPI-API-KEY", &self.config.api_key)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-TIMESTAMP", timestamp.to_string())
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| ExchangeError::Connection(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))?;

        if data["retCode"].as_i64() == Some(0) {
            Ok(data["result"]["orderId"]
                .as_str()
                .unwrap_or("unknown")
                .to_string())
        } else {
            Err(ExchangeError::OrderFailed(
                data["retMsg"]
                    .as_str()
                    .unwrap_or("Unknown error")
                    .to_string(),
            ))
        }
    }

    async fn get_balances(&self) -> Result<Vec<ExchangeBalance>, ExchangeError> {
        let timestamp = Utc::now().timestamp_millis();
        let query = "accountType=UNIFIED";
        let signature = self.sign_request(timestamp, query);

        let url = format!("{}/v5/account/wallet-balance?{}", BYBIT_REST_URL, query);

        let resp = self
            .client
            .get(&url)
            .header("X-BAPI-API-KEY", &self.config.api_key)
            .header("X-BAPI-SIGN", &signature)
            .header("X-BAPI-TIMESTAMP", timestamp.to_string())
            .send()
            .await
            .map_err(|e| ExchangeError::Connection(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))?;

        let coins = data["result"]["list"][0]["coin"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| {
                let free: Decimal = c["availableToWithdraw"].as_str()?.parse().ok()?;
                let locked: Decimal = c["locked"].as_str()?.parse().ok()?;
                if free + locked > Decimal::ZERO {
                    Some(ExchangeBalance {
                        exchange: Exchange::Bybit,
                        asset: c["coin"].as_str()?.to_string(),
                        free,
                        locked,
                        total: free + locked,
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(coins)
    }

    fn fee_pct(&self) -> Decimal {
        self.config.fee_pct
    }
}
