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
use clap::Parser;
use log::{debug, error, info, warn};
use prometheus_endpoint::{HistogramVec, Registry};

mod api;
mod primitives;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) enum JaegerMode {
	/// CLI chart mode.
	Cli(JaegerCliOptions),
	/// Prometheus endpoint mode.
	Prometheus(JaegerPrometheusOptions),
}
#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
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
	max_age: Option<String>,
	/// Mode of running - cli/prometheus.
	#[clap(subcommand)]
	mode: JaegerMode,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct JaegerCliOptions {
	/// Chart width.
	#[clap(long, default_value = "80")]
	chart_width: usize,
	/// Chart height.
	#[clap(long, default_value = "6")]
	chart_height: usize,
}

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct JaegerPrometheusOptions {
	/// Prometheus endpoint port.
	#[clap(long, default_value = "65432")]
	port: u16,
}
