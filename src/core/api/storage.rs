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

use crate::core::storage::{RecordsStorage, RecordsStorageConfig, StorageInfo};
use codec::{Decode, Encode};
use subxt::sp_core::H256;
use tokio::sync::{
	mpsc::{Receiver, Sender},
	oneshot,
};

// Storage requests
#[derive(Clone, Debug)]
pub enum RequestType {
	StorageWrite(H256, Vec<u8>),
	StorageRead(H256),
}

#[derive(Debug)]
pub struct Request {
	pub request_type: RequestType,
	pub response_sender: Option<oneshot::Sender<Response>>,
}

#[derive(Debug)]
pub enum Response {
	StorageReadResponse(Option<Vec<u8>>),
}
pub struct RequestExecutor {
	to_api: Sender<Request>,
}

impl RequestExecutor {
	pub fn new(to_api: Sender<Request>) -> Self {
		RequestExecutor { to_api }
	}
	/// Write a value to storage. Panics if API channel is gone.
	pub async fn storage_write<Value: Encode + StorageInfo>(&self, key: H256, value: Value) {
		let request = Request { request_type: RequestType::StorageWrite(key, value.encode()), response_sender: None };
		self.to_api.send(request).await.expect("Channel closed");
	}

	/// Read a value from storage. Returns `None` if the key is not found.
	pub async fn storage_read<Value: Decode + StorageInfo>(&self, key: H256) -> Option<Value> {
		let (sender, receiver) = oneshot::channel::<Response>();
		let request = Request { request_type: RequestType::StorageRead(key), response_sender: Some(sender) };
		self.to_api.send(request).await.expect("Channel closed");

		match receiver.await {
			Ok(Response::StorageReadResponse(Some(value))) =>
				Some(Value::decode(&mut value.as_slice()).expect("decode failed")),
			Ok(Response::StorageReadResponse(None)) => None,
			Err(err) => panic!("Storage API error {}", err),
		}
	}
}

// A task that handles storage API calls.
pub(crate) async fn api_handler_task(mut api: Receiver<Request>, storage_config: RecordsStorageConfig) {
	// The storage lives here.
	let mut the_storage = RecordsStorage::new(storage_config);

	loop {
		if let Some(request) = api.recv().await {
			match request.request_type {
				RequestType::StorageRead(key) => {
					request
						.response_sender
						.expect("no sender provided")
						.send(Response::StorageReadResponse(the_storage.get(&key)))
						.unwrap();
				},
				RequestType::StorageWrite(key, value) => {
					the_storage.insert(key, value);
				},
			}
		} else {
			// Channel closed.
			break
		}
	}
}
