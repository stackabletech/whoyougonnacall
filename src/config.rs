use crate::{opsgenie, twilio};
use snafu::{OptionExt, ResultExt, Snafu};
use std::env;
use std::ffi::OsString;
use url::Url;

static BIND_ADDRESS_ENVNAME: &str = "WYGC_BIND_ADDRESS";
static DEFAULT_BIND_ADDRESS: &str = "127.0.0.1";

static BIND_PORT_ENVNAME: &str = "WYGC_BIND_PORT";
static DEFAULT_BIND_PORT: &str = "2368";

static TWILIO_TOKEN_ENVNAME : &str = "WYGC_TWILIO_TOKEN";
static TWILIO_BASEURL_ENVNAME : &str = "WYGC_TWILIO_BASEURL";

static OPSGENIE_TOKEN_ENVNAME : &str = "WYGC_OPSGENIE_TOKEN";
static OPSGENIE_BASEURL_ENVNAME : &str = "WYGC_OPSGENIE_BASEURL";



#[derive(Snafu, Debug)]
enum ConfigError {
    #[snafu(display("failed to read value of [{envname}] env var as string"))]
    ConvertOsString { envname: String },

    #[snafu(display("baseurl parse error for service [{service}] - THIS IS NOT ON YOU! It is an error in the code!"))]
    ConstructBaseUrl {
        source: url::ParseError,
        service: String,
    },
}

pub struct Config {
    bind_address: String,
    bind_port: String,

    opsgenie_credentials: String,
    slack_webhook: Option<SlackWebhook>,
}

pub struct SlackWebhook {
    url: Url,
    token: String,
}

pub struct TwilioConfig {
    base_url: Url,
    credentials: String,
    workflow_id: String,
}

impl Config {
    pub fn new() -> Self {
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
        tracing::debug!("Bind address set to: [{}]", bind_address);

        let bind_port = env::var_os(BIND_PORT_ENVNAME)
            .unwrap_or(OsString::from(DEFAULT_BIND_PORT))
            .to_str()
            .context(ConvertOsStringSnafu {
                envname: DEFAULT_BIND_PORT,
            })?
            .to_string();
        tracing::debug!("Bind port set to: [{}]", bind_address);

        let opsgenie_baseurl = opsgenie::get_base_url().context(ConstructBaseUrlSnafu {
            service: "opsgenie",
        })?;
        tracing::debug!(
            "OpsGenie base url parsed as : [{}]",
            opsgenie_baseurl.to_string()
        );

        let twilio_baseurl =
            twilio::get_base_url().context(ConstructBaseUrlSnafu { service: "twilio" })?;
        tracing::debug!(
            "Twilio base url parsed as : [{}]",
            twilio_baseurl.to_string()
        );

        Config {
            bind_address,
            bind_port,
            opsgenie_credentials: "".to_string(),
            slack_webhook: None,
        }
    }
}
