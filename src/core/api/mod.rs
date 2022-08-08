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
use tokio::sync::mpsc::{channel, Sender};

use crate::core::{RecordsStorageConfig, MAX_MSG_QUEUE_SIZE};

mod storage;
mod subxt_wrapper;

// Provides access to subxt and storage APIs, more to come.
#[derive(Clone)]
pub struct ApiService {
	subxt_tx: Sender<subxt_wrapper::Request>,
	storage_tx: Sender<storage::Request>,
}

impl ApiService {
	pub fn new_with_storage(storage_config: RecordsStorageConfig) -> ApiService {
		let (subxt_tx, subxt_rx) = channel(MAX_MSG_QUEUE_SIZE);
		let (storage_tx, storage_rx) = channel(MAX_MSG_QUEUE_SIZE);

		tokio::spawn(subxt_wrapper::api_handler_task(subxt_rx));
		tokio::spawn(storage::api_handler_task(storage_rx, storage_config));

		Self { subxt_tx, storage_tx }
	}

	pub fn storage(&self) -> storage::RequestExecutor {
		storage::RequestExecutor::new(self.storage_tx.clone())
	}

	pub fn subxt(&self) -> subxt_wrapper::RequestExecutor {
		subxt_wrapper::RequestExecutor::new(self.subxt_tx.clone())
	}
}
