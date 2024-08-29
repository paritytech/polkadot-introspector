use clap::{ArgAction, Args};
use futures::future;
use log::LevelFilter;
use tokio::{
	signal,
	sync::broadcast::{self, Sender as BroadcastSender},
};

#[derive(Clone, Debug, Args)]
pub struct VerbosityOptions {
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count, global = true)]
	pub verbose: u8,
}

#[derive(Clone, Copy, Debug)]
pub enum Shutdown {
	Graceful,
	Restart,
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

pub fn init_shutdown() -> BroadcastSender<Shutdown> {
	broadcast::channel(10).0
}

pub async fn on_shutdown(shutdown_tx: BroadcastSender<Shutdown>) {
	let mut shutdown_rx = shutdown_tx.subscribe();
	tokio::select! {
		_ = signal::ctrl_c() => {
			shutdown_tx.send(Shutdown::Graceful).unwrap();
		},
		_ = shutdown_rx.recv() => {}
	}
}

pub async fn run(
	mut futures: Vec<tokio::task::JoinHandle<()>>,
	shutdown_tx: &BroadcastSender<Shutdown>,
) -> color_eyre::Result<()> {
	futures.push(tokio::spawn(on_shutdown(shutdown_tx.clone())));
	future::try_join_all(futures).await?;

	Ok(())
}
