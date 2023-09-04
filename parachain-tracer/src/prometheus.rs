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

use crate::{disputes_tracker::DisputesTracker, progress::ParachainProgressUpdate};
use clap::Parser;
use color_eyre::Result;
use polkadot_introspector_essentials::{constants::STANDARD_BLOCK_TIME, types::OnDemandOrder};
use prometheus_endpoint::{
	prometheus::{Gauge, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts},
	Registry,
};
use std::{net::ToSocketAddrs, time::Duration};

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub struct ParachainTracerPrometheusOptions {
	/// Address to bind Prometheus listener
	#[clap(short = 'a', long = "address", default_value = "0.0.0.0")]
	address: String,
	/// Port to bind Prometheus listener
	#[clap(short = 'p', long = "port", default_value = "65432")]
	port: u16,
}

#[derive(Clone)]
struct DisputesMetrics {
	/// Number of candidates disputed.
	disputed_count: IntCounterVec,
	concluded_valid: IntCounterVec,
	concluded_invalid: IntCounterVec,
	/// Average count of validators that voted against supermajority
	/// Average resolution time in blocks
	resolution_time: HistogramVec,
}

#[derive(Clone)]
struct MetricsInner {
	/// Number of backed candidates.
	backed_count: IntCounterVec,
	/// Number of skipped slots, where no candidate was backed and availability core
	/// was free.
	skipped_slots: IntCounterVec,
	/// Number of candidates included.
	included_count: IntCounterVec,
	/// Disputes stats
	disputes_stats: DisputesMetrics,
	/// Block time measurements for relay parent blocks
	relay_block_times: HistogramVec,
	/// Relative time measurements (in standard blocks) for relay parent blocks
	relay_skipped_slots: IntCounterVec,
	/// Number of slow availability events.
	slow_avail_count: IntCounterVec,
	/// Number of low bitfield propagation events.
	low_bitfields_count: IntCounterVec,
	/// Number of bitfields being set
	bitfields: IntGaugeVec,
	/// Average candidate inclusion time measured in relay chain blocks.
	para_block_times: HistogramVec,
	/// Average candidate backing time measured in relay chain blocks (will be 1 for non async-backing case)
	para_backing_times: HistogramVec,
	/// Average candidate inclusion time measured in seconds.
	para_block_times_sec: HistogramVec,
	/// Parachain's on-demand orders
	para_on_demand_orders: GaugeVec,
	/// Latency between ordering a slot by a parachain and its last backed candidate in relay blocks
	para_on_demand_delay: GaugeVec,
	/// Latency between ordering a slot by a parachain and its last backed candidate in seconds
	para_on_demand_delay_sec: GaugeVec,
	/// Finality lag
	finality_lag: Gauge,
}

/// Parachain tracer prometheus metrics
#[derive(Default, Clone)]
pub struct Metrics(Option<MetricsInner>);

const HISTOGRAM_TIME_BUCKETS_BLOCKS: &[f64] =
	&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 12.0, 15.0, 25.0, 35.0, 50.0];
const HISTOGRAM_TIME_BUCKETS_SECONDS: &[f64] = &[3.0, 6.0, 12.0, 18.0, 24.0, 30.0, 36.0, 48.0, 60.0, 90.0, 120.0];

impl Metrics {
	pub(crate) fn on_backed(&self, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.backed_count.with_label_values(&[&para_id.to_string()[..]]).inc();
		}
	}

	pub(crate) fn on_block(&self, time: f64, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics
				.relay_block_times
				.with_label_values(&[&para_id.to_string()[..]])
				.observe(time);
			let skipped_slots = ((time / STANDARD_BLOCK_TIME).round() as u64).saturating_sub(1);
			if skipped_slots > 0 {
				metrics
					.relay_skipped_slots
					.with_label_values(&[&para_id.to_string()[..]])
					.inc_by(skipped_slots);
			}
		}
	}

	pub(crate) fn on_slow_availability(&self, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics.slow_avail_count.with_label_values(&[&para_id.to_string()[..]]).inc();
		}
	}

	pub(crate) fn on_bitfields(&self, nbitfields: u32, is_low: bool, para_id: u32) {
		if let Some(metrics) = &self.0 {
			metrics
				.bitfields
				.with_label_values(&[&para_id.to_string()[..]])
				.set(nbitfields as i64);

			if is_low {
				metrics.low_bitfields_count.with_label_values(&[&para_id.to_string()[..]]).inc();
			}
		}
	}

	pub(crate) fn on_skipped_slot(&self, update: &ParachainProgressUpdate) {
		if let Some(metrics) = &self.0 {
			metrics
				.skipped_slots
				.with_label_values(&[&update.para_id.to_string()[..]])
				.inc();
		}
	}

	pub(crate) fn on_disputed(&self, dispute_outcome: &DisputesTracker, para_id: u32) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics.disputes_stats.disputed_count.with_label_values(&[&para_str[..]]).inc();

			if dispute_outcome.voted_for > dispute_outcome.voted_against {
				metrics.disputes_stats.concluded_valid.with_label_values(&[&para_str[..]]).inc();
			} else {
				metrics
					.disputes_stats
					.concluded_invalid
					.with_label_values(&[&para_str[..]])
					.inc();
			}

			if let Some(diff) = dispute_outcome.resolve_time {
				metrics
					.disputes_stats
					.resolution_time
					.with_label_values(&[&para_str[..]])
					.observe(diff as f64);
			}
		}
	}

	/// Update metrics on candidate inclusion
	pub(crate) fn on_included(
		&self,
		relay_parent_number: u32,
		previous_included: Option<u32>,
		backed_in: Option<u32>,
		para_block_time_sec: Option<Duration>,
		para_id: u32,
	) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics.included_count.with_label_values(&[&para_str[..]]).inc();

			if let Some(previous_block_number) = previous_included {
				metrics
					.para_block_times
					.with_label_values(&[&para_str[..]])
					.observe(relay_parent_number.saturating_sub(previous_block_number) as f64);
			}
			if let Some(time) = para_block_time_sec {
				metrics
					.para_block_times_sec
					.with_label_values(&[&para_str[..]])
					.observe(time.as_secs_f64());
			}
			if let Some(backed_in) = backed_in {
				metrics
					.para_backing_times
					.with_label_values(&[&para_str[..]])
					.observe(backed_in as f64);
			}
		}
	}

	/// Update on-demand orders
	pub(crate) fn handle_on_demand_order(&self, order: &OnDemandOrder) {
		if let Some(metrics) = &self.0 {
			let para_str: String = order.para_id.to_string();
			metrics
				.para_on_demand_orders
				.with_label_values(&[&para_str[..]])
				.set(order.spot_price as f64);
		}
	}

	/// Update on-demand latency in blocks
	pub(crate) fn handle_on_demand_delay(&self, delay_blocks: u32, para_id: u32, until: &str) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics
				.para_on_demand_delay
				.with_label_values(&[&para_str[..], until])
				.set(delay_blocks as f64);
		}
	}

	/// Update on-demand latency in seconds
	pub(crate) fn handle_on_demand_delay_sec(&self, delay_sec: Duration, para_id: u32, until: &str) {
		if let Some(metrics) = &self.0 {
			let para_str: String = para_id.to_string();
			metrics
				.para_on_demand_delay_sec
				.with_label_values(&[&para_str[..], until])
				.set(delay_sec.as_secs_f64());
		}
	}

	pub(crate) fn on_finality_lag(&self, lag: u32) {
		if let Some(metrics) = &self.0 {
			metrics.finality_lag.set(lag.into());
		}
	}
}

pub async fn run_prometheus_endpoint(prometheus_opts: &ParachainTracerPrometheusOptions) -> Result<Metrics> {
	let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
	let metrics = register_metrics(&prometheus_registry)?;
	let socket_addr_str = format!("{}:{}", prometheus_opts.address, prometheus_opts.port);
	for addr in socket_addr_str.to_socket_addrs()? {
		let prometheus_registry = prometheus_registry.clone();
		tokio::spawn(prometheus_endpoint::init_prometheus(addr, prometheus_registry));
	}

	Ok(metrics)
}

fn register_metrics(registry: &Registry) -> Result<Metrics> {
	let disputes_stats = DisputesMetrics {
		disputed_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_disputed_count", "Number of disputed candidates"), &["parachain_id"])?,
			registry,
		)?,
		concluded_valid: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_disputed_valid_count", "Number of disputed candidates concluded valid"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		concluded_invalid: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_disputed_invalid_count", "Number of disputed candidates concluded invalid"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		resolution_time: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_disputed_resolve_time", "Dispute resolution time in relay parent blocks")
					.buckets(HISTOGRAM_TIME_BUCKETS_BLOCKS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
	};
	Ok(Metrics(Some(MetricsInner {
		backed_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_backed_count", "Number of backed candidates"), &["parachain_id"])?,
			registry,
		)?,
		skipped_slots: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new(
					"pc_skipped_slots",
					"Number of skipped slots, where no candidate was backed and availability core was free",
				),
				&["parachain_id"],
			)?,
			registry,
		)?,
		included_count: prometheus_endpoint::register(
			IntCounterVec::new(Opts::new("pc_included_count", "Number of candidates included"), &["parachain_id"])?,
			registry,
		)?,
		disputes_stats,
		relay_block_times: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_relay_block_time", "Relay chain block time measured in seconds")
					.buckets(HISTOGRAM_TIME_BUCKETS_SECONDS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
		relay_skipped_slots: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_relay_skipped_slots", "Relay chain block time measured in standard blocks") ,
				&["parachain_id"],
			)?,
			registry,
		)?,
		slow_avail_count: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_slow_available_count", "Number of slow availability events. We consider it slow when the relay chain block bitfield entries amounts to less than 2/3 one bits for the availability core to which the parachain is assigned"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		low_bitfields_count: prometheus_endpoint::register(
			IntCounterVec::new(
				Opts::new("pc_low_bitfields_count", "Number of low bitfields count events. This happens when a block author received the signed bitfields from less than 2/3 of the para validators"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		bitfields: prometheus_endpoint::register(
			IntGaugeVec::new(Opts::new("pc_bitfields_count", "Number of bitfields"), &["parachain_id"]).unwrap(),
			registry,
		)?,
		para_block_times: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_para_block_time", "Parachain block time measured in relay chain blocks.")
					.buckets(HISTOGRAM_TIME_BUCKETS_BLOCKS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
		para_block_times_sec: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_para_block_time_sec", "Parachain block time measured in seconds.")
					.buckets(HISTOGRAM_TIME_BUCKETS_SECONDS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
		para_backing_times: prometheus_endpoint::register(
			HistogramVec::new(
				HistogramOpts::new("pc_para_backing_time", "Parachain backing time measured in relay chain blocks.")
					.buckets(HISTOGRAM_TIME_BUCKETS_BLOCKS.into()),
				&["parachain_id"],
			)?,
			registry,
		)?,
		para_on_demand_orders: prometheus_endpoint::register(
			GaugeVec::new(
				Opts::new("pc_para_on_demand_orders", "Parachain's on demand orders"),
				&["parachain_id"],
			)?,
			registry,
		)?,
		para_on_demand_delay: prometheus_endpoint::register(
			GaugeVec::new(
				Opts::new("pc_para_on_demand_delay", "Latency (in relay chain blocks) between when the parachain orders a core and when first candidate is scheduled or backed on that core."),
				&["parachain_id", "until"],
			)?,
			registry,
		)?,
		para_on_demand_delay_sec: prometheus_endpoint::register(
			GaugeVec::new(
				Opts::new("pc_para_on_demand_delay_sec", "Latency (in seconds) between when the parachain orders a core and when first candidate is scheduled or backed on that core."),
				&["parachain_id"],
			)?,
			registry,
		)?,
		finality_lag: prometheus_endpoint::register(
			Gauge::new("pc_finality_lag", "Finality lag")?,
			registry,
		)?,
	})))
}
