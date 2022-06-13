use anyhow::Result;
use axum::{
    extract::{
        self,
        ws::{Message, WebSocket},
        WebSocketUpgrade,
    },
    response::IntoResponse,
    routing::{any, get},
    Extension, Router,
};
use futures_util::{stream::StreamExt, SinkExt};
use parking_lot::Mutex;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};

type BroadcastChannel<T> = (broadcast::Sender<T>, broadcast::Receiver<T>);

#[derive(Default)]
pub struct State {
    pub lobbies: HashMap<String, (BroadcastChannel<(u128, String)>, u32, u128)>,
}

async fn root() -> &'static str {
    "Amethyst is up!"
}

async fn socket_handler(
    stream: WebSocket,
    state: Arc<Mutex<State>>,
    lobby_id: String,
) -> Option<String> {
    if lobby_id.len() > 100 {
        tracing::info!("lobby id too short \"{}\"", lobby_id);
        return None;
    }
    let (mut sender, mut receiver) = stream.split();
    let (lobby_sender, mut lobby_receiver, member_count, last_member_id) = {
        let mut state = state.lock();
        let ((sender, _), member_count, id) =
            state
                .lobbies
                .entry(lobby_id.clone())
                .or_insert((broadcast::channel(128), 0, 0));
        (sender.clone(), sender.subscribe(), *member_count, *id)
    };
    state
        .lock()
        .lobbies
        .entry(lobby_id.clone())
        .and_modify(|(_, member_count, last_member_id)| {
            *last_member_id += 1;
            *member_count += 1;
        });
    let member_id = last_member_id + 1;

    tracing::info!(
        "new peer connected to lobby \"{}\" with id {}",
        lobby_id,
        member_id
    );

    {
        let lobby_id = lobby_id.clone();
        let state = state.clone();
        tokio::spawn(async move {
            tracing::debug!("waiting for new broadcast msg");
            while let Ok((source_client_id, msg)) = lobby_receiver.recv().await {
                tracing::debug!("got new msg");
                if source_client_id == member_id {
                    tracing::debug!("not broadcasting to own self");
                    continue;
                }
                if let Err(e) = sender.send(Message::Text(msg)).await {
                    tracing::error!("Failed to send message: {}", e);
                    break;
                }
            }
        });
    }
    tracing::debug!("waiting for message");
    while let Some(Ok(msg)) = receiver.next().await {
        tracing::debug!("get message");
        match msg {
            Message::Text(msg) => {
                let _ = lobby_sender.send((member_id, msg));
            }
            Message::Close(..) => {
                break;
            }
            _ => {}
        }
    }

    Some(lobby_id)
}

async fn wrapped_socket_handler(ws: WebSocket, state: Arc<Mutex<State>>, lobby_id: String) {
    tracing::info!("peer connected");
    socket_handler(ws, Arc::clone(&state), lobby_id.clone()).await;
    tracing::info!("peer disconnected from \"{}\"", lobby_id);
    // clean up if lobby is empty
    if let Some(0) = state.lock().lobbies.get(&lobby_id).map(|lobby| lobby.1) {
        tracing::info!("removed empty lobby \"{}\"", lobby_id);
        state.lock().lobbies.remove(&lobby_id);
    }
}

async fn upgrade_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<Mutex<State>>>,
    extract::Path((lobby_id,)): extract::Path<(String,)>,
) -> impl IntoResponse {
    tracing::debug!("upgrading to websocket");
    ws.on_upgrade(|ws| wrapped_socket_handler(ws, state, lobby_id))
}

#[tokio::main]
async fn main() -> Result<()> {
    let startup_msg = include_bytes!("startup.txt");
    tracing_subscriber::fmt::init();
    let state = Arc::new(Mutex::new(State::default()));
    let server = Router::new()
        .route("/", any(root))
        .route("/socket/:id", get(upgrade_handler))
        .layer(Extension(state))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any));

    let addr = SocketAddr::from(([127, 0, 0, 1], 7270));
    println!(
        "\u{001b}[36m{}\u{001b}[0m",
        String::from_utf8_lossy(startup_msg).into_owned()
    );
    axum::Server::bind(&addr)
        .serve(server.into_make_service())
        .await?;
    Ok(())
}
