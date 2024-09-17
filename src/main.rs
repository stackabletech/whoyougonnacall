mod config;
mod http_error;
mod opsgenie;
mod twilio;
mod util;

use crate::config::{Config, ConfigError};
use crate::opsgenie::{get_oncall_number, UserPhoneNumber};
use crate::twilio::alert;
use crate::StartupError::ParseConfig;
use axum::body::Bytes;
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{extract::State, Json, Router};
use futures::{future, pin_mut, FutureExt};
use reqwest::{ClientBuilder, Url};
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, Snafu};
use stackable_operator::kube::config::InferConfigError;
use stackable_operator::logging::TracingTarget;
use std::env;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use stackable_telemetry::AxumTraceLayer;
use tokio::net::TcpListener;
use tracing::field::{Field, Visit};
use tracing::{instrument, Value};

static BIND_ADDRESS_ENVNAME: &str = "WYGC_BIND_ADDRESS";
static DEFAULT_BIND_ADDRESS: &str = "127.0.0.1";
static BIND_PORT_ENVNAME: &str = "WYGC_BIND_PORT";
static DEFAULT_BIND_PORT: &str = "2368";

pub const APP_NAME: &str = "who-you-gonna-call";

#[derive(Debug, Clone)]
struct AppState {
    http: reqwest::Client,
    config: Config,
}

#[derive(Snafu, Debug)]
enum StartupError {
    #[snafu(display("failed to register SIGTERM handler"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("Failed parsing config"))]
    ParseConfig { source: ConfigError },

    #[snafu(display("failed to bind listener"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server"))]
    RunServer { source: stackable_webhook::Error },

    #[snafu(display("failed to construct http client"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to read value of [{envname}] env var as string"))]
    ConvertOsString { envname: String },

    #[snafu(display("baseurl parse error for service [{service}] - THIS IS NOT ON YOU! It is an error in the code!"))]
    ConstructBaseUrl {
        source: url::ParseError,
        service: String,
    },
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum RequestError {
    #[snafu(display("error when obtaining information from OpsGenie"))]
    OpsGenie { source: opsgenie::Error },
    #[snafu(display("error when communicating with Twilio"))]
    Twilio { source: twilio::Error },
}

impl http_error::Error for RequestError {
    fn status_code(&self) -> hyper::StatusCode {
        // todo: the warn here loses context about the scope in which the error occurred, eg: stackable_opa_user_info_fetcher::backend::keycloak
        // Also, we should make the log level (warn vs error) more dynamic in the backend's impl `http_error::Error for Error`
        tracing::warn!(
            error = self as &dyn std::error::Error,
            "Error while processing request"
        );
        match self {
            Self::OpsGenie { source } => source.status_code(),
            Self::Twilio { source } => source.status_code(),
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

    // Create config object and error out if anything goes wrong
    let config = Config::new().context(ParseConfigSnafu)?;

    tracing::debug!("Registering shutdown hook..");
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

    let http = ClientBuilder::new()
        .build()
        .context(ConstructHttpClientSnafu)?;
    tracing::debug!("Reqwest client initialized ..");

    use stackable_webhook::{WebhookServer, Options};
    use axum::Router;

    let app = Router::new()
        .route("/oncallnumber", get(get_person_on_call))
        .route("/alert", get(alert_on_call))
        .route("/status", get(health))
        .with_state(AppState {
            http,
            config: config.clone(),
        }); // TODO: get rid of the .clone()

    let server = WebhookServer::new(app, Options::builder()
        .bind_address(config.bind_address.split(".").collect(), config.bind_port)
        .build());

    let bind_address = format!("{}:{}", &config.bind_address, &config.bind_port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .context(BindListenerSnafu)?;
    tracing::info!("Bound to [{}]", &bind_address);

    tracing::info!("Starting server ..");
    /*axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_requested)
        .await
        .context(RunServerSnafu)

     */
    server.run().await.context(RunServerSnafu)?
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase", untagged)]
enum Schedule {
    ScheduleById(ScheduleRequestById),
    ScheduleByName(ScheduleRequestByName),
}

#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    schedule: String,
    twilio_workflow: String,
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
    full_information: Vec<UserPhoneNumber>,
}

#[instrument(name = "health_check")]
async fn health() -> Result<Json<Status>, http_error::JsonResponse<RequestError>> {
    tracing::debug!("Responding healthy to healthcheck");
    Ok(Json(Status {
        health: Health::Healthy,
    }))
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    health: Health,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    Healthy,
    Sick,
}

#[instrument(name = "get_person")]
async fn get_person_on_call(
    State(state): State<AppState>,
    Query(requested_schedule): Query<Schedule>,
    headers: HeaderMap,
) -> Result<Json<AlertInfo>, http_error::JsonResponse<RequestError>> {
    let AppState { http, config } = state;
    tracing::info!("Got request for schedule [{:?}]", requested_schedule);
    Ok(Json(
        get_oncall_number(&requested_schedule, &http, &config)
            .await
            .context(request_error::OpsGenieSnafu)?,
    ))
}

#[instrument(name = "parse_config")]
async fn alert_on_call(
    State(state): State<AppState>,
    Query(requested_alert): Query<Alert>,
    headers: HeaderMap,
) -> Result<Json<AlertInfo>, http_error::JsonResponse<RequestError>> {
    let AppState { http, config } = state;
    tracing::info!("Got alert request [{:?}]", requested_alert);

    let schedule = Schedule::ScheduleByName(ScheduleRequestByName {
        name: requested_alert.schedule,
    });
    let twilio_workflow = requested_alert.twilio_workflow;
    tracing::trace!("twilio workflow: [{}]", twilio_workflow);
    let people_to_alert = get_oncall_number(&schedule, &http, &config)
        .await
        .context(request_error::OpsGenieSnafu)?;

    // Collect all phone number that we need to ring into one vec
    let numbers: Vec<String> = people_to_alert
        .full_information
        .iter()
        .map(|person| person.phone.clone())
        .flatten()
        .collect();

    tracing::info!("Will call these phones: [{:?}]", numbers);

    Ok(Json(
        alert(&numbers, &http, &config)
            .await
            .context(request_error::TwilioSnafu)?,
    ))
}
