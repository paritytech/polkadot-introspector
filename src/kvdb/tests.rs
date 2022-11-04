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

#[cfg(test)]
use crate::kvdb::IntrospectorKvdb;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use std::path::PathBuf;

const TEST_KEY: [u8; 3] = [0u8, 1u8, 2u8];
const TEST_VALUE: [u8; 3] = [0u8, 1u8, 2u8];

fn test_data_iter() -> impl Iterator<Item = (&'static [u8], &'static [u8])> {
	let mut count = 0;
	std::iter::from_fn(move || {
		if count == 0 {
			count += 1;
			Some((&TEST_KEY[..], &TEST_VALUE[..]))
		} else {
			None
		}
	})
}

fn make_temp_dir() -> PathBuf {
	let tmpdir = std::env::temp_dir();
	let mut rng = thread_rng();
	let suffix: String = (0..20).map(|_| rng.sample(Alphanumeric) as char).collect();
	let suffix = format!("intro-kvdb-test-{}", suffix);
	let path = tmpdir.join(suffix);
	std::fs::create_dir(&path).unwrap();
	path
}

fn write_db<D: IntrospectorKvdb>(db: &D, ncolumns: usize) {
	for col_idx in 0..ncolumns {
		db.write_iter(format!("col{}", col_idx).as_str(), test_data_iter()).unwrap();
	}
}

fn copy_db<S: IntrospectorKvdb, D: IntrospectorKvdb>(src_db: &S, dest_db: &D, ncolumns: usize) {
	for col_idx in 0..ncolumns {
		let column = format!("col{}", col_idx);
		let src_it = src_db.iter_values(column.as_str()).unwrap();
		dest_db.write_iter(column.as_str(), src_it).unwrap();
	}
}

fn check_db<D: IntrospectorKvdb>(db: &D, ncolumns: usize) {
	for col_idx in 0..ncolumns {
		let column = format!("col{}", col_idx);
		let mut cnt = 0;
		for (key, value) in db.iter_values(column.as_str()).unwrap() {
			cnt += 1;
			assert_eq!(key.as_ref(), TEST_KEY);
			assert_eq!(value.as_ref(), TEST_VALUE);
		}

		assert_eq!(cnt, 1);
	}
}

#[test]
fn test_migration_rocksdb_rocksdb() {
	let ncolumns = 10;
	let src_dir = make_temp_dir();
	let src_db = crate::kvdb::rocksdb::tests::new_test_rocks_db(src_dir.as_path(), ncolumns);

	write_db(&src_db, ncolumns);
	let dst_dir = make_temp_dir();

	{
		let dest_db = crate::kvdb::rocksdb::IntrospectorRocksDB::new_dumper(&src_db, dst_dir.as_path()).unwrap();
		copy_db(&src_db, &dest_db, ncolumns);
	}

	let dest_db = crate::kvdb::rocksdb::IntrospectorRocksDB::new(dst_dir.as_path()).unwrap();
	check_db(&dest_db, ncolumns);
	std::fs::remove_dir_all(src_dir).unwrap();
	std::fs::remove_dir_all(dst_dir).unwrap();
}

#[test]
fn test_migration_paritydb_paritydb() {
	let ncolumns = 10;
	let src_dir = make_temp_dir();
	let src_db = crate::kvdb::paritydb::tests::new_test_parity_db(src_dir.as_path(), ncolumns);

	write_db(&src_db, ncolumns);
	let dst_dir = make_temp_dir();

	{
		let dest_db = crate::kvdb::paritydb::IntrospectorParityDB::new_dumper(&src_db, dst_dir.as_path()).unwrap();
		copy_db(&src_db, &dest_db, ncolumns);
	}

	let dest_db = crate::kvdb::paritydb::IntrospectorParityDB::new(dst_dir.as_path()).unwrap();
	check_db(&dest_db, ncolumns);
	std::fs::remove_dir_all(src_dir).unwrap();
	std::fs::remove_dir_all(dst_dir).unwrap();
}

#[test]
fn test_migration_paritydb_rocksdb() {
	let ncolumns = 10;
	let src_dir = make_temp_dir();
	let src_db = crate::kvdb::paritydb::tests::new_test_parity_db(src_dir.as_path(), ncolumns);

	write_db(&src_db, ncolumns);
	let dst_dir = make_temp_dir();

	{
		let dest_db = crate::kvdb::rocksdb::IntrospectorRocksDB::new_dumper(&src_db, dst_dir.as_path()).unwrap();
		copy_db(&src_db, &dest_db, ncolumns);
	}

	let dest_db = crate::kvdb::rocksdb::IntrospectorRocksDB::new(dst_dir.as_path()).unwrap();
	check_db(&dest_db, ncolumns);
	std::fs::remove_dir_all(src_dir).unwrap();
	std::fs::remove_dir_all(dst_dir).unwrap();
}

#[test]
fn test_migration_rocksdb_paritydb() {
	let ncolumns = 10;
	let src_dir = make_temp_dir();
	let src_db = crate::kvdb::rocksdb::tests::new_test_rocks_db(src_dir.as_path(), ncolumns);

	write_db(&src_db, ncolumns);
	let dst_dir = make_temp_dir();

	{
		let dest_db = crate::kvdb::paritydb::IntrospectorParityDB::new_dumper(&src_db, dst_dir.as_path()).unwrap();
		copy_db(&src_db, &dest_db, ncolumns);
	}

	let dest_db = crate::kvdb::paritydb::IntrospectorParityDB::new(dst_dir.as_path()).unwrap();
	check_db(&dest_db, ncolumns);
	std::fs::remove_dir_all(src_dir).unwrap();
	std::fs::remove_dir_all(dst_dir).unwrap();
}
