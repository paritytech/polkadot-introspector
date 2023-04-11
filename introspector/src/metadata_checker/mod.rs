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

use clap::Parser;
use crossterm::style::Stylize;
use subxt::{OnlineClient, PolkadotConfig};

use essentials::metadata::polkadot;

#[derive(Clone, Debug, Parser)]
#[clap(rename_all = "kebab-case")]
pub(crate) struct MetadataCheckerOptions {
	/// Web-Socket URL of a relay chain node.
	#[clap(name = "ws", long)]
	pub url: String,
}

pub(crate) struct MetadataChecker {
	opts: MetadataCheckerOptions,
}

impl MetadataChecker {
	pub(crate) fn new(opts: MetadataCheckerOptions) -> color_eyre::Result<Self> {
		Ok(Self { opts })
	}

	pub(crate) async fn run(self) -> color_eyre::Result<()> {
		print!("[metadata-checker] Checking metadata for {}... ", self.opts.url);
		let api = OnlineClient::<PolkadotConfig>::from_url(self.opts.url).await.unwrap();

		match polkadot::validate_codegen(&api) {
			Ok(_) => println!("{}", "OK".green()),
			Err(_) => {
				println!("{}", "FAILED".red());
				std::process::exit(1);
			},
		};

		Ok(())
	}
}
