mod http_error;
mod opsgenie;
mod util;

use crate::opsgenie::{get_oncall_number, UserPhoneNumber};
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{extract::State, Json, Router};
use futures::{future, pin_mut, FutureExt};
use reqwest::{ClientBuilder, Url};
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use std::env;
use std::ffi::OsString;
use stackable_operator::logging::TracingTarget;
use tokio::net::TcpListener;

static BIND_ADDRESS_ENVNAME: &str = "WYGC_BIND_ADDRESS";
static DEFAULT_BIND_ADDRESS: &str = "127.0.0.1";
static BIND_PORT_ENVNAME: &str = "WYGC_BIND_PORT";
static DEFAULT_BIND_PORT: &str = "2368";

pub const APP_NAME: &str = "who-you-gonna-call";

#[derive(Debug, Clone)]
struct AppState {
    http: reqwest::Client,
    opsgenie_baseurl: Url,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: std::io::Error },

    #[snafu(display("failed to construct http client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to read value of [{envname}] env var as string"))]
    ConvertOsString { envname: String },

    #[snafu(display("baseurl parse error - THIS IS NOT ON YOU! It is an error in the code!"))]
    ConstructBaseUrl { source: url::ParseError },
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum GetUserInfoError {
    #[snafu(display("error when obtaining information from OpsGenie"))]
    OpsGenie { source: opsgenie::Error },
}

impl http_error::Error for GetUserInfoError {
    fn status_code(&self) -> hyper::StatusCode {
        // todo: the warn here loses context about the scope in which the error occurred, eg: stackable_opa_user_info_fetcher::backend::keycloak
        // Also, we should make the log level (warn vs error) more dynamic in the backend's impl `http_error::Error for Error`
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::OpsGenie { source } => source.status_code(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    stackable_operator::logging::initialize_logging(
        "WHOYOUGONNACALL_LOG",
        APP_NAME,
        // TODO: Make this configurable
        TracingTarget::None,
    );

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

    // Determine address and port to listen on from env vars, use default values if not set
    // TODO: Pretty sure this is a garbage way of doing this, need to look at it some more
    //  `into_string()` probably is the way to go here ..
    let bind_address = env::var_os(BIND_ADDRESS_ENVNAME)
        .unwrap_or(OsString::from(DEFAULT_BIND_ADDRESS))
        .to_str()
        .context(ConvertOsStringSnafu {
            envname: DEFAULT_BIND_ADDRESS,
        })?
        .to_string();


    let bind_port = env::var_os(BIND_PORT_ENVNAME)
        .unwrap_or(OsString::from(DEFAULT_BIND_PORT))
        .to_str()
        .context(ConvertOsStringSnafu {
            envname: DEFAULT_BIND_PORT,
        })?
        .to_string();

    let opsgenie_baseurl = opsgenie::get_base_url().context(ConstructBaseUrlSnafu)?;

    let http = ClientBuilder::new()
        .build()
        .context(ConstructHttpClientSnafu)?;

    let app = Router::new()
        .route("/oncallnumber", get(get_person_on_call))
        .with_state(AppState {
            http,
            opsgenie_baseurl,
        });
    let listener = TcpListener::bind(format!("{bind_address}:{bind_port}"))
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
struct AlertInfo {
    username: String,
    phone_number: String,
    full_information: Vec<UserPhoneNumber>
}

async fn get_person_on_call(
    State(state): State<AppState>,
    Query(requested_schedule): Query<Schedule>,
    headers: HeaderMap,
) -> Result<Json<AlertInfo>, http_error::JsonResponse<GetUserInfoError>> {
    let AppState {
        http,
        opsgenie_baseurl,
    } = state;

    Ok(Json(
        get_oncall_number(requested_schedule, headers, http, opsgenie_baseurl)
            .await
            .context(get_user_info_error::OpsGenieSnafu)?,
    ))
}
