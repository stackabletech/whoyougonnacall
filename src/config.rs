use crate::config::ConfigError::{MissingOptionalValue, MissingRequiredValue};
use crate::{opsgenie, twilio};
use secrecy::SecretString;
use snafu::{OptionExt, ResultExt, Snafu};
use std::env;
use std::ffi::OsString;
use url::Url;

static BIND_ADDRESS_ENVNAME: &str = "WYGC_BIND_ADDRESS";
static DEFAULT_BIND_ADDRESS: &str = "0.0.0.0";

static BIND_PORT_ENVNAME: &str = "WYGC_BIND_PORT";
static DEFAULT_BIND_PORT: &str = "2368";

static TWILIO_TOKEN_ENVNAME: &str = "WYGC_TWILIO_TOKEN";
static TWILIO_BASEURL_ENVNAME: &str = "WYGC_TWILIO_BASEURL";
static TWILIO_WORKFLOW_ENVNAME: &str = "WYGC_TWILIO_WORKFLOW";

static OPSGENIE_TOKEN_ENVNAME: &str = "WYGC_OPSGENIE_TOKEN";
static OPSGENIE_BASEURL_ENVNAME: &str = "WYGC_OPSGENIE_BASEURL";

static SLACK_TOKEN_ENVNAME: &str = "WYGC_SLACK_TOKEN";
static SLACK_BASEURL_ENVNAME: &str = "WYGC_SLACK_BASEURL";

#[derive(Snafu, Debug)]
enum ConfigError {
    #[snafu(display("failed to read value of [{envname}] env var as string"))]
    ConvertOsString { envname: String },

    #[snafu(display("missing mandatory configuration [{envname}]"))]
    MissingRequiredValue { envname: String },

    #[snafu(display("optional config value not found: [{envname}], the following functionality will be disabled: [{functionality}]"))]
    MissingOptionalValue {
        envname: String,
        functionality: String,
    },

    #[snafu(display("baseurl parse error for service [{service}] - THIS IS NOT ON YOU! It is an error in the code!"))]
    ConstructBaseUrl {
        source: url::ParseError,
        service: String,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    bind_address: String,
    bind_port: String,

    opsgenie_config: OpsgenieConfig,
    twilio_config: TwilioConfig,

    slack_config: Option<SlackConfig>,
}

#[derive(Debug, Clone)]
pub struct SlackConfig {
    url: Url,
    token: SecretString,
}

#[derive(Debug, Clone)]
pub struct OpsgenieConfig {
    base_url: Url,
    credentials: SecretString,
}

#[derive(Debug, Clone)]
pub struct TwilioConfig {
    base_url: Url,
    credentials: SecretString,
    workflow_id: String,
}

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
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

        let twilio_config = TwilioConfig::new()?;
        let opsgenie_config = OpsgenieConfig::new()?;

        // Attempt to parse SlackConfig, if no webhook is configured log a warning and continue,
        // if we encounter an actual error, abort startup
        let slack_config = match SlackConfig::new() {
            Ok(slack_config) => {Ok(Some(slack_config))}
            Err(ref e) if e.kind() == MissingOptionalValue => {Ok(None)}
            Err(e) => {e?}
        };

        // Put it all together into a filled config object
        Ok(Config {
            bind_address,
            bind_port,
            opsgenie_config,
            twilio_config,
            slack_config: None,
        })
    }
}

impl OpsgenieConfig {
    pub fn new() -> Result<Self, ConfigError> {
        // Parse OpsGenie specific configuration values from environment
        // TODO: the default should be in this module I guess..
        let base_url = opsgenie::get_base_url().context(ConstructBaseUrlSnafu {
            service: "opsgenie",
        })?;
        tracing::debug!("OpsGenie base url parsed as : [{}]", base_url.to_string());

        let credentials = SecretString::new(
            env::var_os(OPSGENIE_TOKEN_ENVNAME)
                .context(MissingRequiredValueSnafu {
                    envname: OPSGENIE_TOKEN_ENVNAME,
                })?
                .to_str()
                .context(ConvertOsStringSnafu {
                    envname: OPSGENIE_TOKEN_ENVNAME,
                })?
                .to_string(),
        );

        Ok(OpsgenieConfig {
            base_url,
            credentials,
        })
    }
}

impl TwilioConfig {
    pub fn new() -> Result<Self, ConfigError> {
        // Parse Twilio specific configuration values from environment
        // TODO: the default should be in this module I guess..
        let base_url =
            twilio::get_base_url().context(ConstructBaseUrlSnafu { service: "twilio" })?;
        tracing::debug!("Twilio base url parsed as : [{}]", base_url.to_string());

        let credentials = SecretString::new(
            env::var_os(TWILIO_TOKEN_ENVNAME)
                .context(MissingRequiredValueSnafu {
                    envname: TWILIO_TOKEN_ENVNAME,
                })?
                .to_str()
                .context(ConvertOsStringSnafu {
                    envname: TWILIO_TOKEN_ENVNAME,
                })?
                .to_string(),
        );

        let workflow_id = env::var_os(TWILIO_WORKFLOW_ENVNAME)
            .context(MissingRequiredValueSnafu {
                envname: TWILIO_WORKFLOW_ENVNAME,
            })?
            .to_str()
            .context(ConvertOsStringSnafu {
                envname: TWILIO_WORKFLOW_ENVNAME,
            })?
            .to_string();

        Ok(TwilioConfig {
            base_url,
            credentials,
            workflow_id,
        })
    }
}

impl SlackConfig {
    pub fn new() -> Result<Self, ConfigError> {
        // We try to parse the Slack Webhook url first, if that is not present, no harm done - we log
        // that we won't alert on Slack and go on our merry way
        // If the webhook is present but the token is missing that is not good, and we'll error out
        // with a "missing mandatory value" error, as we cannot call the webhook without a token

        let base_url = env::var_os(SLACK_BASEURL_ENVNAME)
            .context(MissingOptionalValueSnafu {
                envname: SLACK_BASEURL_ENVNAME,
                functionality: "slack notifications",
            })?
            .to_str()
            .context(ConvertOsStringSnafu {
                envname: SLACK_BASEURL_ENVNAME,
            })?
            .to_string();

        let token = SecretString::new(
            env::var_os(SLACK_TOKEN_ENVNAME)
                .context(MissingRequiredValueSnafu {
                    envname: SLACK_TOKEN_ENVNAME,
                })?
                .to_str()
                .context(ConvertOsStringSnafu {
                    envname: SLACK_TOKEN_ENVNAME,
                })?
                .to_string(),
        );

        Ok(SlackConfig { url, token })
    }
}
