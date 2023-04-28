// Copyright 2023 Parity Technologies (UK) Ltd.
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

use api::JaegerApi;
use clap::Parser;
use color_eyre::eyre::eyre;
use futures::future;
use log::{debug, error};
use polkadot_introspector_essentials::init;
use primitives::TraceObject;
use serde::Serialize;
use std::{borrow::Borrow, str::FromStr};

mod api;
mod primitives;

/// Mode of this command
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum JaegerMode {
	/// Get a specific trace
	#[clap(arg_required_else_help = true)]
	Trace {
		/// Trace ID
		id: String,
	},
	/// Get all traces
	AllTraces {
		/// Specific service to get
		service: String,
	},
	/// Get all services
	Services,
	/// Prometheus endpoint mode.
	Prometheus(JaegerPrometheusOptions),
}

/// Output mode for the CLI commands
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum OutputMode {
	Pretty,
	Json,
}

impl FromStr for OutputMode {
	type Err = &'static str;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"pretty" => Ok(OutputMode::Pretty),
			"json" => Ok(OutputMode::Json),
			_ => Err("invalid output mode"),
		}
	}
}

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Examine jaeger traces")]
pub(crate) struct JaegerOptions {
	/// Name a specific node that reports to the Jaeger Agent from which to query traces.
	#[clap(long)]
	service: Option<String>,
	/// URL where Jaeger UI Service runs.
	#[clap(long, default_value = "http://localhost:16686")]
	url: String,
	/// Maximum number of traces to return.
	#[clap(long)]
	limit: Option<usize>,
	/// specify how far back in time to look for traces. In format: `1h`, `1d`
	#[clap(long)]
	lookback: Option<String>,
	/// HTTP API timeout
	#[clap(long, default_value = "10.0")]
	timeout: f32,
	/// Pretty print output of the commands (pretty by default)
	#[clap(long, default_value = "pretty")]
	output: OutputMode,
	/// Mode of running - CLI/Prometheus.
	#[clap(subcommand)]
	mode: JaegerMode,
	#[clap(flatten)]
	verbose: init::VerbosityOptions,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct JaegerPrometheusOptions {
	/// Prometheus endpoint port.
	#[clap(long, default_value = "65432")]
	port: u16,
	/// How often should we check Jaeger UI
	#[clap(long, default_value = "1.0")]
	check_interval: f32,
}

impl From<&JaegerOptions> for api::JaegerApiOptions {
	fn from(cli_opts: &JaegerOptions) -> Self {
		api::JaegerApiOptions::builder()
			.limit(cli_opts.limit)
			.service(cli_opts.service.clone())
			.lookback(cli_opts.lookback.clone())
			.timeout(cli_opts.timeout)
			.build()
	}
}

pub(crate) struct JaegerTool {
	opts: JaegerOptions,
	api: api::JaegerApi,
}

impl JaegerTool {
	/// Returns a new jaeger tool
	pub fn new(opts: JaegerOptions) -> color_eyre::Result<Self> {
		let api = JaegerApi::new(opts.url.as_str(), &opts.borrow().into());
		debug!("created Jaeger API client");
		Ok(Self { opts, api })
	}

	pub async fn run(self) -> color_eyre::Result<Vec<tokio::task::JoinHandle<color_eyre::Result<()>>>> {
		let mut futures: Vec<tokio::task::JoinHandle<color_eyre::Result<()>>> = vec![];
		match self.opts.mode {
			JaegerMode::Trace { id } => {
				futures.push(tokio::spawn(async move {
					let trace = self
						.api
						.trace(id.as_str())
						.await
						.map_err(|e| eyre!("Cannot get trace: {:?}", e))?;
					let response: Vec<TraceObject> = self
						.api
						.to_json(trace.as_str())
						.map_err(|e| eyre!("Cannot parse trace json: {:?}", e))?;
					format_output(&response, self.opts.output)?;
					Ok(())
				}));
			},
			JaegerMode::AllTraces { service } => {
				futures.push(tokio::spawn(async move {
					let traces = self
						.api
						.traces(service.as_str())
						.await
						.map_err(|e| eyre!("Cannot get traces: {:?}", e))?;
					let response: Vec<TraceObject> = self
						.api
						.to_json(traces.as_str())
						.map_err(|e| eyre!("Cannot parse traces json: {:?}", e))?;
					format_output(&response, self.opts.output)?;
					Ok(())
				}));
			},
			JaegerMode::Services => {
				futures.push(tokio::spawn(async move {
					let services = self.api.services().await.map_err(|e| eyre!("Cannot get services: {:?}", e))?;
					let response: Vec<String> = self
						.api
						.to_json(services.as_str())
						.map_err(|e| eyre!("Cannot parse services json: {:?}", e))?;
					format_output(&response, self.opts.output)?;
					Ok(())
				}));
			},
			JaegerMode::Prometheus(_) => {
				todo!();
			},
		}

		Ok(futures)
	}
}

fn format_output<T: Serialize>(input: &Vec<T>, mode: OutputMode) -> color_eyre::Result<()> {
	let res = match mode {
		OutputMode::Pretty => serde_json::to_string_pretty(input)?,
		OutputMode::Json => serde_json::to_string(input)?,
	};

	println!("{}", res);

	Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = JaegerOptions::parse();
	init::init_cli(&opts.verbose)?;

	let jaeger_cli = JaegerTool::new(opts)?;
	match jaeger_cli.run().await {
		Ok(futures) => {
			let results = future::try_join_all(futures).await.map_err(|e| eyre!("Join error: {:?}", e))?;
			for res in results.iter() {
				if let Err(err) = res {
					error!("FATAL: {}", err);
				}
			}
		},
		Err(err) => error!("FATAL: cannot start jaeger command: {}", err),
	};

	Ok(())
}
