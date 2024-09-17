use crate::opsgenie::get_oncall_number;
use crate::twilio::error::{BuildUrlSnafu, RunWorkflowSnafu};
use crate::twilio::Error::{BuildUrl, RunWorkflow};
use crate::util::send_json_request;
use crate::{http_error, AlertInfo, Schedule};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use futures::future::join_all;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::HashMap;
use secrecy::ExposeSecret;
use tracing::instrument;
use url::{ParseError, Url};
use urlencoding::encode;
use crate::config::{Config, TwilioConfig};

static TWILIO_BASEURL: &str = "https://studio.twilio.com/v2/Flows/";
#[derive(Snafu, Debug)]
#[snafu(module)]
pub(crate) enum Error {
    #[snafu(display("Twilio reported error when running the workflow"))]
    RunWorkflow { source: crate::util::Error },
    #[snafu(display("Error creating url for Twilio workflow"))]
    BuildUrl { source: url::ParseError },
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TwilioResponse {
    pub status: String,
}

impl http_error::Error for crate::twilio::Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Error::RunWorkflow { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            Error::BuildUrl { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[instrument(name = "parse_config")]
pub async fn alert(
    numbers: &Vec<String>,
    http: &Client,
    config: &Config,
) -> Result<AlertInfo, crate::twilio::Error> {
    let twilio_config = &config.twilio_config;
    tracing::debug!(
        "url_builder before adding workflow [{}]",
        twilio_config.base_url.to_string()
    );
    tracing::debug!("twilio_workflow: {}", twilio_config.workflow_id);
    let mut url_builder = twilio_config.base_url
        .join(&format!("{}/Executions/", &twilio_config.workflow_id))
        .context(BuildUrlSnafu)?;
    tracing::debug!(
        "url_builder after adding workflow [{}]",
        url_builder.to_string()
    );

    tracing::debug!("Using [{}] as alerting endpoint", url_builder.to_string());

    let mut outgoing_headers = HeaderMap::new();
    outgoing_headers.insert(AUTHORIZATION, twilio_config.credentials.expose_secret().clone().0);

    // Create the Hashmap once here, we'll overwrite the "To" field for every iteration....
    // .. no we won't, we are parallelizing here, so we clone
    let mut params = HashMap::new();
    params.insert("From", "+4941039263102".to_string());

    let requests = numbers
        .iter()
        .map(|number| async {
            let mut my_params = params.clone();
            my_params.insert("To", number.clone());

            (
                number.clone(),
                send_json_request::<TwilioResponse>(
                    http.post(url_builder.clone())
                        .headers(outgoing_headers.clone())
                        .form(&my_params),
                )
                .await,
            )
        })
        .collect::<Vec<_>>();

    let results = join_all(requests).await;

    // TODO: Properly parse the responses and generate a response here that distinguishes between
    //  1. HTTP Error Code
    //  2. HTTP 200 and "active=true"
    //  3. HTTP 200 but "active != true" - we suspect that this would mean a Twilio-side failure
    Ok(AlertInfo {
        username: "".to_string(),
        phone_number: "".to_string(),
        full_information: vec![],
    })
}

pub fn get_base_url() -> Result<Url, url::ParseError> {
    Url::parse(crate::twilio::TWILIO_BASEURL)
}
