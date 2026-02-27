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

const BITGET_WS_URL: &str = "wss://ws.bitget.com/v2/ws/public";
const BITGET_REST_URL: &str = "https://api.bitget.com";

pub struct BitgetConnector {
    config: ExchangeConfig,
    client: reqwest::Client,
}

impl BitgetConnector {
    pub fn new(config: ExchangeConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    fn sign_request(&self, timestamp: i64, method: &str, path: &str, body: &str) -> String {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let prehash = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);
        let mut mac =
            HmacSha256::new_from_slice(self.config.api_secret.as_bytes()).expect("HMAC init");
        mac.update(prehash.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }
}

/// Bitget V2 WebSocket ticker response
#[derive(Debug, Deserialize)]
struct BitgetWsResponse {
    data: Option<Vec<BitgetTickerData>>,
}

#[derive(Debug, Deserialize)]
struct BitgetTickerData {
    #[serde(rename = "instId")]
    inst_id: Option<String>,
    #[serde(rename = "bestBid")]
    best_bid: Option<String>,
    #[serde(rename = "bestAsk")]
    best_ask: Option<String>,
    #[serde(rename = "lastPr")]
    last_price: Option<String>,
    #[serde(rename = "baseVolume")]
    base_volume: Option<String>,
}

#[async_trait]
impl ExchangeConnector for BitgetConnector {
    fn exchange(&self) -> Exchange {
        Exchange::Bitget
    }

    async fn subscribe_ticker(
        &self,
        pair: &TradingPair,
    ) -> Result<mpsc::UnboundedReceiver<Ticker>, ExchangeError> {
        let symbol = pair.symbol_for(Exchange::Bitget);
        let url = BITGET_WS_URL.to_string();
        let pair_clone = pair.clone();
        let subscribe_msg = serde_json::json!({
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "ticker",
                "instId": symbol
            }]
        });

        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            loop {
                match connect_async(&url).await {
                    Ok((ws_stream, _)) => {
                        info!("Connected to Bitget WebSocket for {}", symbol);
                        let (mut write, mut read) = ws_stream.split();

                        // Send subscription
                        let sub_text = serde_json::to_string(&subscribe_msg).unwrap();
                        if let Err(e) = write.send(Message::Text(sub_text.into())).await {
                            error!("Failed to subscribe on Bitget: {}", e);
                            break;
                        }

                        // Bitget REQUIRES a "ping" text message every 30 seconds
                        // or it drops the connection after ~120s
                        let ping_write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
                        let ping_writer = ping_write.clone();
                        let ping_task = tokio::spawn(async move {
                            loop {
                                tokio::time::sleep(tokio::time::Duration::from_secs(25)).await;
                                let mut w = ping_writer.lock().await;
                                if w.send(Message::Text("ping".into())).await.is_err() {
                                    break;
                                }
                            }
                        });

                        let mut msg_count: u64 = 0;

                        while let Some(msg) = read.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    // Skip pong responses and subscription confirmations
                                    if text == "pong"
                                        || text.contains("\"event\"")
                                        || text.contains("\"op\"")
                                    {
                                        continue;
                                    }

                                    msg_count += 1;

                                    // Parse as raw JSON Value — avoids all field naming issues
                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                        let data_arr = &json["data"];

                                        // On first message, log field names from first data element
                                        if msg_count == 1 {
                                            if let Some(arr) = data_arr.as_array() {
                                                if let Some(first) = arr.first() {
                                                    if let Some(obj) = first.as_object() {
                                                        let keys: Vec<&String> = obj.keys().collect();
                                                        info!("[Bitget] Data[0] field keys: {:?}", keys);
                                                    }
                                                }
                                            }
                                        }

                                        // Bitget data is an array
                                        if let Some(arr) = data_arr.as_array() {
                                            for item in arr {
                                                // Use correct Bitget V2 field names: bidPr, askPr, lastPr, baseVolume
                                                let bid_str = item["bidPr"].as_str()
                                                    .or_else(|| item["bestBid"].as_str())
                                                    .unwrap_or("0");
                                                let ask_str = item["askPr"].as_str()
                                                    .or_else(|| item["bestAsk"].as_str())
                                                    .unwrap_or("0");
                                                let last_str = item["lastPr"].as_str()
                                                    .or_else(|| item["last"].as_str())
                                                    .unwrap_or("0");
                                                let vol_str = item["baseVolume"].as_str()
                                                    .or_else(|| item["baseVol"].as_str())
                                                    .unwrap_or("0");

                                                let bid: Decimal = bid_str.parse().unwrap_or(Decimal::ZERO);
                                                let ask: Decimal = ask_str.parse().unwrap_or(Decimal::ZERO);
                                                let last: Decimal = last_str.parse().unwrap_or(Decimal::ZERO);
                                                let vol: Decimal = vol_str.parse().unwrap_or(Decimal::ZERO);

                                                if msg_count <= 3 {
                                                    info!("[Bitget] Parsed: bid={} ask={} last={} vol={}", bid, ask, last, vol);
                                                }

                                                if bid > Decimal::ZERO && ask > Decimal::ZERO {
                                                    let ticker = Ticker {
                                                        exchange: Exchange::Bitget,
                                                        pair: pair_clone.clone(),
                                                        bid,
                                                        ask,
                                                        last,
                                                        volume_24h: vol,
                                                        timestamp: Utc::now(),
                                                    };
                                                    if msg_count <= 3 {
                                                        info!("[Bitget] ✅ Emitting ticker: {} bid={} ask={}", ticker.pair, ticker.bid, ticker.ask);
                                                    }
                                                    if tx.send(ticker).is_err() {
                                                        ping_task.abort();
                                                        return;
                                                    }
                                                } else if msg_count <= 3 {
                                                    warn!("[Bitget] ⚠️ bid={} ask={} (zero) | bid_raw='{}' ask_raw='{}'", bid, ask, bid_str, ask_str);
                                                }
                                            }
                                        }
                                    }
                                }
                                Ok(Message::Ping(data)) => {
                                    let mut w = ping_write.lock().await;
                                    let _ = w.send(Message::Pong(data)).await;
                                }
                                Err(e) => {
                                    error!("Bitget WS error: {}", e);
                                    break;
                                }
                                _ => {}
                            }
                        }

                        ping_task.abort();
                    }
                    Err(e) => {
                        error!("Failed to connect to Bitget WS: {}", e);
                    }
                }
                warn!("Bitget WS disconnected, reconnecting in 1s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });

        Ok(rx)
    }

    async fn get_ticker(&self, pair: &TradingPair) -> Result<Ticker, ExchangeError> {
        let symbol = pair.symbol_for(Exchange::Bitget);
        let url = format!(
            "{}/api/v2/spot/market/tickers?symbol={}",
            BITGET_REST_URL, symbol
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

        let item = &data["data"][0];
        Ok(Ticker {
            exchange: Exchange::Bitget,
            pair: pair.clone(),
            bid: item["bestBid"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            ask: item["bestAsk"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            last: item["lastPr"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .unwrap_or(Decimal::ZERO),
            volume_24h: item["baseVolume"]
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
        let symbol = pair.symbol_for(Exchange::Bitget);
        let timestamp = Utc::now().timestamp_millis();

        let mut body = serde_json::json!({
            "symbol": symbol,
            "side": match side { OrderSide::Buy => "buy", OrderSide::Sell => "sell" },
            "orderType": match order_type { OrderType::Market => "market", OrderType::Limit => "limit" },
            "size": quantity.to_string(),
            "force": "gtc",
        });

        if let Some(p) = price {
            body["price"] = serde_json::Value::String(p.to_string());
        }

        let body_str = serde_json::to_string(&body).unwrap();
        let path = "/api/v2/spot/trade/place-order";
        let signature = self.sign_request(timestamp, "POST", path, &body_str);

        let url = format!("{}{}", BITGET_REST_URL, path);

        let resp = self
            .client
            .post(&url)
            .header("ACCESS-KEY", &self.config.api_key)
            .header("ACCESS-SIGN", &signature)
            .header("ACCESS-TIMESTAMP", timestamp.to_string())
            .header(
                "ACCESS-PASSPHRASE",
                self.config.passphrase.as_deref().unwrap_or(""),
            )
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| ExchangeError::Connection(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))?;

        if data["code"].as_str() == Some("00000") {
            Ok(data["data"]["orderId"]
                .as_str()
                .unwrap_or("unknown")
                .to_string())
        } else {
            Err(ExchangeError::OrderFailed(
                data["msg"]
                    .as_str()
                    .unwrap_or("Unknown error")
                    .to_string(),
            ))
        }
    }

    async fn get_balances(&self) -> Result<Vec<ExchangeBalance>, ExchangeError> {
        let timestamp = Utc::now().timestamp_millis();
        let path = "/api/v2/spot/account/assets";
        let signature = self.sign_request(timestamp, "GET", path, "");

        let url = format!("{}{}", BITGET_REST_URL, path);

        let resp = self
            .client
            .get(&url)
            .header("ACCESS-KEY", &self.config.api_key)
            .header("ACCESS-SIGN", &signature)
            .header("ACCESS-TIMESTAMP", timestamp.to_string())
            .header(
                "ACCESS-PASSPHRASE",
                self.config.passphrase.as_deref().unwrap_or(""),
            )
            .send()
            .await
            .map_err(|e| ExchangeError::Connection(e.to_string()))?;

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))?;

        let balances = data["data"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|b| {
                let free: Decimal = b["available"].as_str()?.parse().ok()?;
                let locked: Decimal = b["frozen"].as_str()?.parse().ok()?;
                if free + locked > Decimal::ZERO {
                    Some(ExchangeBalance {
                        exchange: Exchange::Bitget,
                        asset: b["coin"].as_str()?.to_string(),
                        free,
                        locked,
                        total: free + locked,
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(balances)
    }

    fn fee_pct(&self) -> Decimal {
        self.config.fee_pct
    }
}
