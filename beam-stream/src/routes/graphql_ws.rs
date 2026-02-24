use async_graphql::http::{WebSocket as GqlWebSocket, WebSocketProtocols, WsMessage};
use beam_stream::graphql::AppSchema;
use beam_stream::state::{AppContext, AppState, UserContext};
use futures_util::{SinkExt, StreamExt};
use salvo::prelude::*;
use salvo::websocket::WebSocketUpgrade;

/// WebSocket handler for GraphQL subscriptions.
/// Supports both `graphql-transport-ws` (graphql-ws v5+) and `graphql-ws` protocols.
#[handler]
pub async fn graphql_ws_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let schema = depot.obtain::<AppSchema>().unwrap().clone();
    let state = depot.obtain::<AppState>().unwrap().clone();

    // Determine which sub-protocol the client is requesting
    let protocol = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|protocols| {
            protocols
                .split(',')
                .map(str::trim)
                .find_map(|p| p.parse::<WebSocketProtocols>().ok())
        })
        .unwrap_or(WebSocketProtocols::SubscriptionsTransportWS);

    // Extract auth token from query string or Authorization header
    let token = req.query::<String>("token").or_else(|| {
        req.headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
    });

    if let Err(e) = WebSocketUpgrade::new()
        .upgrade(req, res, move |ws| {
            let schema = schema.clone();
            let state = state.clone();
            let token = token.clone();

            async move {
                // Resolve user context from token if present
                let user_context = if let Some(token) = token {
                    match state.services.auth.verify_token(&token).await {
                        Ok(user) => Some(UserContext {
                            user_id: user.user_id,
                        }),
                        Err(e) => {
                            tracing::warn!("WS auth token invalid: {}", e);
                            None
                        }
                    }
                } else {
                    None
                };

                let app_context = AppContext::new(user_context);

                let (mut salvo_sink, salvo_stream) = ws.split();

                // Map Salvo WS messages to Vec<u8> (implements AsRef<[u8]>)
                let input_stream = Box::pin(salvo_stream.filter_map(|msg| async move {
                    match msg {
                        Ok(msg) if msg.is_text() || msg.is_binary() => {
                            Some(msg.as_bytes().to_vec())
                        }
                        _ => None,
                    }
                }));

                // Build the per-connection data (AppContext for auth guard)
                let mut conn_data = async_graphql::Data::default();
                conn_data.insert(app_context);

                // Drive the GraphQL WebSocket protocol as a Stream<Item = WsMessage>
                let mut gql_ws = Box::pin(
                    GqlWebSocket::new(schema, input_stream, protocol).connection_data(conn_data),
                );

                while let Some(ws_msg) = gql_ws.next().await {
                    let salvo_msg = match ws_msg {
                        WsMessage::Text(text) => salvo::websocket::Message::text(text),
                        WsMessage::Close(code, reason) => {
                            salvo::websocket::Message::close_with(code, reason)
                        }
                    };
                    if salvo_sink.send(salvo_msg).await.is_err() {
                        break;
                    }
                }
            }
        })
        .await
    {
        tracing::warn!("WebSocket upgrade failed: {:?}", e);
    }
}
