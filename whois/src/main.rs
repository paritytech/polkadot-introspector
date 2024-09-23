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

use chrono::{DateTime, Utc};
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
	metadata::{
		polkadot::session::events::new_session::SessionIndex,
		polkadot_primitives::{AvailabilityBitfield, ValidatorIndex},
	},
	types::{AccountId32, SessionKeys, H256},
	utils,
};
use serde::{Deserialize, Serialize};
use serde_binary::binary_stream::Endian;
use ss58_registry::Ss58AddressFormat;
use std::{
	collections::{BTreeMap, HashMap, HashSet},
	fs::{self, File},
	io::Write,
	time::{Duration, UNIX_EPOCH},
};
use subp2p_explorer::util::{crypto::sr25519, p2p::get_peer_id};
use subp2p_explorer_cli::commands::authorities::PeerDetails;

#[derive(Clone, Debug, Parser)]
#[clap(author, version, about = "Simple command to query polkadot validator identities")]
struct WhoIsOptions {
	#[clap(subcommand)]
	command: WhoisCommand,
	/// Web-Socket URLs of a relay chain node.
	#[clap(long)]
	pub ws: String,
	#[clap(flatten)]
	pub verbose: init::VerbosityOptions,
	#[clap(flatten)]
	pub retry: utils::RetryOptions,
	/// The session index used for computing the validator indices.
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
	/// Bootnodes of the chain, must contain a multiaddress together with the peer ID.
	/// For example, "/ip4/127.0.0.1/tcp/30333/ws/p2p/12D3KooWEyoppNCUx8Yx66oV9fJnriXwCcXwDDUA2kj6vnc6iDEp".
	#[clap(long, use_value_delimiter = true, value_parser)]
	bootnodes: Vec<String>,
	/// The number of seconds the discovery process should run for, default value
	/// is empirically values found for the polkadot and kusama network, smaller
	/// networks may require a smaller values.
	#[clap(long, short, value_parser = parse_duration, default_value = "900")]
	timeout: std::time::Duration,
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
	/// Display performance statistics for bitfields for each validator.
	BitFieldsPerformance(BitFieldsPerformance),
}
#[derive(Copy, Clone, Debug, Args)]
struct BitFieldsPerformance {
	/// The block number to start from.
	start_block: u32,
	/// The step to go back in blocks.
	num_to_skip: u32,
	/// The number of blocks to check.
	num_blocks: u32,
	/// Print only validators that have at least this number of sessions with poor performance.
	num_sessions_bellow_threshold: u32,
	/// The threshold for poor performance.
	poor_performance_threshold: f64,
}

#[derive(Clone, Debug, Eq, PartialEq, Default)]
struct BitfieldsStats {
	// Number of blocks with poor performance.
	num_poor_performance: usize,
	// Number of blocks with the bitfield of the validator being present.
	num_present: usize,
}

impl BitfieldsStats {
	fn percentage_poor(&self) -> f64 {
		(self.num_poor_performance as f64 * 100.0) / self.num_present as f64
	}
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
	#[error("Could not determine the genesis hash")]
	NoGenesisHash,
	#[error("Could not determine the chain name")]
	NoChainName,
	#[error("Invalid session index, session needs to be between {0} and {1}")]
	InvalidSessionIndex(u32, u32),
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

// The maximum number of para sessions a node stores before starting to proun
const NUMBER_OF_STORED_SESSIONS: u32 = 6;

impl Whois {
	fn new(opts: WhoIsOptions) -> color_eyre::Result<Self> {
		Ok(Self { opts })
	}

	async fn run(
		self,
		mut executor: RequestExecutor,
	) -> color_eyre::Result<Vec<tokio::task::JoinHandle<()>>, WhoisError> {
		let Ok(Some(genesis_hash)) = executor.get_block_hash(&self.opts.ws, Some(0)).await else {
			return Err(WhoisError::NoGenesisHash)
		};

		let Ok(chain_name) = executor.get_chain_name(&self.opts.ws).await else { return Err(WhoisError::NoChainName) };

		println!("Using chain name: {} genesis_hash: {:?}", chain_name, genesis_hash);

		let Ok(session_index_now) = executor.get_session_index_now(&self.opts.ws).await else {
			return Err(WhoisError::NoSessionIndex)
		};

		if session_index_now < self.opts.session_index ||
			session_index_now - self.opts.session_index > NUMBER_OF_STORED_SESSIONS
		{
			return Err(WhoisError::InvalidSessionIndex(
				session_index_now - NUMBER_OF_STORED_SESSIONS,
				session_index_now,
			));
		}

		let para_session_account_keys = match executor
			.get_session_account_keys(&self.opts.ws, self.opts.session_index, None)
			.await
		{
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

		let network_cache =
			NetworkCache::build_cache(session_index_now, genesis_hash, chain_name.as_str(), &self.opts).await?;
		let network_cache_for_session = network_cache.get_closest_to_session(self.opts.session_index)?;

		let run_sanity_check = matches!(self.opts.command, WhoisCommand::DumpAll);

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
							format!("0x{}", hex::encode(session_keys.authority_discovery.0)) == authority_discovery
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
							.find(|(_, session_keys)| session_keys.authority_discovery.0 == authority_key)
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
			WhoisCommand::BitFieldsPerformance(opts) => {
				self.run_bitfields_performance_analysis(
					opts,
					&mut executor,
					session_queued_keys,
					network_cache_for_session,
				)
				.await;
				executor.close().await;
				return Ok(vec![]);
			},
		};

		let mut current_authority_discovery_keys = HashSet::new();
		for (validator, validator_index) in accounts_to_discover {
			let session_keys_for_validator = &session_queued_keys.iter().find(|(account, _)| account == &validator);

			if let Some((_, session_keys_for_validator)) = session_keys_for_validator {
				let authority_discovery_key = session_keys_for_validator.authority_discovery.0;
				let (peer_details, info, peer_id) = network_cache_for_session.get_details(authority_discovery_key);
				current_authority_discovery_keys.insert(authority_discovery_key);
				println!(
					"validator_index={:?}, account={:}, peer_id={:}, authority_id_discover=0x{:}, addresses={:?}, version={:?}",
					validator_index.unwrap_or(usize::MAX),
					validator,
					peer_id.map(|peer_id| peer_id.to_string()).unwrap_or("unknown".to_string()),
					hex::encode(authority_discovery_key),
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
			println!("");
		}

		if run_sanity_check {
			network_cache_for_session.sanity_check(current_authority_discovery_keys);
		}

		executor.close().await;

		Ok(vec![])
	}

	async fn run_bitfields_performance_analysis(
		&self,
		opts: BitFieldsPerformance,
		executor: &mut RequestExecutor,
		session_queued_keys: Vec<(AccountId32, SessionKeys)>,
		network_cache_for_session: &PerSessionNetworkCache,
	) {
		println!("Running bitfields performance analysis");
		let mut per_session_by_validatory_stats: BTreeMap<SessionIndex, BTreeMap<u32, BitfieldsStats>> =
			BTreeMap::new();
		let mut accounts_by_session = BTreeMap::new();
		let mut per_account_by_session_stats: BTreeMap<AccountId32, BTreeMap<SessionIndex, BitfieldsStats>> =
			BTreeMap::new();
		let mut session_timestamps = BTreeMap::new();
		let mut start = opts.start_block;
		let mut num_blocks_poor_perf_more_than_a_third = BTreeMap::new();

		for _ in 0..opts.num_blocks {
			let Ok(Some(block_hash)) = executor.get_block_hash(&self.opts.ws, Some(start)).await else {
				break;
			};

			let Ok(session_index_now) = executor.get_session_index(&self.opts.ws, block_hash).await else {
				break;
			};

			if !accounts_by_session.contains_key(&session_index_now) {
				let Ok(timestamp) = executor.get_block_timestamp(&self.opts.ws, block_hash).await else {
					break;
				};

				let timestamp_date = UNIX_EPOCH + Duration::from_millis(timestamp);
				let timestamp_date: DateTime<Utc> = DateTime::from(timestamp_date);
				let timestamp_str = timestamp_date.format("%Y-%m-%d %H:%M").to_string();
				session_timestamps.insert(session_index_now, timestamp_str.clone());
				println!("session: {} timestamp: {}", session_index_now, timestamp_str);
				let para_session_account_keys = match executor
					.get_session_account_keys(&self.opts.ws, session_index_now, Some(block_hash))
					.await
				{
					Ok(Some(validators)) => validators,
					Err(_e) => break,
					_ => break,
				};

				let para_session_account_keys = para_session_account_keys
					.into_iter()
					.enumerate()
					.map(|(validator_index, account)| (account, validator_index))
					.collect_vec();
				accounts_by_session.insert(session_index_now, para_session_account_keys);
			}

			let Ok(para_inherent) = executor.extract_parainherent_data(&self.opts.ws, Some(block_hash)).await else {
				break;
			};

			let bitfields = para_inherent
				.bitfields
				.into_iter()
				.map(|b| (b.payload, b.validator_index))
				.collect::<Vec<(AvailabilityBitfield, ValidatorIndex)>>();

			let mut num_poor_performance_per_block = 0;

			let Some(max) = bitfields
				.iter()
				.map(|(bitfield, _)| bitfield.0.as_bits().iter().filter(|bit| *bit).count())
				.max()
			else {
				break;
			};

			let Some(session_accounts) = accounts_by_session.get(&session_index_now) else {
				break;
			};
			let session_stats = per_session_by_validatory_stats.entry(session_index_now).or_default();

			for bitfield in bitfields {
				let num_bits_set = bitfield.0 .0.as_bits().iter().filter(|bit| *bit).count();

				let Some(validator) = session_accounts
					.iter()
					.find(|(_, validator)| *validator as u32 == bitfield.1 .0)
					.map(|val| val.0.clone())
				else {
					break;
				};

				let per_account_stats = per_account_by_session_stats
					.entry(validator)
					.or_default()
					.entry(session_index_now)
					.or_default();

				let per_session_stats = session_stats.entry(bitfield.1 .0).or_insert(Default::default());

				// The validator has poor performance if it has less than 2 bits set.
				if num_bits_set < std::cmp::min(max, 2) {
					num_poor_performance_per_block += 1;
					(*per_session_stats).num_poor_performance += 1;
					per_account_stats.num_poor_performance += 1;
				}
				(*per_session_stats).num_present += 1;
				per_account_stats.num_present += 1;
			}

			if num_poor_performance_per_block > session_accounts.len() / 3 {
				*(num_blocks_poor_perf_more_than_a_third.entry(session_index_now).or_insert(0)) += 1;
			}
			start -= opts.num_to_skip;
		}

		print_per_session_performance(opts, per_session_by_validatory_stats, &session_timestamps);

		print_per_account_performance(
			opts,
			session_queued_keys,
			network_cache_for_session,
			per_account_by_session_stats,
			&session_timestamps,
		);

		for (session, more_than_a_third) in num_blocks_poor_perf_more_than_a_third.iter() {
			println!(
				"block_start: {}, session: {}: {}, Count blocks with unincluded {:}",
				opts.start_block,
				session,
				session_timestamps.get(session).cloned().unwrap_or_default(),
				more_than_a_third,
			);
		}
	}
}

fn print_per_account_performance(
	opts: BitFieldsPerformance,
	session_queued_keys: Vec<(AccountId32, SessionKeys)>,
	network_cache_for_session: &PerSessionNetworkCache,
	per_account_by_session_stats: BTreeMap<AccountId32, BTreeMap<SessionIndex, BitfieldsStats>>,
	session_timestamps: &BTreeMap<u32, String>,
) {
	for (account, account_stats) in per_account_by_session_stats.iter() {
		let count_past_30 = account_stats
			.iter()
			.map(|(_, stats)| (stats.num_poor_performance as f64 * 100.0) / stats.num_present as f64)
			.filter(|x| *x > opts.poor_performance_threshold)
			.count();

		if count_past_30 < opts.num_sessions_bellow_threshold as usize {
			continue;
		}

		let session_keys_for_validator = &session_queued_keys.iter().find(|(this_account, _)| this_account == account);

		let name = if let Some((_, session_keys_for_validator)) = session_keys_for_validator {
			let authority_discovery_key = session_keys_for_validator.authority_discovery.0;
			let (_, info, _) = network_cache_for_session.get_details(authority_discovery_key);
			info
		} else {
			"unknown".to_string()
		};

		println!("block_start: {}, account: {} name: {}", opts.start_block, account, name);
		for (session, stats) in account_stats.iter() {
			let percentage = stats.percentage_poor();

			println!(
				"     at {} session: {}, has {:?} with zero bits, {:.2}% percent",
				session_timestamps.get(session).cloned().unwrap_or_default(),
				session,
				stats.num_poor_performance,
				percentage
			);
		}
	}
}

fn print_per_session_performance(
	opts: BitFieldsPerformance,
	per_session_by_validatory_stats: BTreeMap<u32, BTreeMap<u32, BitfieldsStats>>,
	session_timestamps: &BTreeMap<u32, String>,
) {
	let mut count_per_session_poor_performance = BTreeMap::new();
	for (session, stats_by_validator) in per_session_by_validatory_stats.iter() {
		for (validator, stats) in stats_by_validator.iter() {
			let percentage = stats.percentage_poor();
			if percentage > opts.poor_performance_threshold {
				*(count_per_session_poor_performance.entry(session).or_insert(0)) += 1;
			}

			println!(
				"block_start: {}, session: {}: validator: {:?} has {:?} zero bits {:.3}%",
				opts.start_block, session, validator, stats.num_poor_performance, percentage
			);
		}
	}

	for (session, count) in count_per_session_poor_performance.iter() {
		println!(
			"block_start: {}, session: {}: {} Number of validators with 10% missing {:} count_all_with_0 {:}",
			opts.start_block,
			session,
			session_timestamps.get(session).cloned().unwrap_or_default(),
			count,
			per_session_by_validatory_stats.len()
		);
	}
}

const DEFAULT_CACHE_DIR: &str = "whois_p2pcache";

// Information about the p2p network at a given session index.
#[derive(Serialize, Deserialize)]
struct PerSessionNetworkCache {
	/// PeerId to PeerDetails mapping.
	pub peer_details: HashMap<Vec<u8>, PeerDetails>,
	/// PeerId to version mapping.
	pub peer_versions: HashMap<Vec<u8>, String>,
	/// Authority Id to discovered addresses.
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
	async fn build_cache(
		session_index_now: u32,
		genesis_hash: H256,
		chain_name: &str,
		opts: &WhoIsOptions,
	) -> color_eyre::Result<Self, WhoisError> {
		let mut update_cache = opts.update_p2p_cache;
		let cache_path = format!("{}/{}", DEFAULT_CACHE_DIR, chain_name.to_ascii_lowercase());
		println!("Using cache path: {}", cache_path);
		let mut cache: NetworkCache = if let Ok(serialized_cache) = fs::read(cache_path.as_str()) {
			serde_binary::from_vec(serialized_cache, Endian::Big)
				.map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()))?
		} else {
			update_cache = true;
			Default::default()
		};
		let address_format = Ss58AddressFormat::all_names()
			.iter()
			.copied()
			.find(|x| chain_name.eq_ignore_ascii_case(x))
			.unwrap_or("substrate");
		if update_cache {
			println!("Discovering DHT authorithies, this may take a while...");
			let (authorithy_discovery, _) = subp2p_explorer_cli::commands::authorities::discover_authorities(
				opts.ws.clone(),
				format!("{:?}", genesis_hash),
				opts.bootnodes.clone(),
				opts.timeout,
				address_format.into(),
				Default::default(),
			)
			.await
			.map_err(|err| {
				println!("Error: {:?}", err);
				WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into())
			})?;

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
		if let Some(prefix) = path.parent() {
			let _ = std::fs::create_dir_all(prefix).map_err(|_| WhoisError::InvalidP2PCache(DEFAULT_CACHE_DIR.into()));
		};

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
			println!("Using p2p cache took at session: {}", closest_session_lower);
			Ok(self
				.session_to_network_info
				.get(closest_session_lower)
				.expect("closest_session_lower is obtained from session_to_network_info; qed"))
		} else if let Some(closest_session_larger) = closest_session_larger {
			println!("Using p2p cache took at session: {}", closest_session_larger);
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
	fn get_details(
		&self,
		authority_discovery_key: sr25519::PublicKey,
	) -> (Option<PeerDetails>, String, Option<PeerId>) {
		let Some(details) = self.authority_to_details.get(&authority_discovery_key) else {
			return (Default::default(), Default::default(), None)
		};

		let Some(addr) = details.iter().next() else { return (Default::default(), Default::default(), None) };

		let peer_id = get_peer_id(addr);
		let serialized_key = peer_id.map(|peer_d| peer_d.to_bytes()).unwrap_or_default();
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

	// Run sanity checks on the p2p cache and print the peers with known problems.
	// This are things that in the past have caused issues for validators.
	fn sanity_check(&self, current_discovery_keys: HashSet<[u8; 32]>) {
		let mut served_authorithies_by_peer = HashMap::new();
		println!("Running sanity checks on p2p cache for past present and future authorities");
		for (authority_id, addresses) in self.authority_to_details.iter() {
			let mut peer_ids = HashSet::new();
			if !current_discovery_keys.contains(authority_id) {
				continue;
			}
			// https://github.com/paritytech/polkadot-sdk/blob/b9eb68bcb5ab93e58bcba4425975ad00374da2bc/substrate/client/authority-discovery/src/worker.rs#L74
			const MAX_ADDRESSES_PER_AUTHORITY: usize = 10;
			if addresses.len() > MAX_ADDRESSES_PER_AUTHORITY {
				println!(
					"WARN: Authority 0x{:} has more than the maximum recommended addresses recommended {:} found {:} ",
					hex::encode(authority_id),
					MAX_ADDRESSES_PER_AUTHORITY,
					addresses.len()
				);
			}
			for address in addresses {
				let peer_id = get_peer_id(address);
				if let Some(peer_id) = peer_id {
					peer_ids.insert(peer_id);
					served_authorithies_by_peer
						.entry(peer_id)
						.or_insert(HashSet::new())
						.insert(authority_id);
				}
			}

			if peer_ids.len() > 1 {
				println!("WARN: Authority 0x{:} has multiple peer ids: {:?}", hex::encode(authority_id), peer_ids);
			}

			if peer_ids.is_empty() {
				println!("WARN: Authority 0x{:} has no peer ids", hex::encode(authority_id));
			}
		}

		for (peer_id, authorities) in served_authorithies_by_peer.iter() {
			if authorities.len() > 1 {
				println!(
					"WARN: Peer {:} serves multiple authorities: {:}",
					peer_id,
					authorities.iter().fold(String::new(), |acc, authority_id| {
						format!("{:} 0x{:}", acc, hex::encode(authority_id))
					})
				);
			}
		}
		println!("Sanity checks on p2p cache completed");
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
