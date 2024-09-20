use crate::config::{Config, TwilioConfig};
use crate::twilio::error::BuildUrlSnafu;
use crate::util::send_json_request;
use crate::{http_error, AlertInfo};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use futures::future::join_all;
use reqwest::Client;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::HashMap;
use tracing::instrument;
use url::{ParseError, Url};
use urlencoding::encode;

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

#[instrument(name = "dial_outgoing")]
pub async fn alert(
    numbers: &Vec<String>,
    http: &Client,
    config: &Config,
) -> Result<AlertResult, crate::twilio::Error> {
    let twilio_config = &config.twilio_config;
    tracing::trace!(?twilio_config.base_url, "url_builder before adding workflow"
    );
    tracing::trace!(twilio_config.workflow_id, "triggering twilio_workflow");
    let mut url_builder = twilio_config
        .base_url
        .join(&format!("{}/Executions/", &twilio_config.workflow_id))
        .context(BuildUrlSnafu)?;
    tracing::trace!(?url_builder, "url_builder after adding workflow");

    let mut outgoing_headers = HeaderMap::new();
    outgoing_headers.insert(
        AUTHORIZATION,
        twilio_config.credentials.expose_secret().clone().0,
    );
    // Create the Hashmap once here, we'll overwrite the "To" field for every iteration....
    // .. no we won't, we are parallelizing here, so we clone
    let mut params = HashMap::new();
    params.insert("From", twilio_config.outgoing_number.clone());
    tracing::info!(
        ?numbers,
        ?url_builder,
        ?params,
        twilio_config.outgoing_number,
        "These numbers will be alerted via Twilio."
    );

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

    //results.iter().map(|(number, result)| result.and_then())

    let mut response = AlertResult {
        overall_result: OverallResult::Success,
        detailed_result: vec![],
    };
    for (number, result) in results {
        response.detailed_result.push(match result {
            Ok(response) => {
                if response.status.eq("active") {
                    DialNumberResult::Success { number }
                } else {
                    DialNumberResult::Unknown {
                        number,
                        status: response.status,
                    }
                }
            }
            Err(e) => DialNumberResult::Failure {
                number,
                error: e.to_string(),
            },
        });
    }
    response.update_overall_result();

    // TODO: Properly parse the responses and generate a response here that distinguishes between
    //  1. HTTP Error Code
    //  2. HTTP 200 and "active=true"
    //  3. HTTP 200 but "active != true" - we suspect that this would mean a Twilio-side failure
    Ok(response)
}

pub fn get_base_url() -> Result<Url, url::ParseError> {
    Url::parse(crate::twilio::TWILIO_BASEURL)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub enum OverallResult {
    Success,
    PartialSuccess,
    Failure,
}

/// Success when we get back http 200 and active=true in the response
/// Failure for any http response code != 200
/// Unknown for http 200 but active=true is missing in response (I don't really know when this happens
/// some Twilio docs reading may be necessary)
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub enum DialNumberResult {
    Success { number: String },
    Failure { number: String, error: String },
    Unknown { number: String, status: String },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AlertResult {
    pub overall_result: OverallResult,
    pub detailed_result: Vec<DialNumberResult>,
}

impl AlertResult {
    pub fn update_overall_result(&mut self) {
        let succeeded_calls = self.detailed_result.iter().any(|s| match s {
            DialNumberResult::Success { .. } => true,
            _ => false,
        });

        let unknown_calls = self.detailed_result.iter().any(|s| match s {
            DialNumberResult::Unknown { .. } => true,
            _ => false,
        });

        let failed_calls = self.detailed_result.iter().any(|s| match s {
            DialNumberResult::Failure { .. } => true,
            _ => false,
        });

        self.overall_result = if succeeded_calls && (unknown_calls || failed_calls) {
            OverallResult::PartialSuccess
        } else if succeeded_calls && !(unknown_calls || failed_calls) {
            OverallResult::Success
        } else {
            OverallResult::Failure
        }
    }
}

#[cfg(test)]
mod test {
    use super::{AlertResult, DialNumberResult, OverallResult};
    use rstest::rstest;
    use stackable_operator::cluster_resources::ClusterResourceApplyStrategy::Default;

    #[rstest]
    // Order of columns: success, unknown, failed, result
    #[case(true, true, true, OverallResult::PartialSuccess)]
    #[case(false, true, true, OverallResult::Failure)]
    #[case(true, false, true, OverallResult::PartialSuccess)]
    #[case(true, true, false, OverallResult::PartialSuccess)]
    #[case(true, false, false, OverallResult::Success)]
    #[case(false, false, true, OverallResult::Failure)]
    #[case(false, true, false, OverallResult::Failure)]
    #[case(false, false, false, OverallResult::Failure)]
    fn test_update_overall_status(
        #[case] success: bool,
        #[case] unknown: bool,
        #[case] failed: bool,
        #[case] expected: OverallResult,
    ) {
        let mut result = AlertResult {
            overall_result: OverallResult::Success,
            detailed_result: vec![],
        };
        if success {
            result.detailed_result.push(DialNumberResult::Success {
                number: "".to_string(),
            })
        };
        if unknown {
            result.detailed_result.push(DialNumberResult::Unknown {
                number: "".to_string(),
                status: "".to_string(),
            })
        };
        if failed {
            result.detailed_result.push(DialNumberResult::Failure {
                number: "".to_string(),
                error: "".to_string(),
            })
        }
        result.update_overall_result();
        assert_eq!(result.overall_result, expected);
    }
}
