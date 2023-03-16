// Copyright 2017-2021 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

use std::{
	pin::Pin,
	result,
	task::{Context, Poll},
};
// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.
use async_channel::bounded;
pub use async_channel::{TryRecvError, TrySendError};
use futures::Stream;

/// Create a wrapped `mpsc::broadcast` pair of `Sender` and `Receiver`.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
	let (tx, rx) = bounded::<T>(capacity);
	let (tx_priority, rx_priority) = bounded::<T>(capacity);
	(Sender { inner_priority: tx_priority, inner: tx }, Receiver { inner_priority: rx_priority, inner: rx })
}

/// A receiver tracking the messages consumed by itself.
#[derive(Debug)]
pub struct Receiver<T> {
	inner: async_channel::Receiver<T>,
	inner_priority: async_channel::Receiver<T>,
}

impl<T> std::ops::Deref for Receiver<T> {
	type Target = async_channel::Receiver<T>;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T> std::ops::DerefMut for Receiver<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<T> Stream for Receiver<T> {
	type Item = T;
	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		match async_channel::Receiver::poll_next(Pin::new(&mut self.inner_priority), cx) {
			Poll::Ready(maybe_value) => Poll::Ready(maybe_value),
			Poll::Pending => match async_channel::Receiver::poll_next(Pin::new(&mut self.inner), cx) {
				Poll::Ready(maybe_value) => Poll::Ready(maybe_value),
				Poll::Pending => Poll::Pending,
			},
		}
	}

	fn size_hint(&self) -> (usize, Option<usize>) {
		self.inner.size_hint()
	}
}

impl<T> Clone for Receiver<T> {
	fn clone(&self) -> Self {
		Self { inner: self.inner.clone(), inner_priority: self.inner_priority.clone() }
	}
}

impl<T> Receiver<T> {
	/// Attempt to receive the next item.
	pub fn try_next(&mut self) -> Result<T, TryRecvError> {
		match self.inner_priority.try_recv() {
			Ok(value) => Ok(value),
			Err(TryRecvError::Empty) => match self.inner.try_recv() {
				Ok(value) => Ok(value),
				Err(err) => Err(err),
			},
			Err(err) => Err(err),
		}
	}

	/// Returns the current number of messages in the channel
	pub fn len(&self) -> usize {
		self.inner.len() + self.inner_priority.len()
	}

	pub fn is_empty(&self) -> bool {
		self.inner.is_empty() && self.inner_priority.is_empty()
	}
}

impl<T> futures::stream::FusedStream for Receiver<T> {
	fn is_terminated(&self) -> bool {
		self.inner.is_terminated() || self.inner_priority.is_terminated()
	}
}

#[derive(Debug)]
pub struct Sender<T> {
	inner: async_channel::Sender<T>,
	inner_priority: async_channel::Sender<T>,
}

/// A bounded channel error
#[derive(thiserror::Error, Debug)]
pub enum SendError {
	#[error("Bounded channel has been disconnected")]
	Disconnected,
}

impl<T> Clone for Sender<T> {
	fn clone(&self) -> Self {
		Self { inner: self.inner.clone(), inner_priority: self.inner_priority.clone() }
	}
}

impl<T> std::ops::Deref for Sender<T> {
	type Target = async_channel::Sender<T>;
	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl<T> std::ops::DerefMut for Sender<T> {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl<T> Sender<T> {
	/// Send message, wait until capacity is available.
	pub async fn send(&mut self, msg: T) -> result::Result<(), SendError>
	where
		Self: Unpin,
	{
		match self.inner.try_send(msg) {
			Err(send_err) => {
				if !send_err.is_full() {
					return Err(SendError::Disconnected)
				}
				let fut = self.inner.send(send_err.into_inner());
				futures::pin_mut!(fut);
				fut.await.map_err(|_| SendError::Disconnected)
			},
			_ => Ok(()),
		}
	}

	/// Send message over priority channel, wait until capacity is available.
	pub async fn send_priority(&mut self, msg: T) -> async_channel::Send<'_, T>
	where
		Self: Unpin,
	{
		self.inner_priority.send(msg)
	}

	/// Attempt to send message or fail immediately.
	pub fn try_send(&mut self, msg: T) -> result::Result<(), TrySendError<T>> {
		self.inner.try_send(msg)
	}

	/// Attempt to send message over priority channel or fail immediately.
	pub fn try_send_priority(&mut self, msg: T) -> result::Result<(), TrySendError<T>> {
		self.inner_priority.try_send(msg)
	}

	/// Returns the current number of messages in the channel
	pub fn len(&self) -> usize {
		self.inner.len() + self.inner_priority.len()
	}

	pub fn is_empty(&self) -> bool {
		self.inner.is_empty() && self.inner_priority.is_empty()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use futures::executor::block_on;
	#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
	struct Msg {
		val: u8,
	}

	#[test]
	fn try_send_try_next() {
		block_on(async move {
			let (mut tx, mut rx) = channel::<Msg>(5);
			let msg = Msg::default();
			tx.try_send(msg).unwrap();
			tx.try_send(msg).unwrap();
			tx.try_send(msg).unwrap();
			tx.try_send_priority(Msg { val: 42 }).unwrap();
			assert_eq!(rx.try_next().unwrap(), Msg { val: 42 });
			assert_eq!(rx.try_next().unwrap(), Msg::default());
			assert_eq!(rx.try_next().unwrap(), Msg::default());
			assert_eq!(rx.try_next().unwrap(), Msg::default());
			assert!(rx.try_next().is_err());
		});
	}

	#[test]
	fn multi_consumer() {
		block_on(async move {
			let (mut tx, mut rx) = channel::<Msg>(5);
			let mut rx_clone = rx.clone();
			let msg = Msg::default();
			tx.try_send(msg).unwrap();
			tx.try_send(msg).unwrap();
			tx.try_send(msg).unwrap();
			tx.try_send_priority(Msg { val: 42 }).unwrap();
			assert_eq!(rx.try_next().unwrap(), Msg { val: 42 });
			assert_eq!(rx_clone.try_next().unwrap(), Msg::default());
			assert_eq!(rx.try_next().unwrap(), Msg::default());
			assert_eq!(rx_clone.try_next().unwrap(), Msg::default());
			assert!(rx.try_next().is_err());
			assert!(rx_clone.try_next().is_err());
		});
	}
}
