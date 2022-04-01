// Copyright 2022 Parity Technologies (UK) Ltd.
// This file is part of polkadot-introspector.
//
// polkadot-introspector is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// polkadot-introspector is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with polkadot-introspector.  If not, see <http://www.gnu.org/licenses/>.
//

//! A wrapper for Jaeger HTTP API

use log::debug;
use reqwest;
use serde::Deserialize;
use std::error::Error;
use std::time::Duration;
use typed_builder::TypedBuilder;

use super::primitives::*;

/// `/api/traces`
/// Params:
///     limit: specify how many to return
///     service: Where did the trace originate
///     prettyPrint: Make JSON nice
const TRACES_ENDPOINT: &'static str = "/api/traces";
/// `/api/services`
///     returns services reporting to the jaeger agent
const SERVICES_ENDPOINT: &'static str = "/api/services";

/// Used to distinguish our user-agent
const HTTP_UA: &'static str = "polkadot-introspector";

/// Main API exported module
pub struct JaegerApi {
	/// Base URL for the requests
	base_url: reqwest::Url,
	/// Cached urls for frequent requests
	traces_url: reqwest::Url,
	services_url: reqwest::Url,
	/// Async HTTP client
	http_client: reqwest::Client,
}

impl JaegerApi {
	/// Creates a new JaegerAPI
	pub fn new(url: &str, opts: &JaegerApiOptions) -> Self {
		let http_client = reqwest::Client::builder()
			.timeout(Duration::from_secs_f32(opts.timeout))
			.user_agent(HTTP_UA)
			.build()
			.expect("cannot build HTTP client");
		let base_url = reqwest::Url::parse(url).expect("cannot parse base URL");

		Self {
			base_url: base_url.clone(),
			traces_url: opts.enrich_base_url(base_url.join(TRACES_ENDPOINT).expect("cannot parse traces URL")),
			services_url: opts.enrich_base_url(base_url.join(SERVICES_ENDPOINT).expect("cannot parse services URL")),
			http_client,
		}
	}

	pub async fn traces(&self, service: &str) -> Result<String, Box<dyn Error>> {
		let mut url = self.traces_url.clone();
		append_query_param(&mut url, "service", service);
		let response = self.http_client.get(url).send().await?;
		debug!("got response from the /traces endpoint, {:?}", &response);
		response
			.text()
			.await
			.or_else(|err| Err(Box::new(err) as Box<dyn std::error::Error>))
	}

	pub async fn trace(&self, id: &str) -> Result<String, Box<dyn Error>> {
		let url = self.traces_url.join(format!("/{}", id).as_str())?;
		let response = self.http_client.get(url).send().await?;
		debug!("got response from the /traces/{} endpoint, {:?}", id, &response);
		response
			.text()
			.await
			.or_else(|err| Err(Box::new(err) as Box<dyn std::error::Error>))
	}

	pub async fn services(&self) -> Result<String, Box<dyn Error>> {
		let response = self.http_client.get(self.services_url.clone()).send().await?;
		debug!("got response from the /services endpoint, {:?}", &response);
		response
			.text()
			.await
			.or_else(|err| Err(Box::new(err) as Box<dyn std::error::Error>))
	}

	pub fn to_json<'a, T>(&self, response: &'a str) -> Result<Vec<T>, Box<dyn Error>>
	where
		T: Deserialize<'a>,
	{
		let response: RpcResponse<T> = serde_json::from_str(&response)?;
		Ok(response.consume())
	}
}

#[derive(TypedBuilder, Clone)]
pub struct JaegerApiOptions {
	#[builder(default)]
	limit: Option<usize>,
	#[builder(default)]
	service: Option<String>,
	#[builder(default)]
	lookback: Option<String>,
	#[builder(default = 10.0)]
	timeout: f32,
}

impl JaegerApiOptions {
	pub fn enrich_base_url(&self, mut base_url: reqwest::Url) -> reqwest::Url {
		if let Some(limit) = self.limit {
			append_query_param(&mut base_url, "limit", format!("{}", limit).as_str());
		}

		if let Some(ref service) = self.service {
			append_query_param(&mut base_url, "service", service.as_str());
		}

		if let Some(ref lookback) = self.lookback {
			append_query_param(&mut base_url, "lookback", lookback.as_str());
		}

		base_url
	}
}

fn append_query_param(url: &mut reqwest::Url, param: &str, value: &str) {
	url.query_pairs_mut().append_pair(param, value);
}
