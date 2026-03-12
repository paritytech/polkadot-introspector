use clap::{ArgAction, Args};
use futures::future;
use log::{LevelFilter, error, warn};
use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};
use tokio::{
	signal,
	sync::{
		broadcast::{self, Receiver as BroadcastReceiver, Sender as BroadcastSender},
		mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
		watch,
	},
	task::JoinHandle,
};

#[derive(Clone, Debug, Args)]
pub struct VerbosityOptions {
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count, global = true)]
	pub verbose: u8,
}

/// Outcome of a supervised run: normal completion, restart, or cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOutcome {
	Completed,
	RestartRequested,
	Cancelled,
}

impl RunOutcome {
	/// Returns `true` if the outcome is a restart request.
	pub fn is_restart_requested(&self) -> bool {
		matches!(self, RunOutcome::RestartRequested)
	}
}

/// Shared handle for coordinating cancellation and lifecycle outcomes across tasks.
#[derive(Clone)]
pub struct RunContext {
	cancel_tx: watch::Sender<bool>,
	shutdown_tx: BroadcastSender<()>,
	outcome_tx: UnboundedSender<RunOutcome>,
	outcome_sent: Arc<AtomicBool>,
}

impl RunContext {
	/// Returns a watch receiver that becomes `true` when cancellation is requested.
	pub fn subscribe_cancel(&self) -> watch::Receiver<bool> {
		self.cancel_tx.subscribe()
	}

	/// Returns a broadcast receiver for legacy shutdown notifications (e.g. websocket listeners).
	pub fn subscribe_shutdown(&self) -> BroadcastReceiver<()> {
		self.shutdown_tx.subscribe()
	}

	fn cancel(&self) {
		if !*self.cancel_tx.borrow() {
			let _ = self.cancel_tx.send(true);
			let _ = self.shutdown_tx.send(());
		}
	}

	fn send_outcome(&self, outcome: RunOutcome) {
		if self.outcome_sent.swap(true, Ordering::AcqRel) {
			return;
		}
		if self.outcome_tx.send(outcome).is_err() {
			warn!("Outcome receiver already dropped, {:?} may be lost", outcome);
		}
	}

	/// Signals that work completed normally and cancels remaining tasks.
	pub fn complete(&self) {
		self.send_outcome(RunOutcome::Completed);
		self.cancel();
	}

	/// Signals that a restart is needed (e.g. RPC disconnect) and cancels remaining tasks.
	pub fn request_restart(&self) {
		self.send_outcome(RunOutcome::RestartRequested);
		self.cancel();
	}

	/// Signals user-initiated cancellation (e.g. ctrl+c) and cancels remaining tasks.
	pub fn request_cancel(&self) {
		self.send_outcome(RunOutcome::Cancelled);
		self.cancel();
	}
}

pub fn init_cli(opts: &VerbosityOptions) -> color_eyre::Result<()> {
	color_eyre::install()?;
	let log_level = match opts.verbose {
		0 => LevelFilter::Warn,
		1 => LevelFilter::Info,
		2 => LevelFilter::Debug,
		_ => LevelFilter::Trace,
	};
	env_logger::Builder::from_default_env()
		.filter(None, log_level)
		.format_timestamp(Some(env_logger::fmt::TimestampPrecision::Micros))
		.try_init()?;

	Ok(())
}

/// Creates a fresh `RunContext` and its paired outcome receiver.
pub fn init_run_context() -> (RunContext, UnboundedReceiver<RunOutcome>) {
	let (outcome_tx, outcome_rx) = unbounded_channel();
	let (shutdown_tx, _) = broadcast::channel(10);
	let (cancel_tx, _) = watch::channel(false);

	(RunContext { cancel_tx, shutdown_tx, outcome_tx, outcome_sent: Arc::new(AtomicBool::new(false)) }, outcome_rx)
}

/// Waits for ctrl+c and requests cancellation on the given context.
pub async fn on_shutdown(run_context: RunContext) {
	match signal::ctrl_c().await {
		Ok(()) => run_context.request_cancel(),
		Err(e) => {
			error!("Failed to listen for shutdown signal: {:?}", e);
			run_context.request_cancel();
		},
	}
}

/// Spawns a background task that listens for ctrl+c and triggers cancellation.
pub fn spawn_shutdown_listener(run_context: RunContext) -> JoinHandle<()> {
	tokio::spawn(on_shutdown(run_context))
}

/// Runs worker futures until an outcome is signaled, then aborts remaining workers.
pub async fn run_supervised(
	futures: Vec<tokio::task::JoinHandle<()>>,
	shutdown_listener: JoinHandle<()>,
	outcome_rx: &mut UnboundedReceiver<RunOutcome>,
) -> color_eyre::Result<RunOutcome> {
	let abort_handles: Vec<_> = futures.iter().map(|future| future.abort_handle()).collect();
	let workers = future::try_join_all(futures);
	tokio::pin!(workers);

	let outcome = tokio::select! {
		result = &mut workers => {
			shutdown_listener.abort();
			match result {
				Ok(_) => {},
				Err(e) if e.is_cancelled() => {},
				Err(e) => return Err(e.into()),
			}
			return Ok(RunOutcome::Completed);
		},
		maybe_outcome = outcome_rx.recv() => {
			match maybe_outcome {
				Some(outcome) => outcome,
				None => {
					error!("All RunContext senders dropped without signaling an outcome");
					RunOutcome::Cancelled
				},
			}
		}
	};

	for handle in &abort_handles {
		handle.abort();
	}
	let _ = workers.await;
	shutdown_listener.abort();
	Ok(outcome)
}
