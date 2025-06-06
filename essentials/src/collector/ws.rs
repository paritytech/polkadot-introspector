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
use crate::{
	chain_events::SubxtDisputeResult,
	collector::{CollectorPrefixType, CollectorStorageApi, candidate_record::CandidateRecord},
	types::{H256, Timestamp},
};
use futures::{SinkExt, StreamExt};
use log::{debug, warn};
use polkadot_introspector_priority_channel::Receiver;
use serde::{Deserialize, Serialize};
use std::{
	convert::Infallible,
	error::Error,
	fmt::Debug,
	fs,
	marker::Send,
	net::SocketAddr,
	path::PathBuf,
	str::FromStr,
	time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast::Receiver as BroadcastReceiver;
use typed_builder::TypedBuilder;
use warp::{
	Filter, Rejection, Reply,
	http::StatusCode,
	ws::{Message, WebSocket},
};

/// Structure for a Web-Socket builder
#[derive(TypedBuilder, Clone, Debug)]
pub struct WebSocketListenerConfig {
	/// Address to listen on
	listen_addr: SocketAddr,
	/// Private key for SSL HTTP server
	#[builder(default)]
	privkey: Option<PathBuf>,
	/// SSL certificate for HTTP server
	#[builder(default)]
	cert: Option<PathBuf>,
}

/// Starts a Web-Socket listener given the config
pub struct WebSocketListener {
	/// Configuration for a listener
	config: WebSocketListenerConfig,
	/// Storage to access
	api: CollectorStorageApi,
}

/// Defines Web-Socket event types
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
	not_before: Option<Timestamp>,
}

/// Used to handle requests to get a specific candidate info
#[derive(Deserialize, Serialize)]
struct CandidateGetQuery {
	/// Candidate hash
	hash: String,
}

/// Used to handle requests with a health query
#[derive(Deserialize, Serialize)]
struct HealthQuery {
	/// Ping like field (optional)
	ts: Timestamp,
}

/// Common functions for a listener
impl WebSocketListener {
	/// Creates a new socket listener with the specific config
	pub(crate) fn new(config: WebSocketListenerConfig, api: CollectorStorageApi) -> Self {
		Self { config, api }
	}

	/// Spawn an async HTTP server
	pub(crate) async fn spawn<Shutdown, Update>(
		&self,
		mut shutdown_rx: BroadcastReceiver<Shutdown>,
		updates_broadcast: Receiver<Update>,
	) -> Result<(), Box<dyn Error>>
	where
		Shutdown: Send + Sync + 'static + Clone,
		Update: Send + Sync + 'static + Clone + Serialize + Debug,
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
			let privkey = fs::read(self.config.privkey.as_ref().unwrap()).expect("cannot read privkey file");
			let cert = fs::read(self.config.cert.as_ref().unwrap()).expect("cannot read privkey file");
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
	api: CollectorStorageApi,
) -> impl Filter<Extract = (CollectorStorageApi,), Error = Infallible> + Clone {
	warp::any().map(move || api.clone())
}

fn with_updates_channel<T: Send>(
	updates_rx: Receiver<T>,
) -> impl Filter<Extract = (Receiver<T>,), Error = Infallible> + Clone {
	warp::any().map(move || updates_rx.clone())
}

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
pub struct HealthReply {
	/// How many candidates have we processed
	pub candidates_stored: usize,
	/// Timestamp from a request or our local timestamp
	pub ts: Timestamp,
}

async fn health_handler(api: CollectorStorageApi, ping: Option<HealthQuery>) -> Result<impl Reply, Rejection> {
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
	api: CollectorStorageApi,
	filter: Option<CandidatesQuery>,
) -> Result<impl Reply, Rejection> {
	let keys = if let Some(para_id) = filter.and_then(|filt| filt.parachain_id) {
		api.storage().storage_keys_prefix(CollectorPrefixType::Candidate(para_id)).await
	} else {
		let mut output: Vec<H256> = vec![];
		for prefix in api.storage().storage_prefixes().await {
			if let CollectorPrefixType::Candidate(_) = &prefix {
				output.extend(api.storage().storage_keys_prefix(prefix).await);
			}
		}

		output
	};

	Ok(warp::reply::json(&keys.into_iter().collect::<Vec<_>>().as_slice()))
}

async fn candidate_get_handler(
	api: CollectorStorageApi,
	candidate_hash: CandidateGetQuery,
) -> Result<impl Reply, Rejection> {
	let decoded_hash = H256::from_str(candidate_hash.hash.as_str()).map_err(|_| warp::reject::reject())?;
	let candidate_record = api.storage().storage_read(decoded_hash).await;

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

async fn handle_ws_connection<T>(ws: WebSocket, update_channel: Receiver<T>, remote: Option<SocketAddr>)
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
