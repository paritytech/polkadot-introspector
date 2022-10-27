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
use crate::{
	collector::{candidate_record::CandidateRecord, CollectorKey, CANDIDATE_PREFIX},
	core::{api::ApiService, SubxtDisputeResult},
};
use futures::{SinkExt, StreamExt};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::{
	convert::Infallible,
	error::Error,
	fmt::Debug,
	fs,
	marker::Send,
	net::SocketAddr,
	path::PathBuf,
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use subxt::sp_core::H256;
use tokio::sync::broadcast::{Receiver, Sender};
use typed_builder::TypedBuilder;
use warp::{
	http::StatusCode,
	ws::{Message, WebSocket},
	Filter, Rejection, Reply,
};

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
	api: ApiService<CollectorKey>,
}

/// Defines WebSocket event types
#[derive(Clone, Copy, Serialize, Debug)]
pub(crate) enum WebSocketEventType {
	Backed,
	DisputeInitiated(H256),
	DisputeConcluded(H256, SubxtDisputeResult),
	Included(Duration),
	TimedOut(Duration),
}

/// Handles websocket updates
#[derive(Clone, Serialize, Debug)]
pub(crate) struct WebSocketUpdateEvent {
	/// Candidate hash
	pub candidate_hash: H256,
	/// Parachain ID
	pub parachain_id: u32,
	/// Timestamp for the event
	pub ts: Duration,
	/// The real event
	pub event: WebSocketEventType,
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
	pub(crate) fn new(config: WebSocketListenerConfig, api: ApiService<CollectorKey>) -> Self {
		Self { config, api }
	}

	/// Spawn an async HTTP server
	pub(crate) async fn spawn<T, U>(
		self,
		mut shutdown_rx: Receiver<T>,
		updates_broadcast: Sender<U>,
	) -> Result<(), Box<dyn Error + Sync + Send>>
	where
		T: Send + Sync + 'static + Clone,
		U: Send + Sync + 'static + Clone + Serialize + Debug,
	{
		let has_sane_tls = self.config.privkey.is_some() && self.config.cert.is_some();

		// Setup routes
		let opt_ping = warp::query::<HealthQuery>()
			.map(Some)
			.or_else(|_| async { Ok::<(Option<HealthQuery>,), std::convert::Infallible>((None,)) });
		let health_route = warp::path!("v1" / "health")
			.and(with_api_service(self.api.clone()))
			.and(opt_ping)
			.and_then(health_handler);

		let opt_candidates = warp::query::<CandidatesQuery>()
			.map(Some)
			.or_else(|_| async { Ok::<(Option<CandidatesQuery>,), std::convert::Infallible>((None,)) });
		let candidates_route = warp::path!("v1" / "candidates")
			.and(with_api_service(self.api.clone()))
			.and(opt_candidates)
			.and_then(candidates_handler);

		let get_candidate_route = warp::path!("v1" / "candidate")
			.and(with_api_service(self.api.clone()))
			.and(warp::query::<CandidateGetQuery>())
			.and_then(candidate_get_handler);
		let ws_route = warp::path!("v1" / "ws")
			.and(warp::ws())
			.and(with_updates_channel(updates_broadcast))
			.and(warp::addr::remote())
			.and_then(ws_handler);
		let routes = health_route
			.or(candidates_route)
			.or(get_candidate_route)
			.or(ws_route)
			.with(warp::cors().allow_any_origin())
			.recover(handle_rejection);
		let server = warp::serve(routes);

		if has_sane_tls {
			let privkey = fs::read(self.config.privkey.unwrap()).expect("cannot read privkey file");
			let cert = fs::read(self.config.cert.unwrap()).expect("cannot read privkey file");
			let tls_server = server.tls().cert(cert).key(privkey);
			// TODO: understand why there is no `try_bind_with_graceful_shutdown` for TLSServer in Warp
			let (_, server_fut) = tls_server.bind_with_graceful_shutdown(self.config.listen_addr, async move {
				shutdown_rx.recv().await.ok();
			});

			tokio::task::spawn(server_fut);
		} else {
			let (_, server_fut) = server.try_bind_with_graceful_shutdown(self.config.listen_addr, async move {
				shutdown_rx.recv().await.ok();
			})?;

			tokio::task::spawn(server_fut);
		}

		Ok(())
	}
}

// Helper to share storage state
fn with_api_service(
	api: ApiService<CollectorKey>,
) -> impl Filter<Extract = (ApiService<CollectorKey>,), Error = Infallible> + Clone {
	warp::any().map(move || api.clone())
}

fn with_updates_channel<T: Send>(
	updates_rx: Sender<T>,
) -> impl Filter<Extract = (Receiver<T>,), Error = Infallible> + Clone {
	warp::any().map(move || updates_rx.subscribe())
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct HealthReply {
	/// How many candidates have we processed
	pub candidates_stored: usize,
	/// Timestamp from a request or our local timestamp
	pub ts: u64,
}

async fn health_handler(api: ApiService<CollectorKey>, ping: Option<HealthQuery>) -> Result<impl Reply, Rejection> {
	let storage_size = api.storage().storage_len().await;
	let ts = match ping {
		Some(h) => h.ts,
		None => SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
	};
	Ok(warp::reply::json(&HealthReply { candidates_stored: storage_size, ts }))
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct CandidatesReply {
	/// How many candidates have we processed
	pub candidates: Vec<H256>,
}

async fn candidates_handler(
	api: ApiService<CollectorKey>,
	_filter: Option<CandidatesQuery>,
) -> Result<impl Reply, Rejection> {
	let keys = api
		.storage()
		.storage_keys(Some(CollectorKey { prefix: CANDIDATE_PREFIX.into(), hash: None }))
		.await;
	// TODO: add filters support somehow...

	Ok(warp::reply::json(
		&keys
			.into_iter()
			.filter_map(|collector_key| collector_key.hash)
			.collect::<Vec<_>>()
			.as_slice(),
	))
}

async fn candidate_get_handler(
	api: ApiService<CollectorKey>,
	candidate_hash: CandidateGetQuery,
) -> Result<impl Reply, Rejection> {
	let candidate_record = api
		.storage()
		.storage_read(CollectorKey { prefix: CANDIDATE_PREFIX.into(), hash: Some(candidate_hash.hash) })
		.await;

	match candidate_record {
		Some(rec) => {
			let rec: CandidateRecord = rec.into_inner().map_err(|_| warp::reject::reject())?;
			Ok(warp::reply::json(&rec).into_response())
		},
		None => Ok(warp::reply::with_status("No such candidate", StatusCode::NOT_FOUND).into_response()),
	}
}

pub async fn ws_handler<T>(
	ws: warp::ws::Ws,
	update_channel: Receiver<T>,
	remote: Option<SocketAddr>,
) -> Result<impl Reply, Rejection>
where
	T: Send + Sync + Clone + Debug + Serialize + 'static,
{
	Ok(ws.on_upgrade(move |socket| handle_ws_connection(socket, update_channel, remote)))
}

async fn handle_ws_connection<T>(ws: WebSocket, mut update_channel: Receiver<T>, remote: Option<SocketAddr>)
where
	T: Send + Sync + Clone + Debug + Serialize + 'static,
{
	let (mut client_ws_sender, _client_ws_rcv) = ws.split();
	debug!("connected to ws: {:?}", remote.as_ref());

	tokio::task::spawn(async move {
		loop {
			match update_channel.recv().await {
				Ok(update) => {
					debug!("received event: {:?}", &update);

					match client_ws_sender
						.send(Message::text(serde_json::to_string(&update).unwrap()))
						.await
					{
						Ok(_) => {
							debug!("{:?} sent update to ws client", remote.as_ref());
						},
						Err(err) => {
							warn!("{:?} cannot send data: {:?}", remote.as_ref(), err);
							return
						},
					}
				},
				Err(err) => {
					warn!("{:?} update channel error = {:?}", remote.as_ref(), err);

					return
				},
			}
		}
	});
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
