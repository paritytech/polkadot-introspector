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
	core::storage::{HasPrefix, RecordsStorage, RecordsStorageConfig, StorageEntry},
	eyre,
};
use std::{fmt::Debug, hash::Hash};
use tokio::sync::{
	mpsc::{Receiver, Sender},
	oneshot,
};

// Storage requests
#[derive(Clone, Debug)]
pub enum RequestType<K: Ord + Hash + Sized> {
	Write(K, StorageEntry),
	Read(K),
	Replace(K, StorageEntry),
	Size,
	Keys,
	KeysWithPrefix(K),
}

#[derive(Debug)]
pub struct Request<K: Ord + Hash + Sized> {
	pub request_type: RequestType<K>,
	pub response_sender: Option<oneshot::Sender<Response<K>>>,
}

#[derive(Debug)]
pub enum Response<K: Ord + Hash + Sized> {
	StorageReadResponse(Option<StorageEntry>),
	StorageSizeResponse(usize),
	StorageKeysResponse(Vec<K>),
	StorageStatusResponse(color_eyre::Result<()>),
}
pub struct RequestExecutor<K: Ord + Hash + Sized> {
	to_api: Sender<Request<K>>,
}

impl<K: Ord + Hash + Sized + Debug> RequestExecutor<K> {
	pub fn new(to_api: Sender<Request<K>>) -> Self {
		RequestExecutor { to_api }
	}
	/// Write a value to storage. Panics if API channel is gone.
	pub async fn storage_write(&self, key: K, value: StorageEntry) -> color_eyre::Result<()> {
		let (sender, receiver) = oneshot::channel::<Response<K>>();
		let request = Request { request_type: RequestType::Write(key, value), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");
		match receiver.await {
			Ok(Response::StorageStatusResponse(res)) => res,
			Ok(_) => Err(eyre!("invalid response on write command")),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Replaces a value in storage. Panics if API channel is gone.
	pub async fn storage_replace(&self, key: K, value: StorageEntry) {
		let request = Request { request_type: RequestType::Replace(key, value), response_sender: None };
		self.to_api.send(request).await.expect("Channel closed");
	}

	/// Read a value from storage. Returns `None` if the key is not found.
	pub async fn storage_read(&self, key: K) -> Option<StorageEntry> {
		let (sender, receiver) = oneshot::channel::<Response<K>>();
		let request = Request { request_type: RequestType::Read(key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::StorageReadResponse(Some(value))) => Some(value),
			Ok(_) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns number of entries in a storage
	pub async fn storage_len(&self) -> usize {
		let (sender, receiver) = oneshot::channel::<Response<K>>();
		let request = Request { request_type: RequestType::Size, response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::StorageSizeResponse(len)) => len,
			Ok(_) => panic!("Storage API error: invalid size reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns all keys from a storage
	pub async fn storage_keys(&self) -> Vec<K> {
		let (sender, receiver) = oneshot::channel::<Response<K>>();
		let request = Request { request_type: RequestType::Keys, response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::StorageKeysResponse(value)) => value,
			Ok(_) => panic!("Storage API error: invalid keys reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}

	/// Returns keys matching a specific prefix from a storage
	pub async fn storage_keys_prefix(&self, prefix: K) -> Vec<K> {
		let (sender, receiver) = oneshot::channel::<Response<K>>();
		let request = Request { request_type: RequestType::KeysWithPrefix(prefix), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::StorageKeysResponse(value)) => value,
			Ok(_) => panic!("Storage API error: invalid keys reply"),
			Err(err) => panic!("Storage API error {}", err),
		}
	}
}

/// Creates a task that handles storage API calls (generic version with no prefixes support).
pub(crate) async fn api_handler_task<K>(mut api: Receiver<Request<K>>, storage_config: RecordsStorageConfig)
where
	K: Ord + Sized + Hash + Debug + Clone,
{
	// The storage lives here.
	let mut the_storage = RecordsStorage::new(storage_config);

	while let Some(request) = api.recv().await {
		match request.request_type {
			RequestType::Read(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageReadResponse(the_storage.get(&key)))
					.unwrap();
			},
			RequestType::Write(key, value) => {
				let res = the_storage.insert(key, value);

				if let Some(sender) = request.response_sender {
					// A callee wants to know about the errors
					sender.send(Response::StorageStatusResponse(res)).unwrap();
				}
			},
			RequestType::Replace(key, value) => {
				let res = the_storage.replace(key, value);

				if let Some(sender) = request.response_sender {
					sender.send(Response::StorageReadResponse(res)).unwrap();
				}
			},
			RequestType::Size => {
				let size = the_storage.len();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageSizeResponse(size))
					.unwrap();
			},
			RequestType::Keys => {
				let keys = the_storage.keys();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageKeysResponse(keys))
					.unwrap();
			},
			RequestType::KeysWithPrefix(_) => {
				unimplemented!()
			},
		}
	}
}

/// Creates the API handler with prefixes support. `K` must has `HasPrefix` trait
pub(crate) async fn api_handler_task_prefixed<K>(mut api: Receiver<Request<K>>, storage_config: RecordsStorageConfig)
where
	K: Ord + Sized + Hash + Debug + Clone + HasPrefix,
{
	// The storage lives here.
	let mut the_storage = RecordsStorage::new(storage_config);

	while let Some(request) = api.recv().await {
		match request.request_type {
			RequestType::Read(key) => {
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageReadResponse(the_storage.get(&key)))
					.unwrap();
			},
			RequestType::Write(key, value) => {
				let res = the_storage.insert(key, value);

				if let Some(sender) = request.response_sender {
					// A callee wants to know about the errors
					sender.send(Response::StorageStatusResponse(res)).unwrap();
				}
			},
			RequestType::Replace(key, value) => {
				let res = the_storage.replace(key, value);

				if let Some(sender) = request.response_sender {
					sender.send(Response::StorageReadResponse(res)).unwrap();
				}
			},
			RequestType::Size => {
				let size = the_storage.len();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageSizeResponse(size))
					.unwrap();
			},
			RequestType::Keys => {
				let keys = the_storage.keys();
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageKeysResponse(keys))
					.unwrap();
			},
			RequestType::KeysWithPrefix(prefix) => {
				let keys = the_storage.keys_prefix(&prefix);
				request
					.response_sender
					.expect("no sender provided")
					.send(Response::StorageKeysResponse(keys))
					.unwrap();
			},
		}
	}
}
