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

use crate::kvdb::IntrospectorKvdb;
use clap::Parser;
use color_eyre::Result;
use log::{error, info};
use prometheus_endpoint::{prometheus::IntGaugeVec, Opts, Registry};

#[derive(Clone, Debug, Parser, Default)]
#[clap(rename_all = "kebab-case")]
pub struct KvdbPrometheusOptions {
	/// Prometheus endpoint port.
	#[clap(long, default_value = "65432")]
	port: u16,
	/// Database poll timeout (default, once per 5 minutes).
	#[clap(long, default_value = "300.0")]
	poll_timeout: f32,
}

struct KvdbPrometheusMetrics {
	keys_size_gauge: IntGaugeVec,
	values_size_gauge: IntGaugeVec,
	elements_count_gauge: IntGaugeVec,
}

pub async fn run_prometheus_endpoint_with_db<D: IntrospectorKvdb + Send + Sync + 'static>(
	db: D,
	prometheus_opts: KvdbPrometheusOptions,
) -> Result<Vec<tokio::task::JoinHandle<()>>> {
	let prometheus_registry = Registry::new_custom(Some("introspector".into()), None)?;
	let metrics = register_metrics(&prometheus_registry);
	let socket_addr =
		std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(0, 0, 0, 0)), prometheus_opts.port);
	let mut futures: Vec<tokio::task::JoinHandle<()>> = vec![];
	futures.push(tokio::spawn(async move {
		prometheus_endpoint::init_prometheus(socket_addr, prometheus_registry)
			.await
			.unwrap()
	}));
	futures.push(tokio::spawn(async move {
		update_db(db, metrics, prometheus_opts).await;
	}));
	info!("Starting prometheus node on {:?}", socket_addr);

	Ok(futures)
}

struct UpdateResult {
	column: String,
	keys_count: i64,
	keys_space: i64,
	values_space: i64,
}

async fn update_db<D: IntrospectorKvdb>(
	mut db: D,
	metrics: KvdbPrometheusMetrics,
	prometheus_opts: KvdbPrometheusOptions,
) {
	loop {
		info!("Starting update db iteration");
		match db.list_columns() {
			Ok(columns) => {
				let mut update_results: Vec<UpdateResult> = vec![];

				let all_done = columns.iter().all(|col| {
					let mut keys_space = 0_i64;
					let mut keys_count = 0_i64;
					let mut values_space = 0_i64;

					info!("Iterating over column {}", col.as_str());
					match db.iter_values(col.as_str()) {
						Ok(iter) =>
							for (key, value) in iter {
								keys_space += key.len() as i64;
								keys_count += 1;
								values_space += value.len() as i64;
							},
						Err(e) => {
							error!(
								"Failed to get iterator for column {} in database: {:?}, trying to reopen database",
								col.as_str(),
								e
							);
							return false
						},
					}

					update_results.push(UpdateResult { column: col.clone(), keys_count, keys_space, values_space });
					true
				});

				if all_done {
					for res in &update_results {
						metrics
							.elements_count_gauge
							.with_label_values(&[res.column.as_str()])
							.set(res.keys_count);
						metrics
							.keys_size_gauge
							.with_label_values(&[res.column.as_str()])
							.set(res.keys_space);
						metrics
							.values_size_gauge
							.with_label_values(&[res.column.as_str()])
							.set(res.values_space);
					}
					info!("Ended update db iteration");
				} else {
					db = maybe_reopen_db(db);
				}
			},
			Err(e) => {
				error!("Failed to list columns in database: {:?}, trying to reopen database", e);
				db = maybe_reopen_db(db);
			},
		}
		tokio::time::sleep(std::time::Duration::from_secs_f32(prometheus_opts.poll_timeout)).await;
	}
}

fn register_metrics(registry: &Registry) -> KvdbPrometheusMetrics {
	let elements_count_gauge = prometheus_endpoint::register(
		IntGaugeVec::new(Opts::new("kvdb_elements_count", "Number of keys in kvdb"), &["column"]).unwrap(),
		registry,
	)
	.expect("Failed to register metric");
	let keys_size_gauge = prometheus_endpoint::register(
		IntGaugeVec::new(Opts::new("kvdb_keys_size", "Size of keys in KVDB"), &["column"]).unwrap(),
		registry,
	)
	.expect("Failed to register metric");
	let values_size_gauge = prometheus_endpoint::register(
		IntGaugeVec::new(Opts::new("kvdb_values_size", "Size of keys in KVDB"), &["column"]).unwrap(),
		registry,
	)
	.expect("Failed to register metric");

	KvdbPrometheusMetrics { keys_size_gauge, values_size_gauge, elements_count_gauge }
}

fn maybe_reopen_db<D: IntrospectorKvdb>(db: D) -> D {
	match D::new(db.get_db_path()) {
		Ok(new_db) => new_db,
		Err(e) => {
			error!("Cannot reopen database at {:?}: {:?}", db.get_db_path(), e);
			db
		},
	}
}
