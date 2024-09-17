| Enviroment Variable  | Description | Mandatory | Default Value  |
|---|---|---|---|
|WYGC_BIND_ADDRESS   |The address to bind the server to.   |No   |0.0.0.0   |
|WYGC_BIND_PORT   |Port to listen on for incoming connections.   |No   |2368   |
|WYGC_TWILIO_BASEURL   |Baseurl that will be used to connect to Twilio, there should normally be no reason to change this.   |No   |https://studio.twilio.com/v2/Flows/   |
|WYGC_TWILIO_WORKFLOW   |Workflow ID to call on Twilio.   |Yes   |   |
|WYGC_TWILIO_TOKEN   |Value of `AUTHORIZATION` header that will be set on requests to Twilio. Should have the format `Basic xxxxxx....`   |   |Yes   |
|WYGC_OPSGENIE_BASEURL   |Baseurl that will be used to connect to Twilio, there should normally be no reason to change this.   |No   |https://api.opsgenie.com/v2/   |
|WYGC_OPSGENIE_TOKEN   |Value of `AUTHORIZATION` header that will be set on requests to Opsgenie. Should have the format `GenieKey xxxxxx....`      |Yes   |   |
|WYGC_SLACK_BASEURL   |Webhook url for the slack channel to send alerts to. If not set, no slack notifications are attempted.   |No  |   |
|WYGC_SLACK_TOKEN   |   |Yes when WYGC_SLACK_BASEURL is set    |   |

