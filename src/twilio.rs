use crate::opsgenie::get_oncall_number;
use crate::twilio::error::{BuildUrlSnafu, RunWorkflowSnafu};
use crate::twilio::Error::{BuildUrl, RunWorkflow};
use crate::util::send_json_request;
use crate::{http_error, AlertInfo, Schedule};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use futures::future::join_all;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::collections::HashMap;
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

pub async fn alert(
    numbers: &Vec<String>,
    twilio_workflow: &str,
    headers: &HeaderMap,
    http: &Client,
    twilio_base_url: &Url,
) -> Result<AlertInfo, crate::twilio::Error> {
    tracing::debug!(
        "url_builder before adding workflow [{}]",
        twilio_base_url.to_string()
    );
    tracing::debug!("twilio_workflow: {}", twilio_workflow);
    let mut url_builder = twilio_base_url
        .join(&format!("{}/Executions/", &twilio_workflow))
        .context(BuildUrlSnafu)?;
    tracing::debug!(
        "url_builder after adding workflow [{}]",
        url_builder.to_string()
    );

    tracing::debug!("Using [{}] as alerting endpoint", url_builder.to_string());

    let mut outgoing_headers = HeaderMap::new();
    if let Some(twiliokey) = headers.get("twilio_credentials") {
        tracing::debug!("Found twilio credentials, adding these to outgoing header..");
        outgoing_headers.insert(AUTHORIZATION, twiliokey.clone());
    }

    // Create the Hashmap once here, we'll overwrite the "To" field for every iteration....
    // .. no we won't, we are parallelizing here, so we clone
    let mut params = HashMap::new();
    params.insert("From", "+41039263102".to_string());

    let requests = numbers
        .iter()
        .map(|number| async {
            let mut my_params = params.clone();
            my_params.insert("To", number.clone());

            (number.clone(), send_json_request::<TwilioResponse>(
                http.post(url_builder.clone())
                    .headers(outgoing_headers.clone())
                    .form(&my_params),
            ).await)
        })
        .collect::<Vec<_>>();

    let (succeeded, failed)= join_all(requests).await.iter().partition(|result| result.1.is_ok());
    tracing::debug!("{:?}", succeeded);
    /*
    for number in numbers {
        let mut params = HashMap::new();
        params.insert("To", number.clone());
        params.insert("From", "+4941039263102".to_string());

        tracing::info!("Ringing [{}]", number);
        tracing::debug!("Calling url: [{}] with form encoded params [{:?}]", url_builder.to_string(), params);
        let twilio_response: TwilioResponse = send_json_request(
            http.post(url_builder.clone())
                .headers(outgoing_headers.clone())
                .form(&params)
        )
        .await.context(RunWorkflowSnafu)?;


        if twilio_response.status.eq("active") {
            tracing::info!("Twilio reported success!");
        }
    }

     */

    Ok(AlertInfo {
        username: "".to_string(),
        phone_number: "".to_string(),
        full_information: vec![],
    })
}

pub fn get_base_url() -> Result<Url, url::ParseError> {
    Url::parse(crate::twilio::TWILIO_BASEURL)
}
