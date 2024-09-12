mod http_error;

use std::sync::Arc;
use axum::{extract::State, routing::post, Json, Router};
use axum::extract::{Path, Query};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use futures::{future, pin_mut, FutureExt};
use hyper::http::HeaderValue;
use reqwest::ClientBuilder;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use tokio::{fs::File, io::AsyncReadExt, net::TcpListener};
use crate::get_user_info_error::MissingAuthorizationHeaderSnafu;

#[derive(Debug, Clone)]
struct AppState {
    http: reqwest::Client,
}


#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to parse config file"))]
    ParseConfig { source: serde_json::Error },

    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to construct http client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to open ca certificate"))]
    OpenCaCert { source: std::io::Error },

    #[snafu(display("failed to read ca certificate"))]
    ReadCaCert { source: std::io::Error },

    #[snafu(display("failed to parse ca certificate"))]
    ParseCaCert { source: reqwest::Error },
}


#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let shutdown_requested = tokio::signal::ctrl_c().map(|_| ());
    #[cfg(unix)]
        let shutdown_requested = {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context(RegisterSigtermSnafu)?;
        async move {
            let sigterm = sigterm.recv().map(|_| ());
            pin_mut!(shutdown_requested, sigterm);
            future::select(shutdown_requested, sigterm).await;
        }
    };

    let mut client_builder = ClientBuilder::new();

    let http = client_builder.build().context(ConstructHttpClientSnafu)?;

    let app = Router::new()
        .route("/oncallnumber", get(get_person_on_call))
        .with_state(AppState {
            http,
        });
    let listener = TcpListener::bind("127.0.0.1:2368")
        .await
        .context(BindListenerSnafu)?;

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase", untagged)]
enum Schedule {
    ScheduleById(ScheduleRequestById),
    ScheduleByName(ScheduleRequestByName),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct ScheduleRequestByName {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct ScheduleRequestById {
    id: String,
}



#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
struct PersonInfo {
    phone_number: String,
}


#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("request is missing AUTHORIZATION header"))]
    MissingAuthorizationHeader {  },
}

impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> StatusCode {
        match self {
            GetUserInfoError::MissingAuthorizationHeader { .. } => {StatusCode::UNAUTHORIZED}
        }
    }
}

async fn get_person_on_call(
    State(state): State<AppState>,
    Query(requested_schedule): Query<Schedule>,
    headers: HeaderMap
) -> Result<Json<PersonInfo>, http_error::JsonResponse<Arc<GetUserInfoError>>> {
    let AppState {
        http,
    } = state;

    let bearer_token = headers.get(AUTHORIZATION).context(MissingAuthorizationHeaderSnafu)?;
    println!("headers: {:?}", headers);
    match requested_schedule {
        Schedule::ScheduleById(id) => {
            let id = id.id;
            println!("retrieving schedule by id {id}");
            let authorization_header = headers.get(AUTHORIZATION).unwrap_or_default().to_str().unwrap_or_default();
            println!("authz header: {}", authorization_header);
        }
        Schedule::ScheduleByName(name) => {
            let name = name.name;
            println!("retrieving schedule by name {name}");
        }
    }

    Ok(Json(
        PersonInfo { phone_number: "+491234".to_string() }
    ))
}

