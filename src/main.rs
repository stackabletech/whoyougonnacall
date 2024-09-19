mod config;
mod http_error;
mod opsgenie;
mod twilio;
mod util;

use crate::config::{enable_log_exporter, enable_trace_exporter, Config, ConfigError};
use crate::opsgenie::{get_oncall_number, UserPhoneNumber};
use crate::twilio::alert;
use crate::StartupError::{InitializeTelemetry, ParseConfig};
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
use stackable_telemetry::{AxumTraceLayer, Tracing};
use std::env;
use std::env::var_os;
use std::ffi::OsString;
use std::fmt::{Debug, Display, Formatter};
use std::process::{ExitCode, Termination};
use std::str::ParseBoolError;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
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
    #[snafu(display("failed to register SIGTERM handler: \n{source}"))]
    RegisterSigterm { source: std::io::Error },

    #[snafu(display("Failed parsing config: \n{source}"))]
    ParseConfig { source: ConfigError },

    #[snafu(display("failed to bind listener: \n{source}"))]
    BindListener { source: std::io::Error },

    #[snafu(display("failed to run server: \n{source}"))]
    RunServer { source: stackable_webhook::Error },

    #[snafu(display("failed to construct http client: \n{source}"))]
    ConstructHttpClient { source: reqwest::Error },

    #[snafu(display("failed to initialize tracing: \n{source}"))]
    InitializeTelemetry {
        source: stackable_telemetry::tracing::Error,
    },
}

#[derive(Snafu, Debug)]
#[snafu(module)]
enum RequestError {
    #[snafu(display("error when obtaining information from OpsGenie: : \n{source}"))]
    OpsGenie { source: opsgenie::Error },
    #[snafu(display("error when communicating with Twilio: : \n{source}"))]
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
async fn main() -> ExitCode {
    match run().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            // TODO: Not sure whats better here, we log both for now
            eprintln!("{}", e);
            eprintln!("{:?}", e);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), StartupError> {
    let mut builder = Tracing::builder()
        .service_name("whoyougonnacall")
        .with_console_output("WYGC_CONSOLE", LevelFilter::INFO);

    // Read env vars for whether to enable trace and log exporting
    // We do this first in order to have tracing properly initialized
    // when we start parsing the config
    if enable_trace_exporter().context(ParseConfigSnafu)? {
        builder = builder.with_otlp_trace_exporter("TEST_OTLP_TRACE", LevelFilter::TRACE);
    }
    if enable_log_exporter().context(ParseConfigSnafu)? {
        builder = builder.with_otlp_log_exporter("TEST_OTLP_LOG", LevelFilter::TRACE);
    }

    let _tracing_guard = builder.build().init().context(InitializeTelemetrySnafu)?;

    // Create config object and error out if anything goes wrong
    let config = Config::new().context(ParseConfigSnafu)?;

    tracing::info!(?config, "Config parsed successfully");

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
    tracing::debug!(?http, "Reqwest client initialized");

    use axum::Router;
    use stackable_webhook::{Options, WebhookServer};

    let app = Router::new()
        .route("/oncallnumber", get(get_person_on_call))
        .route("/alert", get(alert_on_call))
        .route("/status", get(health))
        .with_state(AppState {
            http,
            config: config.clone(),
            // TODO: get rid of the .clone() but ... lifetimes ... shared state is not easy
            //  https://stackoverflow.com/questions/75121484/shared-state-doesnt-work-because-of-lifetimes
        });

    let server = WebhookServer::new(
        app,
        Options::builder()
            .bind_address(config.bind_address, config.bind_port)
            .build(),
    );

    /*let bind_address = format!("{}:{}", &config.bind_address, &config.bind_port);
    let listener = TcpListener::bind(&bind_address)
        .await
        .context(BindListenerSnafu)?;
    tracing::info!("Bound to [{}]", &bind_address);*/

    tracing::info!("Starting server ..");
    /*axum::serve(listener, app.into_make_service())
       .with_graceful_shutdown(shutdown_requested)
       .await
       .context(RunServerSnafu)

    */
    Ok(server.run().await.context(RunServerSnafu)?)
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
    tracing::info!("Responding healthy to healthcheck");
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
