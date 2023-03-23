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

/// Creates a new channel with a specified capacity and returns a tuple of Sender and Receiver structs.
///
/// The returned Sender and Receiver structs contain both a regular channel and a priority channel.
/// These channels can be used for asynchronous message passing between threads.
///
/// # Arguments
///
/// * capacity - The maximum number of messages that can be stored in both the regular and priority channels.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
	let (tx, rx) = bounded::<T>(capacity);
	let (tx_priority, rx_priority) = bounded::<T>(capacity);
	(Sender { inner_priority: tx_priority, inner: tx }, Receiver { inner_priority: rx_priority, inner: rx })
}

/// A `Receiver` is a public structure that allows for asynchronous message passing between threads.
/// It consists of two inner channels: a regular channel and a priority channel, both of type `T`
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

/// A trait for defining the strategy used when selecting between the priority and regular channels
/// in the `Receiver` struct.
///
/// Implementations of this trait should provide their own logic for deciding whether to try the
/// priority channel first by implementing the `should_try_priority` method.
pub trait SelectionStrategy {
	fn should_try_priority() -> bool;
}

/// A `SelectionStrategy` implementation that always tries the priority channel first.
///
/// This is the default strategy used by the `Receiver` struct.
pub struct PriorityFirst;
impl SelectionStrategy for PriorityFirst {
	fn should_try_priority() -> bool {
		true
	}
}

const PROBABILISTIC_THRESHOLD: f64 = 0.9;
/// A probabilistic send strategy with the probability of checking the priority channel
/// in 90% of the cases
pub struct Probabilistic;

/// A `SelectionStrategy` implementation that probabilistically tries the priority channel first.
///
/// The likelihood of trying the priority channel first is determined by a threshold value.
impl SelectionStrategy for Probabilistic {
	fn should_try_priority() -> bool {
		rand::random::<f64>() < PROBABILISTIC_THRESHOLD
	}
}

impl<T> Receiver<T> {
	/// Attempt to receive the next item using the default strategy.
	pub fn try_next(&mut self) -> Result<T, TryRecvError> {
		self.try_next_with_strategy::<PriorityFirst>()
	}

	/// Attempt to receive the next item using the provided selection strategy `S`.
	///
	/// # Type Parameters
	///
	/// * `S` - The selection strategy to use when choosing between the priority and regular channels.
	pub fn try_next_with_strategy<S: SelectionStrategy>(&mut self) -> Result<T, TryRecvError> {
		let (first_channel, second_channel) = if S::should_try_priority() {
			(&self.inner_priority, &self.inner)
		} else {
			(&self.inner, &self.inner_priority)
		};

		match first_channel.try_recv() {
			Ok(value) => Ok(value),
			Err(TryRecvError::Empty) => match second_channel.try_recv() {
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

	/// Returns if the channel is empty (meaning that both `inner` channels are empty)
	pub fn is_empty(&self) -> bool {
		self.inner.is_empty() && self.inner_priority.is_empty()
	}
}

impl<T> futures::stream::FusedStream for Receiver<T> {
	fn is_terminated(&self) -> bool {
		self.inner.is_terminated() || self.inner_priority.is_terminated()
	}
}

/// A `Sender` is a public structure that allows for asynchronous message passing between threads.
/// It consists of two inner channels: a regular channel and a priority channel, both of type `T`
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
		let fut = self.inner.send(msg);
		futures::pin_mut!(fut);
		fut.await.map_err(|_| SendError::Disconnected)
	}

	/// Send message over priority channel, wait until capacity is available.
	pub async fn send_priority(&mut self, msg: T) -> result::Result<(), SendError>
	where
		Self: Unpin,
	{
		let fut = self.inner_priority.send(msg);
		futures::pin_mut!(fut);
		fut.await.map_err(|_| SendError::Disconnected)
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
	#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
	struct Msg {
		val: u8,
	}

	#[tokio::test]
	async fn try_send_try_next() {
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
	}

	#[tokio::test]
	async fn multi_consumer() {
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
	}

	/// A `SelectionStrategy` implementation that always tries the regular channel first.
	pub struct RegularFirst;
	impl SelectionStrategy for RegularFirst {
		fn should_try_priority() -> bool {
			false
		}
	}

	#[tokio::test]
	async fn test_priority_first_strategy() {
		let (mut tx, mut rx) = channel(1);

		tx.send(43).await.unwrap();
		tx.send_priority(42).await.unwrap();

		let received_value = rx.try_next_with_strategy::<PriorityFirst>().unwrap();
		assert_eq!(received_value, 42);
	}

	#[tokio::test]
	async fn test_regular_first_strategy() {
		let (mut tx, mut rx) = channel(1);

		tx.send(43).await.unwrap();
		tx.send_priority(42).await.unwrap();

		let received_value = rx.try_next_with_strategy::<RegularFirst>().unwrap();
		assert_eq!(received_value, 43);
	}
}
