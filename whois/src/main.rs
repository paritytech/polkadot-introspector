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

use clap::{Args, Parser, Subcommand};
use futures::future;
use itertools::Itertools;
use libp2p::{Multiaddr, PeerId};
use polkadot_introspector_essentials::{
	api::{
		api_client::ApiClientMode,
		executor::{RequestExecutor, RequestExecutorError},
	},
	init,
	types::{AccountId32, H256},
	utils,
};
use serde::{Deserialize, Serialize};
use serde_binary::binary_stream::Endian;
use std::{
	collections::{HashMap, HashSet},
	fs::{self, File},
	io::Write,
};
use subp2p_explorer::util::{crypto::sr25519, p2p::get_peer_id};
use subp2p_explorer_cli::commands::authorities::PeerDetails;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Simple telemetry feed")]
struct WhoIsOptions {
	#[clap(subcommand)]
	command: WhoisCommand,
	/// Web-Socket URLs of a relay chain node.
	#[clap(long)]
	pub ws: String,
	/// Name of a chain to connect
	#[clap(long)]
	pub chain: Option<String>,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: utils::RetryOptions,
	/// The session index used for computing the validator indicies.
	#[clap(long)]
	pub session_index: u32,
	/// An optional block hash to fetch the queued keys at, otherwise we use the queued keys at the current block.
	#[clap(long)]
	pub queued_keys_at_block: Option<H256>,
	/// Tells if we should update the p2p cache.
	///
	/// Note building the p2p cache takes around 10 to 15 minutes
	#[clap(long)]
	pub update_p2p_cache: bool,
	/// Hex-encoded genesis hash of the chain.
	///
	/// For example, "781e4046b4e8b5e83d33dde04b32e7cb5d43344b1f19b574f6d31cbbd99fe738"
	#[clap(long, short)]
	genesis: String,
	/// Bootnodes of the chain, must contain a multiaddress together with the peer ID.
	/// For example, "/ip4/127.0.0.1/tcp/30333/ws/p2p/12D3KooWEyoppNCUx8Yx66oV9fJnriXwCcXwDDUA2kj6vnc6iDEp".
	#[clap(long, use_value_delimiter = true, value_parser)]
	bootnodes: Vec<String>,
	/// The number of seconds the discovery process should run for.
	#[clap(long, short, value_parser = parse_duration)]
	timeout: std::time::Duration,
	/// The address format name of the chain.
	/// Used to display the SS58 address of the authorities.
	///
	/// For example:
	/// - "polkadot" for Polkadot
	/// - "substrate" for Substrate
	/// - "kusama" for Kusama
	///
	/// Used to display the SS58 address of the authorities.
	#[clap(long, short)]
	address_format: String,
}

fn parse_duration(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
	let seconds = arg.parse()?;
	Ok(std::time::Duration::from_secs(seconds))
}

#[derive(Clone, Debug, Subcommand)]
enum WhoisCommand {
	/// Display information about a validator by its SS58-formated account address.
	ByAccount(AccountOptions),
	/// Display information about a validator by its index in the para_session.
	ByValidatorIndex(SessionOptions),
	/// Display information about a validator by its authority discovery key.
	ByAuthorityDiscovery(AuthorithyDiscoveryOptions),
	/// Display information about a validator by its peer id.
	ByPeerId(PeerIdOptions),
	/// Display information about all validators.
	DumpAll,
}

#[derive(Clone, Debug, Args)]
struct AccountOptions {
	/// SS58-formated validator's address
	pub validators: Vec<AccountId32>,
}

#[derive(Clone, Debug, Args)]
struct AuthorithyDiscoveryOptions {
	/// Authorithy Discovery in the hex format
	pub authority_discovery: Vec<String>,
}

#[derive(Clone, Debug, Args)]
struct PeerIdOptions {
	/// PeerId of the validator
	pub peer_id: Vec<PeerId>,
}

#[derive(Clone, Debug, Args)]
struct SessionOptions {
	/// Validator index in the para_session
	pub validator_indices: Vec<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum WhoisError {
	#[error("Validator's next keys not found")]
	NoNextKeys,
	#[error("Could not fetch current session index")]
	NoSessionIndex,
	#[error("Keys for the session with given index not found")]
	NoSessionKeys,
	#[error("Validator with given index not found")]
	NoValidator,
	#[error("Can't connect to relay chain")]
	SubxtError(RequestExecutorError),
	#[error("Could not fetch para session account keys")]
	NoParaSessionAccountKeys,
	#[error("Could not fetch session queued keys")]
	NoSessionQueuedKeys,
	#[error("Validator index {0} is invalid in para session account keys")]
	InvalidValidatorIndex(usize),
	#[error("AuthorityDiscovery {0} is invalid in para session account keys")]
	InvalidAuthorityDiscovery(String),
	#[error("Could not find PeerId {0} in the p2p network cache, consider updating the cache if not updated")]
	InvalidPeerId(PeerId),
	#[error("Could not find authority key for peer id {0}")]
	InvalidPeerIdNoAuthority(PeerId),
	#[error("Invalid p2p cache, consider deleting and updating the cache at {0}")]
	InvalidP2PCache(String),
}

struct Whois {
	opts: WhoIsOptions,
}

impl Whois {
	fn new(opts: WhoIsOptions) -> color_eyre::Result<Self> {
		Ok(Self { opts })
	}

	async fn run(
		self,
		mut executor: RequestExecutor,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>, WhoisError> {
		let Ok(session_index_now) = executor.get_session_index_now(&self.opts.ws).await else {
			return Err(WhoisError::NoSessionIndex)
		};

		let para_session_account_keys =
			match executor.get_session_account_keys(&self.opts.ws, self.opts.session_index).await {
				Ok(Some(validators)) => validators,
				Err(e) => return Err(WhoisError::SubxtError(e)),
				_ => return Err(WhoisError::NoParaSessionAccountKeys),
			};

		let session_queued_keys = match executor
			.get_session_queued_keys(&self.opts.ws, self.opts.queued_keys_at_block)
			.await
		{
			Ok(Some(queued_keys)) => queued_keys,
			Err(e) => return Err(WhoisError::SubxtError(e)),
			_ => return Err(WhoisError::NoSessionQueuedKeys),
		};

		let network_cache = NetworkCache::build_cache(session_index_now, &self.opts).await?;
		let network_cache_for_session = network_cache.get_closest_to_session(self.opts.session_index)?;

		// A vector of (validator, validator_index) pairs representing the validator account
		// and its index in para_session_account_keys.
		let accounts_to_discover = match self.opts.command {
			WhoisCommand::ByAccount(v) => v
				.validators
				.into_iter()
				.map(|v| {
					let index = para_session_account_keys.iter().position(|x| &v == x);
					(v, index)
				})
				.collect_vec(),
			WhoisCommand::ByValidatorIndex(v) => v
				.validator_indices
				.into_iter()
				.map(|validator_index| {
					para_session_account_keys
						.get(validator_index)
						.cloned()
						.ok_or(WhoisError::InvalidValidatorIndex(validator_index))
						.map(|account| (account, Some(validator_index)))
				})
				.collect::<Result<Vec<_>, _>>()?,
			WhoisCommand::ByAuthorityDiscovery(authority_discovery) => authority_discovery
				.authority_discovery
				.into_iter()
				.map(|authority_discovery| {
					let account = session_queued_keys
						.iter()
						.find(|(_, session_keys)| {
							format!("0x{}", hex::encode(session_keys.authority_discovery.0 .0)) == authority_discovery
						})
						.ok_or(WhoisError::InvalidAuthorityDiscovery(authority_discovery));
					account.map(|(account, _)| {
						let validator_index = para_session_account_keys.iter().position(|x| account == x);
						(account.clone(), validator_index)
					})
				})
				.collect::<Result<Vec<_>, _>>()?,
			WhoisCommand::ByPeerId(opts) => opts
				.peer_id
				.into_iter()
				.map(|peer_id| {
					let authority_key = network_cache_for_session
						.get_authority_key_from_peer_id(peer_id)
						.ok_or(WhoisError::InvalidPeerId(peer_id));

					authority_key.and_then(|authority_key| {
						let account_for_key = session_queued_keys
							.iter()
							.find(|(_, session_keys)| session_keys.authority_discovery.0 .0 == authority_key)
							.ok_or(WhoisError::InvalidPeerIdNoAuthority(peer_id));

						account_for_key.map(|(account, _)| {
							let validator_index = para_session_account_keys.iter().position(|x| account == x);
							(account.clone(), validator_index)
						})
					})
				})
				.collect::<Result<Vec<_>, _>>()?,
			WhoisCommand::DumpAll => para_session_account_keys
				.into_iter()
				.enumerate()
				.map(|(validator_index, account)| (account, Some(validator_index)))
				.collect_vec(),
		};

		println!(
			"======================================== List of validators ========================================"
		);

		for (validator, validator_index) in accounts_to_discover {
			let session_keys_for_validator = &session_queued_keys.iter().find(|(account, _)| account == &validator);

			if let Some((_, session_keys_for_validator)) = session_keys_for_validator {
				let authorithy_discovery_key = session_keys_for_validator.authority_discovery.0 .0.clone();
				let (peer_details, info, peer_id) = network_cache_for_session.get_details(authorithy_discovery_key);

				println!(
					"validator_index={:?}, account={:}, peer_id={:}, authorithy_id_discover=0x{:}, addresses={:?}, version={:?}",
					validator_index.unwrap_or(usize::MAX),
					validator,
					peer_id,
					hex::encode(authorithy_discovery_key),
					peer_details.map(|details| details.addresses().clone()),
					info,
				);
			} else {
				println!(
					"validator_index={:?}, account={:}, no information could be found",
					validator_index.unwrap_or(usize::MAX),
					validator,
				);
			}
			println!("==============================================================================================");
		}
		executor.close().await;

		Ok(vec![])
	}
}

const DEFAULT_CACHE_DIR: &str = "whois_p2pcache";

// Information about the p2p network at a given session index.
#[derive(Serialize, Deserialize)]
struct PerSessionNetworkCache {
	pub peer_details: HashMap<Vec<u8>, PeerDetails>,
	pub peer_versions: HashMap<Vec<u8>, String>,
	pub authority_to_details: HashMap<sr25519::PublicKey, HashSet<Multiaddr>>,
}

// A cache of the p2p network information for different sessions.
#[derive(Serialize, Deserialize, Default)]
struct NetworkCache {
	session_to_network_info: HashMap<u32, PerSessionNetworkCache>,
}

impl NetworkCache {
	// Build the p2p network cache.
	//
	// Because build the p2p cache takes around 10 to 15 minutes,
	// we only build the cache if the update_p2p_cache flag is set
	// or if the cache does not exist.
	async fn build_cache(session_index_now: u32, opts: &WhoIsOptions) -> color_eyre::Result<Self, WhoisError> {
		let mut update_cache = opts.update_p2p_cache;
		let cache_path = format!("{}/{}", DEFAULT_CACHE_DIR, opts.chain.clone().unwrap_or("any".to_string()));
		println!("Using cache path: {}", cache_path);
		let mut cache: NetworkCache = if let Ok(serialized_cache) = fs::read(cache_path.as_str()) {
			serde_binary::from_vec(serialized_cache, Endian::Big)
				.map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?
		} else {
			update_cache = true;
			Default::default()
		};

		if update_cache {
			println!("Discovering DHT authorithies, this may take a while...");
			let (authorithy_discovery, _) = subp2p_explorer_cli::commands::authorities::discover_authorities(
				opts.ws.clone(),
				opts.genesis.clone(),
				opts.bootnodes.clone(),
				opts.timeout,
				opts.address_format.clone(),
				Default::default(),
			)
			.await
			.map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?;

			let peer_details = authorithy_discovery.peer_details().clone();
			let peer_info = authorithy_discovery.peer_info().clone();
			let authority_to_details = authorithy_discovery.authority_to_details().clone();

			let network_cache = PerSessionNetworkCache {
				peer_details: peer_details.into_iter().map(|(key, value)| (key.to_bytes(), value)).collect(),
				peer_versions: peer_info
					.into_iter()
					.map(|(key, value)| (key.to_bytes(), value.agent_version))
					.collect(),
				authority_to_details,
			};

			cache.session_to_network_info.insert(session_index_now, network_cache);
		}

		let serialized_cache = serde_binary::to_vec(&cache, Endian::Big)
			.map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?;
		let path = std::path::Path::new(cache_path.as_str());
		path.parent().map(|prefix| {
			let _ = std::fs::create_dir_all(prefix).map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()));
		});
		let mut file =
			File::create(cache_path.as_str()).map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?;
		file.write_all(serialized_cache.as_slice())
			.map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?;
		Ok(cache)
	}

	/// Get the p2p network cache closest to the given session index.
	fn get_closest_to_session(&self, session_index: u32) -> color_eyre::Result<&PerSessionNetworkCache, WhoisError> {
		let closest_session_lower = self
			.session_to_network_info
			.keys()
			.filter(|value| *value <= &session_index)
			.max();

		let closest_session_larger = self
			.session_to_network_info
			.keys()
			.filter(|value| *value >= &session_index)
			.min();

		if let Some(closest_session_lower) = closest_session_lower {
			Ok(self
				.session_to_network_info
				.get(closest_session_lower)
				.expect("closest_session_lower is obtained from session_to_network_info; qed"))
		} else if let Some(closest_session_larger) = closest_session_larger {
			Ok(self
				.session_to_network_info
				.get(closest_session_larger)
				.expect("closest_session_larger is obtained from session_to_network_info; qed"))
		} else {
			Err(WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))
		}
	}
}

impl PerSessionNetworkCache {
	fn get_details(&self, authority_discovery_key: sr25519::PublicKey) -> (Option<PeerDetails>, String, PeerId) {
		let Some(details) = self.authority_to_details.get(&authority_discovery_key) else {
			return (Default::default(), Default::default(), PeerId::random())
		};

		let Some(addr) = details.iter().next() else {
			return (Default::default(), Default::default(), PeerId::random())
		};

		let peer_id = get_peer_id(addr).unwrap_or(PeerId::random());
		let serialized_key = peer_id.to_bytes();
		(
			self.peer_details.get(&serialized_key).cloned(),
			self.peer_versions
				.get(&serialized_key)
				.cloned()
				.unwrap_or("unknown".to_string()),
			peer_id,
		)
	}

	fn get_authority_key_from_peer_id(&self, peer_id: PeerId) -> Option<sr25519::PublicKey> {
		let serialized_key = peer_id.to_bytes();
		self.peer_details.get(&serialized_key).map(|x| x.authority_id()).cloned()
	}
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
	let opts = WhoIsOptions::parse();
	init::init_cli(&opts.verbose)?;

	let whois = Whois::new(opts.clone())?;
	let shutdown_tx = init::init_shutdown();
	let executor = RequestExecutor::build(opts.ws.clone(), ApiClientMode::RPC, &opts.retry, &shutdown_tx).await?;

	let mut futures = vec![];
	futures.extend(whois.run(executor).await?);

	future::try_join_all(futures).await?;

	Ok(())
}
