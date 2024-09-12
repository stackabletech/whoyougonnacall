use crate::opsgenie::error::{
    MissingAuthorizationHeaderSnafu, NoOnCallPersonSnafu, NoPhoneNumberSnafu,
    RequestOnCallPersonSnafu, RequestPhoneNumberForPersonSnafu,
};
use crate::opsgenie::Error::{
    NoOnCallPerson, NoPhoneNumber, RequestOnCallPerson, RequestPhoneNumberForPerson,
};
use crate::util::send_json_request;
use crate::{get_person_on_call, http_error, PersonInfo, Schedule};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use futures::future::join_all;
use reqwest::header::{ACCEPT, AUTHORIZATION, HOST};
use reqwest::{Client, Response, Url};
use serde::Deserialize;
use snafu::{OptionExt, ResultExt, Snafu};

static OPSGENIE_BASEURL: &str = "https://api.opsgenie.com/v2/";
#[derive(Snafu, Debug)]
#[snafu(module)]
pub(crate) enum Error {
    #[snafu(display("request is missing AUTHORIZATION header"))]
    MissingAuthorizationHeader {},
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
            Error::MissingAuthorizationHeader { .. } => StatusCode::UNAUTHORIZED,
            Error::RequestOnCallPerson { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Error::NoOnCallPerson { .. } => StatusCode::IM_A_TEAPOT,
            Error::NoPhoneNumber { .. } => StatusCode::IM_A_TEAPOT,
            Error::RequestPhoneNumberForPerson { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Clone, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UserPhoneNumber {
    name: String,
    phone: Vec<String>,
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
    schedule: Schedule,
    headers: HeaderMap,
    http: Client,
    base_url: Url,
) -> Result<PersonInfo, Error> {
    let authorization_header = headers
        .get(AUTHORIZATION)
        .context(MissingAuthorizationHeaderSnafu)?;

    let mut url_builder = base_url.clone();

    let (schedule_identifier, schedule_identifier_type) = match schedule {
        Schedule::ScheduleById(id) => (id.id, "id"),
        Schedule::ScheduleByName(name) => (name.name, "name"),
    };

    url_builder = url_builder
        .join(&format!("schedules/{schedule_identifier}/on-calls"))
        .unwrap();

    // TODO: Double check if this is even necessary or if reqwest maybe
    //  rewrites the host header anyway
    let mut outgoing_headers = headers.clone();
    outgoing_headers.remove(HOST);

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
            get_phone_number(http.clone(), base_url.clone(), &outgoing_headers, &user)
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
    Ok(PersonInfo {
        username: username.clone(),
        phone_number: phone_number.clone(),
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
    let mut url_builder = url_builder.join(&format!("users/{username}")).unwrap();
    let contact_information = send_json_request::<ContactInformationResult>(
        http.get(url_builder.clone())
            .headers(headers.clone())
            .query(&[("expand", "contact")]),
    )
    .await?;
    let numbers = contact_information
        .data
        .userContacts
        .iter()
        .filter(|user_contact| user_contact.contactMethod.eq("voice"))
        .map(|user_contact| format_phone_number(user_contact.to.clone()))
        .collect::<Vec<String>>();

    Ok(numbers)
}
// https://api.opsgenie.com/v2/users/soenke.liebau@stackable.tech?expand=contact

fn format_phone_number(number: String) -> String {
    let number = number.replace("-", "");
    format!("+{}", number)
}