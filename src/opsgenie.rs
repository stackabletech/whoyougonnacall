use crate::opsgenie::error::{
    NoOnCallPersonSnafu, NoPhoneNumberSnafu,
    RequestOnCallPersonSnafu, RequestPhoneNumberForPersonSnafu,
};
use crate::util::send_json_request;
use crate::{http_error, AlertInfo, Schedule};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use hyper::header::AUTHORIZATION;
use reqwest::header::HOST;
use reqwest::{Client, Url};
use secrecy::ExposeSecret;
use serde::{Serialize, Deserialize};
use snafu::{OptionExt, ResultExt, Snafu};
use crate::config::Config;

static OPSGENIE_BASEURL: &str = "https://api.opsgenie.com/v2/";
#[derive(Snafu, Debug)]
#[snafu(module)]
pub(crate) enum Error {
    #[snafu(display("requesting on call person failed"))]
    RequestOnCallPerson { source: crate::util::Error },
    #[snafu(display("requesting phone number failed for [{username}]"))]
    RequestPhoneNumberForPerson {
        source: crate::util::Error,
        username: String,
    },
    #[snafu(display("OpsGenie says no one is currently on call!"))]
    NoOnCallPerson {},
    #[snafu(display("User [{username}] has no phone number configured!"))]
    NoPhoneNumber { username: String },
}

impl http_error::Error for Error {
    fn status_code(&self) -> StatusCode {
        match self {
            Error::RequestOnCallPerson { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Error::NoOnCallPerson { .. } => StatusCode::IM_A_TEAPOT,
            Error::NoPhoneNumber { .. } => StatusCode::IM_A_TEAPOT,
            Error::RequestPhoneNumberForPerson { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct UserPhoneNumber {
    pub name: String,
    pub phone: Vec<String>,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OnCallResult {
    data: OnCallResultData,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OnCallResultData {
    on_call_recipients: Vec<String>,
}

pub fn get_base_url() -> Result<Url, url::ParseError> {
    Url::parse(OPSGENIE_BASEURL)
}

pub(crate) async fn get_oncall_number(
    schedule: &Schedule,
    http: &Client,
    config: &Config,
) -> Result<AlertInfo, Error> {
    let Config { opsgenie_config, slack_config, .. } = config;
    let mut url_builder = opsgenie_config.base_url.clone();

    let (schedule_identifier, schedule_identifier_type) = match schedule {
        Schedule::ScheduleById(id) => (&id.id, "id"),
        Schedule::ScheduleByName(name) => (&name.name, "name"),
    };

    url_builder = url_builder
        .join(&format!("schedules/{schedule_identifier}/on-calls"))
        .unwrap();

    let mut outgoing_headers = HeaderMap::new();
    outgoing_headers.insert(AUTHORIZATION, opsgenie_config.credentials.expose_secret().clone().0);

    tracing::debug!("Retrieving on call person from [{}]", url_builder.to_string());
    tracing::debug!("Using headers: [{:?}]", outgoing_headers);

    let persons_on_call = send_json_request::<OnCallResult>(
        http.get(url_builder.clone())
            .headers(outgoing_headers.clone())
            .query(&[
                ("flat", "true"),
                ("scheduleIdentifierType", schedule_identifier_type),
            ]),
    )
    .await
    .context(RequestOnCallPersonSnafu)?;

    // We don't need this value, this is just to check the response wasn't empty and no one is
    // on call
    persons_on_call
        .data
        .on_call_recipients
        .get(0)
        .context(NoOnCallPersonSnafu)?;

    let mut result_list: Vec<UserPhoneNumber> = Vec::new();

    for user in persons_on_call.data.on_call_recipients {
        println!("Looking up phone number for user [{}]", user);
        let phone_number =
            get_phone_number(http.clone(), opsgenie_config.base_url.clone(), &outgoing_headers, &user)
                .await
                .context(RequestPhoneNumberForPersonSnafu { username: &user })?;
        result_list.push(UserPhoneNumber {
            name: user.to_string(),
            phone: phone_number,
        })
    }

    println!("{:?}", result_list);
    let user = result_list.get(0).context(NoOnCallPersonSnafu)?;
    let username = &user.name;
    let phone_number = user.phone.get(0).context(NoPhoneNumberSnafu{ username: username})?;

    Ok(AlertInfo {
        username: username.clone(),
        phone_number: phone_number.clone(),
        full_information: result_list
    })
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ContactInformationResult {
    data: ContactInformationResultData,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ContactInformationResultData {
    id: String,
    username: String,
    full_name: String,
    userContacts: Vec<UserContact>,
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UserContact {
    to: String,
    id: String,
    contactMethod: String,
    enabled: bool,
}

async fn get_phone_number(
    http: Client,
    base_url: Url,
    headers: &HeaderMap,
    username: &str,
) -> Result<Vec<String>, crate::util::Error> {
    let url_builder = base_url.clone();
    let url_builder = url_builder.join(&format!("users/{username}")).unwrap();
    tracing::debug!("Retrieving contact information for [{}] information from [{}]", username, url_builder.to_string());
    tracing::debug!("Using headers: [{:?}]", headers);
    let contact_information = send_json_request::<ContactInformationResult>(
        http.get(url_builder.clone())
            .headers(headers.clone())
            .query(&[("expand", "contact")]),
    )
    .await?;
    tracing::trace!("Got data from opsgenie: [{:?}]", contact_information);

    let mut numbers = contact_information
        .data
        .userContacts
        .iter()
        .filter(|user_contact| user_contact.contactMethod.eq("voice") || user_contact.contactMethod.eq("sms"))
        .map(|user_contact| format_phone_number(user_contact.to.clone()))
        .collect::<Vec<String>>();

    // Sort to enable easier deduplication and remove duplicate numbers
    numbers.sort();
    numbers.dedup();

    Ok(numbers)
}

fn format_phone_number(number: String) -> String {
    let number = number.replace("-", "");
    format!("+{}", number)
}