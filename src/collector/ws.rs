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

use super::{event_handler::StorageType, records_storage::StorageEntry};

use log::warn;
use serde::{Deserialize, Serialize};
use sp_core::H256;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{convert::Infallible, error::Error, fs, marker::Send, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::oneshot::Receiver;
use typed_builder::TypedBuilder;
use warp::{http::StatusCode, Filter, Rejection, Reply};

/// Structure for a WebSocket builder
#[derive(TypedBuilder, Clone, Debug)]
pub struct WebSocketListenerConfig {
	/// Address to listen on
	listen_addr: SocketAddr,
	/// Private key for SSL HTTP server
	#[builder(default)]
	privkey: Option<PathBuf>,
	/// X509 certificate for HTTP server
	#[builder(default)]
	cert: Option<PathBuf>,
}

/// Starts a ws listener given the config
pub struct WebSocketListener {
	/// Configuration for a listener
	config: WebSocketListenerConfig,
	/// Storage to access
	storage: Arc<StorageType<H256>>,
}

/// Used to handle requests to obtain candidates
#[derive(Deserialize, Serialize)]
struct CandidatesQuery {
	/// Filter candidates by parachain
	parachain_id: Option<u32>,
	/// Filter candidates by time
	not_before: Option<u64>,
}

/// Used to handle requests to get a specific candidate info
#[derive(Deserialize, Serialize)]
struct CandidateGetQuery {
	/// Candidate hash
	hash: H256,
}

/// Used to handle requests with a health query
#[derive(Deserialize, Serialize)]
struct HealthQuery {
	/// Ping like field (optional)
	ts: u64,
}

/// Common functions for a listener
impl WebSocketListener {
	/// Creates a new socket listener with the specific config
	pub fn new(config: WebSocketListenerConfig, storage: Arc<StorageType<H256>>) -> Self {
		Self { config, storage }
	}

	/// Spawn an async HTTP server
	pub async fn spawn<T>(self, shutdown_recv: Receiver<T>) -> Result<(), Box<dyn Error + Sync + Send>>
	where
		T: Send + 'static,
	{
		let has_sane_tls = self.config.privkey.is_some() && self.config.cert.is_some();

		// Setup routes
		let opt_ping = warp::query::<HealthQuery>()
			.map(Some)
			.or_else(|_| async { Ok::<(Option<HealthQuery>,), std::convert::Infallible>((None,)) });
		let health_route = warp::path!("v1" / "health")
			.and(with_storage(self.storage.clone()))
			.and(opt_ping)
			.and_then(health_handler);

		let opt_candidates = warp::query::<CandidatesQuery>()
			.map(Some)
			.or_else(|_| async { Ok::<(Option<CandidatesQuery>,), std::convert::Infallible>((None,)) });
		let candidates_route = warp::path!("v1" / "candidates")
			.and(with_storage(self.storage.clone()))
			.and(opt_candidates)
			.and_then(candidates_handler);

		let get_candidate_route = warp::path!("v1" / "candidate")
			.and(with_storage(self.storage.clone()))
			.and(warp::query::<CandidateGetQuery>())
			.and_then(candidate_get_handler);

		let routes = health_route
			.or(candidates_route)
			.or(get_candidate_route)
			.with(warp::cors().allow_any_origin())
			.recover(handle_rejection);
		let server = warp::serve(routes);

		if has_sane_tls {
			let privkey = fs::read(self.config.privkey.unwrap()).expect("cannot read privkey file");
			let cert = fs::read(self.config.cert.unwrap()).expect("cannot read privkey file");
			let tls_server = server.tls().cert(cert).key(privkey);
			// TODO: understand why there is no `try_bind_with_graceful_shutdown` for TLSServer in Warp
			let (_, server_fut) = tls_server.bind_with_graceful_shutdown(self.config.listen_addr, async {
				shutdown_recv.await.ok();
			});

			tokio::task::spawn(server_fut);
		} else {
			let (_, server_fut) = server.try_bind_with_graceful_shutdown(self.config.listen_addr, async {
				shutdown_recv.await.ok();
			})?;

			tokio::task::spawn(server_fut);
		}

		Ok(())
	}
}

fn with_storage(
	storage: Arc<StorageType<H256>>,
) -> impl Filter<Extract = (Arc<StorageType<H256>>,), Error = Infallible> + Clone {
	warp::any().map(move || storage.clone())
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct HealthReply {
	/// How many candidates have we processed
	pub candidates_stored: usize,
	/// Timestamp from a request or our local timestamp
	pub ts: u64,
}

async fn health_handler(storage: Arc<StorageType<H256>>, ping: Option<HealthQuery>) -> Result<impl Reply, Rejection> {
	let storage_locked = storage.lock().unwrap();
	let ts = match ping {
		Some(h) => h.ts,
		None => SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
	};
	Ok(warp::reply::json(&HealthReply { candidates_stored: storage_locked.len(), ts }))
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct CandidatesReply {
	/// How many candidates have we processed
	pub candidates: Vec<H256>,
}

async fn candidates_handler(
	storage: Arc<StorageType<H256>>,
	filter: Option<CandidatesQuery>,
) -> Result<impl Reply, Rejection> {
	let storage_locked = storage.lock().unwrap();
	let records = storage_locked.records();
	let candidates = if let Some(filter_query) = filter {
		records
			.iter()
			.filter(|(_, value)| {
				if let Some(parachain_id) = filter_query.parachain_id {
					parachain_id == value.parachain_id().unwrap_or(0)
				} else {
					true
				}
			})
			.filter(|(_, value)| {
				if let Some(not_before) = filter_query.not_before {
					not_before <= value.get_time().as_secs()
				} else {
					true
				}
			})
			.map(|(key, _)| key)
			.cloned()
			.collect::<Vec<_>>()
	} else {
		records.keys().cloned().collect::<Vec<_>>()
	};

	Ok(warp::reply::json(&CandidatesReply { candidates }))
}

async fn candidate_get_handler(
	storage: Arc<StorageType<H256>>,
	candidate_hash: CandidateGetQuery,
) -> Result<impl Reply, Rejection> {
	let storage_locked = storage.lock().unwrap();
	let candidate_record = storage_locked.get(&candidate_hash.hash);

	match candidate_record {
		Some(rec) => Ok(warp::reply::json(rec).into_response()),
		None => Ok(warp::reply::with_status("No such candidate", StatusCode::NOT_FOUND).into_response()),
	}
}

async fn handle_rejection(err: Rejection) -> std::result::Result<impl Reply, Infallible> {
	let (code, message) = if err.is_not_found() {
		(StatusCode::NOT_FOUND, "Not Found")
	} else if err.find::<warp::filters::body::BodyDeserializeError>().is_some() {
		(StatusCode::BAD_REQUEST, "Invalid Body")
	} else if err.find::<warp::reject::MethodNotAllowed>().is_some() {
		(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
	} else {
		warn!("unhandled error: {:?}", err);
		(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
	};

	Ok(warp::reply::with_status(message, code))
}
