use clap::{ArgAction, Parser};
use futures::future;
use log::LevelFilter;
use tokio::{signal, sync::broadcast};

#[derive(Clone, Debug, Parser)]
#[clap(
    allow_external_subcommands = true, // HACK: to parse it as a standalone config
)]
pub struct VerbosityOptions {
	/// Verbosity level: -v - info, -vv - debug, -vvv - trace
	#[clap(short = 'v', long, action = ArgAction::Count)]
	pub verbose: u8,
}

pub fn init_cli() -> color_eyre::Result<()> {
	color_eyre::install()?;

	let opts = VerbosityOptions::parse();
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

pub fn init_shutdown() -> broadcast::Sender<()> {
	let (shutdown_tx, _) = broadcast::channel(1);
	shutdown_tx
}

pub fn init_futures_with_shutdown(
	mut futures: Vec<tokio::task::JoinHandle<()>>,
	shutdown_tx: broadcast::Sender<()>,
) -> Vec<tokio::task::JoinHandle<()>> {
	futures.push(tokio::spawn(on_shutdown(shutdown_tx)));
	futures
}

pub async fn on_shutdown(shutdown_tx: broadcast::Sender<()>) {
	signal::ctrl_c().await.unwrap();
	let _ = shutdown_tx.send(());
}

pub async fn run(futures: Vec<tokio::task::JoinHandle<()>>) -> color_eyre::Result<()> {
	future::try_join_all(futures).await?;
	Ok(())
}
