use async_stream::stream;
use futures_core::stream::Stream;
use futures_util::stream::StreamExt;
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
}

pub struct Client {
    client: reqwest::Client,
    url: Url,
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
        Self {
            client: reqwest::Client::new(),
            url: base.join("/operations/").unwrap(),
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
        streaming_request(self, subpath.as_ref(), input, false).await
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
        streaming_request(self, subpath.as_ref(), input, true).await
    }
}

async fn decode_response<T>(subpath: &str, resp: reqwest::Response) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    // Always try to decode the response first. Since even values with non-200
    // HTTP codes might contain useful error messages
    let status = resp.status();
    match resp.json::<Response<T>>().await {
        Ok(result) => {
            // Response was decoded. If it's a GraphQL error, insert the status code
            match result {
                Response::Data(data) => Ok(data.data),
                Response::Error(error) => Err(ResponseError {
                    status_code: status.as_u16(),
                    code: error.code,
                    errors: error.errors,
                }
                .into()),
            }
        }
        Err(error) => {
            if !status.is_success() {
                error!(
                    "request to {} failed with status: {}",
                    subpath,
                    status.as_u16()
                );
                return Err(Error::InvalidHTTPStatusCodeError(status.as_u16()));
            }
            Err(anyhow::anyhow!("failed to parse response: {}", error).into())
        }
    }
}

async fn streaming_request<T, U>(
    client: &Client,
    subpath: &str,
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

    let req = client
        .client
        .get(url)
        .query(&[("wg_variables", input)])
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

    if !resp.status().is_success() {
        error!(
            "subscription/live query failed with status: {}",
            resp.status().as_u16()
        );
        return Err(Error::InvalidHTTPStatusCodeError(resp.status().as_u16()));
    }

    let mut resp_stream = resp.bytes_stream();

    let stream = stream!(while let Some(item) = resp_stream.next().await {
        let item = item.map_err(|e| anyhow::anyhow!("failed to read response: {}", e))?;
        let item = serde_json::from_slice::<U>(&item)
            .map_err(|e| anyhow::anyhow!("failed to parse response: {}", e))?;
        yield Ok(item);
    });

    Ok(stream)
}
