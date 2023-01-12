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
#![allow(dead_code)]

use crate::{
	core::storage::{
		HashedPlainRecordsStorage, HashedPrefixedRecordsStorage, PrefixedRecordsStorage, RecordsStorage,
		RecordsStorageConfig, StorageEntry,
	},
	eyre,
};
use std::{fmt::Debug, hash::Hash};
use tokio::sync::{
	mpsc::{Receiver, Sender},
	oneshot,
};

// Storage requests
#[derive(Clone, Debug)]
pub enum RequestType<K, P> {
	Write(K, StorageEntry),
	WritePrefix(P, K, StorageEntry),
	Read(K),
	ReadPrefix(P, K),
	Delete(K),
	DeletePrefix(P, K),
	Replace(K, StorageEntry),
	ReplacePrefix(P, K, StorageEntry),
	Size,
	Keys,
	Prefixes,
	KeysWithPrefix(P),
}

#[derive(Debug)]
pub struct Request<K, P> {
	pub request_type: RequestType<K, P>,
	pub response_sender: Option<oneshot::Sender<Response<K, P>>>,
}

#[derive(Debug)]
pub enum Response<K, P> {
	Read(Option<StorageEntry>),
	Size(usize),
	Keys(Vec<K>),
	Prefixes(Vec<P>),
	Status(color_eyre::Result<()>),
}
pub struct RequestExecutor<K, P> {
	to_api: Sender<Request<K, P>>,
}

impl<K, P> RequestExecutor<K, P>
where
	P: Debug,
	K: Debug,
{
	pub fn new(to_api: Sender<Request<K, P>>) -> Self {
		RequestExecutor { to_api }
	}
	/// Write a value to storage. Panics if API channel is gone.
	pub async fn storage_write(&self, key: K, value: StorageEntry) -> color_eyre::Result<()> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Write(key, value), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");
		match receiver.await {
			Ok(Response::Status(res)) => res,
			Ok(_) => Err(eyre!("invalid response on write command")),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Write a value with a prefix to storage. Panics if API channel is gone.
	pub async fn storage_write_prefixed(&self, prefix: P, key: K, value: StorageEntry) -> color_eyre::Result<()> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request =
			Request { request_type: RequestType::WritePrefix(prefix, key, value), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");
		match receiver.await {
			Ok(Response::Status(res)) => res,
			Ok(_) => Err(eyre!("invalid response on write command")),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Replaces a value in storage. Panics if API channel is gone.
	pub async fn storage_replace(&self, key: K, value: StorageEntry) {
		let request = Request { request_type: RequestType::Replace(key, value), response_sender: None };
		self.to_api.send(request).await.expect("Channel closed");
	}

	/// Replaces a value in storage at the specific prefix. Panics if API channel is gone.
	pub async fn storage_replace_prefix(&self, prefix: P, key: K, value: StorageEntry) {
		let request = Request { request_type: RequestType::ReplacePrefix(prefix, key, value), response_sender: None };
		self.to_api.send(request).await.expect("Channel closed");
	}

	/// Read a value from storage. Returns `None` if the key is not found.
	pub async fn storage_read(&self, key: K) -> Option<StorageEntry> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Read(key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Read(Some(value))) => Some(value),
			Ok(_) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Read a value from storage at specific prefix. Returns `None` if the key is not found.
	pub async fn storage_read_prefixed(&self, prefix: P, key: K) -> Option<StorageEntry> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::ReadPrefix(prefix, key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Read(Some(value))) => Some(value),
			Ok(_) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Delete a value from storage. Returns `None` if the key is not found.
	pub async fn storage_delete(&self, key: K) -> Option<StorageEntry> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Delete(key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Read(Some(value))) => Some(value),
			Ok(_) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Delete a value from storage at specific prefix. Returns `None` if the key is not found.
	pub async fn storage_delete_prefixed(&self, prefix: P, key: K) -> Option<StorageEntry> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::DeletePrefix(prefix, key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Read(Some(value))) => Some(value),
			Ok(_) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns number of entries in a storage
	pub async fn storage_len(&self) -> usize {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Size, response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Size(len)) => len,
			Ok(_) => panic!("Storage API error: invalid size reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns all keys from a storage
	pub async fn storage_keys(&self) -> Vec<K> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Keys, response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Keys(value)) => value,
			Ok(_) => panic!("Storage API error: invalid keys reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns keys matching a specific prefix from a storage
	pub async fn storage_keys_prefix(&self, prefix: P) -> Vec<K> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::KeysWithPrefix(prefix), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Keys(value)) => value,
			Ok(_) => panic!("Storage API error: invalid keys reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	pub async fn storage_prefixes(&self) -> Vec<P> {
		let (sender, receiver) = oneshot::channel::<Response<K, P>>();
		let request = Request { request_type: RequestType::Prefixes, response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::Prefixes(value)) => value,
			Ok(_) => panic!("Storage API error: invalid keys reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}
}

/// Creates a task that handles storage API calls (generic version with no prefixes support).
pub(crate) async fn api_handler_task<K>(mut api: Receiver<Request<K, ()>>, storage_config: RecordsStorageConfig)
where
	K: Eq + Sized + Hash + Debug + Clone,
{
	// The storage lives here.
	let mut the_storage = HashedPlainRecordsStorage::new(storage_config);

	while let Some(request) = api.recv().await {
		match request.request_type {
			RequestType::Read(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.get(&key)))
					.unwrap();
			},
			RequestType::Delete(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.delete(&key)))
					.unwrap();
			},
			RequestType::Write(key, value) => {
				let res = the_storage.insert(key, value);

				if let Some(sender) = request.response_sender {
					// A callee wants to know about the errors
					sender.send(Response::Status(res)).unwrap();
				}
			},
			RequestType::Replace(key, value) => {
				let res = the_storage.replace(&key, value);

				if let Some(sender) = request.response_sender {
					sender.send(Response::Read(res)).unwrap();
				}
			},
			RequestType::Size => {
				let size = the_storage.len();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Size(size))
					.unwrap();
			},
			RequestType::Keys => {
				let keys = the_storage.keys();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Keys(keys))
					.unwrap();
			},
			RequestType::KeysWithPrefix(_) => {
				unimplemented!()
			},
			RequestType::WritePrefix(_, _, _) => {
				unimplemented!()
			},
			RequestType::ReadPrefix(_, _) => {
				unimplemented!()
			},
			RequestType::DeletePrefix(_, _) => {
				unimplemented!()
			},
			RequestType::ReplacePrefix(_, _, _) => {
				unimplemented!()
			},
			RequestType::Prefixes => {
				unimplemented!()
			},
		}
	}
}

/// Creates the API handler with prefixes support.
pub(crate) async fn api_handler_task_prefixed<K, P>(
	mut api: Receiver<Request<K, P>>,
	storage_config: RecordsStorageConfig,
) where
	K: Eq + Sized + Hash + Debug + Clone,
	P: Eq + Sized + Hash + Debug + Clone,
{
	// The storage lives here.
	let mut the_storage = HashedPrefixedRecordsStorage::new(storage_config);

	while let Some(request) = api.recv().await {
		match request.request_type {
			RequestType::Read(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.get(&key)))
					.unwrap();
			},
			RequestType::Delete(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.delete(&key)))
					.unwrap();
			},
			RequestType::Write(key, value) => {
				let res = the_storage.insert(key, value);

				if let Some(sender) = request.response_sender {
					// A callee wants to know about the errors
					sender.send(Response::Status(res)).unwrap();
				}
			},
			RequestType::Replace(key, value) => {
				let res = the_storage.replace(&key, value);

				if let Some(sender) = request.response_sender {
					sender.send(Response::Read(res)).unwrap();
				}
			},
			RequestType::Size => {
				let size = the_storage.len();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Size(size))
					.unwrap();
			},
			RequestType::Keys => {
				let keys = the_storage.keys();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Keys(keys))
					.unwrap();
			},
			RequestType::KeysWithPrefix(prefix) => {
				let keys = the_storage.prefixed_keys(&prefix);
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Keys(keys))
					.unwrap();
			},
			RequestType::Prefixes => {
				let prefixes = the_storage.prefixes();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Prefixes(prefixes))
					.unwrap();
			},
			RequestType::WritePrefix(prefix, key, value) => {
				let res = the_storage.insert_prefix(prefix, key, value);

				if let Some(sender) = request.response_sender {
					// A callee wants to know about the errors
					sender.send(Response::Status(res)).unwrap();
				}
			},
			RequestType::ReadPrefix(prefix, key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.get_prefix(&prefix, &key)))
					.unwrap();
			},
			RequestType::DeletePrefix(prefix, key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::Read(the_storage.delete_prefix(&prefix, &key)))
					.unwrap();
			},
			RequestType::ReplacePrefix(prefix, key, value) => {
				let res = the_storage.replace_prefix(&prefix, &key, value);

				if let Some(sender) = request.response_sender {
					sender.send(Response::Read(res)).unwrap();
				}
			},
		}
	}
}
