use async_graphql::http::GraphiQLSource;
use beam_stream::graphql::AppSchema;
use beam_stream::state::{AppContext, AppState, UserContext};
use salvo::prelude::*;

const GRAPHQL_PATH: &str = "graphql";
const GRAPHQL_WS_PATH: &str = "graphql/ws";

#[handler]
pub async fn graphiql(res: &mut Response) {
    res.render(Text::Html(
        GraphiQLSource::build()
            .endpoint(&format!("/v1/{}", GRAPHQL_PATH))
            .subscription_endpoint(&format!("/v1/{}", GRAPHQL_WS_PATH))
            .finish(),
    ));
}

#[handler]
pub async fn graphql_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let schema = depot.obtain::<AppSchema>().unwrap().clone();
    let state = depot.obtain::<AppState>().unwrap().clone();

    // Parse GraphQL request from body
    let body_bytes = match req.payload().await {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain("Failed to read request body"));
            return;
        }
    };

    let gql_request: async_graphql::Request = match serde_json::from_slice(&body_bytes) {
        Ok(r) => r,
        Err(_) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Text::Plain("Invalid GraphQL request"));
            return;
        }
    };

    let mut gql_request = gql_request;
    let mut user_context = None;

    if let Some(auth_header) = req.headers().get("Authorization")
        && let Ok(auth_str) = auth_header.to_str()
        && auth_str.starts_with("Bearer ")
    {
        let token = &auth_str[7..];

        // Get AuthService from state
        match state.services.auth.verify_token(token).await {
            Ok(user) => {
                user_context = Some(UserContext {
                    user_id: user.user_id,
                });
            }
            Err(e) => {
                tracing::warn!("Failed to verify token: {}", e);
            }
        }
    }

    let app_context = AppContext::new(user_context);
    gql_request = gql_request.data(app_context);

    let gql_response = schema.execute(gql_request).await;

    // Serialize response
    match serde_json::to_vec(&gql_response) {
        Ok(json_bytes) => {
            res.headers_mut()
                .insert("Content-Type", "application/json".parse().unwrap());
            res.body(salvo::http::body::ResBody::Once(bytes::Bytes::from(
                json_bytes,
            )));
        }
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            res.render(Text::Plain("Failed to serialize GraphQL response"));
        }
    }
}
