// Based on Kirill Valiavin's initial client implementation
use async_stream::stream;
pub use futures_core::stream::Stream;
pub use futures_util::stream::StreamExt;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use crate::{
    errors::{Error, GraphQLErrors, Result},
    ResponseError,
};

#[derive(Default, Clone)]
pub struct ClientOptions {
    pub url: Option<Url>,
    pub application_hash: Option<String>,
}

pub struct Client {
    client: reqwest::Client,
    url: Url,
    application_hash: String,
}

#[derive(Deserialize, Serialize)]
struct ResponseData<T> {
    pub data: T,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Response<T> {
    Data(ResponseData<T>),
    Error(GraphQLErrors),
}

impl Client {
    pub fn new(options: ClientOptions) -> Self {
        let base = options
            .url
            .unwrap_or_else(|| Url::parse("http://localhost:9991/").unwrap());
        let application_hash = options.application_hash.unwrap_or_default();
        Self {
            client: reqwest::Client::new(),
            url: base.join("/operations/").unwrap(),
            application_hash,
        }
    }

    pub async fn query<P, I, R>(&self, subpath: P, input: I) -> Result<R>
    where
        P: AsRef<str>,
        I: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        let subpath = subpath.as_ref();
        let url = self
            .url
            .join(subpath)
            .map_err(|e| anyhow::anyhow!("failed to parse url subpath: {}", e))?;

        let data = serde_json::to_string(&input)?;

        let req = self
            .client
            .get(url)
            .query(&[("wg_variables", data)])
            .query(&[("wg_app_hash", &self.application_hash)])
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");

        debug!("query: {:?}", req);

        let resp = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("failed to send request: {}", e))?;

        decode_response(subpath, resp).await
    }

    pub async fn mutate<P, I, R>(&self, subpath: P, input: I) -> Result<R>
    where
        P: AsRef<str>,
        I: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        let subpath = subpath.as_ref();
        let url = self
            .url
            .join(subpath)
            .map_err(|e| anyhow::anyhow!("failed to parse url subpath: {}", e))?;

        let req = self
            .client
            .post(url)
            .query(&[("wg_app_hash", &self.application_hash)])
            .json(&input)
            .header("Accept", "application/json");

        debug!("mutation: {:?}", req);

        let resp = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("failed to send request: {}", e))?;

        decode_response(subpath, resp).await
    }

    pub async fn subscribe<P, I, R>(
        &self,
        subpath: P,
        input: I,
    ) -> Result<impl Stream<Item = Result<R>>>
    where
        P: AsRef<str>,
        I: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        streaming_request(self, subpath.as_ref(), &self.application_hash, input, false).await
    }

    pub async fn live_query<P, I, R>(
        &self,
        subpath: P,
        input: I,
    ) -> Result<impl Stream<Item = Result<R>>>
    where
        P: AsRef<str>,
        I: Serialize,
        R: for<'de> Deserialize<'de>,
    {
        streaming_request(self, subpath.as_ref(), &self.application_hash, input, true).await
    }
}

async fn decode_response<T>(subpath: &str, resp: reqwest::Response) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let status = resp.status();
    let data = resp
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("error reading response: {}", e))?;
    decode_bytes(subpath, status, &data)
}

fn decode_bytes<T>(subpath: &str, status_code: reqwest::StatusCode, data: &[u8]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    // Try to decode the response first. Since even values with non-200
    // HTTP codes might contain useful error messages
    match serde_json::from_slice::<Response<T>>(data) {
        Ok(response) => {
            // Response was decoded. If it's a GraphQL error, insert the status code
            match response {
                Response::Data(data) => Ok(data.data),
                Response::Error(error) => Err(ResponseError {
                    status_code: status_code.as_u16(),
                    code: error.code,
                    errors: error.errors,
                }
                .into()),
            }
        }
        Err(error) => {
            if !status_code.is_success() {
                error!(
                    "request to {} failed with status: {}",
                    subpath,
                    status_code.as_u16()
                );
                return Err(Error::InvalidHTTPStatusCodeError(status_code.as_u16()));
            }
            Err(error.into())
        }
    }
}

async fn streaming_request<T, U>(
    client: &Client,
    subpath: &str,
    application_hash: &str,
    input: T,
    live: bool,
) -> Result<impl Stream<Item = Result<U>>>
where
    T: Serialize,
    U: for<'de> Deserialize<'de>,
{
    let url = client
        .url
        .join(subpath)
        .map_err(|e| anyhow::anyhow!("failed to parse url subpath: {}", e))?;

    let data = serde_json::to_string(&input)?;

    let req = client
        .client
        .get(url)
        .query(&[("wg_variables", data)])
        .query(&[("wg_app_hash", application_hash)])
        .header("Accept", "application/json")
        .header("Content-Type", "application/json");

    let req = if live {
        req.query(&[("wg_live", true)])
    } else {
        req
    };

    debug!("Request: {:?}", req);

    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("failed to send request: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        error!(
            "subscription/live query failed with status: {}",
            resp.status().as_u16()
        );
        return Err(Error::InvalidHTTPStatusCodeError(resp.status().as_u16()));
    }

    let mut resp_stream = resp.bytes_stream();

    let subpath = String::from(subpath);
    let stream = stream!(while let Some(item) = resp_stream.next().await {
        // TODO: Handle chunking
        let data = item.map_err(|e| anyhow::anyhow!("failed to read response: {}", e))?;
        yield decode_bytes(&subpath, status, &data)
    });

    Ok(stream)
}
