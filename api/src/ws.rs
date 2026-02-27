use actix_web::{web, HttpRequest, HttpResponse};
use actix_ws::Message;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::info;

use crate::state::AppState;

/// WebSocket handler — streams real-time data to UI clients
pub async fn ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, msg_stream) = actix_ws::handle(&req, stream)?;

    // Create a channel for this client
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Register the client
    state.ws_clients.lock().await.push(tx);
    info!("WebSocket client connected");

    // Send current status on connect
    let status = state.get_status().await;
    if let Ok(json) = serde_json::to_string(&arb_core::types::WsMessage::Status(status)) {
        let _ = session.text(json).await;
    }

    // Send current prices on connect
    for entry in state.prices.iter() {
        let ticker = entry.value().clone();
        if let Ok(json) = serde_json::to_string(&arb_core::types::WsMessage::Ticker(ticker)) {
            let _ = session.text(json).await;
        }
    }

    // Spawn a task to forward broadcast messages to this client
    let mut session_clone = session.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if session_clone.text(msg).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages from client using actix_web::rt (not tokio::spawn)
    // actix_ws MessageStream is not Send, so we use actix_web::rt::spawn instead
    actix_web::rt::spawn(async move {
        let mut msg_stream = msg_stream;
        while let Some(Ok(msg)) = msg_stream.next().await {
            match msg {
                Message::Ping(bytes) => {
                    if session.pong(&bytes).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => {
                    info!("WebSocket client disconnected");
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(response)
}
