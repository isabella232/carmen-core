extern crate carmen_core;
extern crate failure;
extern crate serde;
extern crate serde_json;

use carmen_core::gridstore::*;
use failure::Error;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;

// Util functions for tests and benchmarks

/// Round a float to a number of digits past the decimal point
pub fn round(value: f64, digits: i32) -> f64 {
    let multiplier = 10.0_f64.powi(digits);
    (value * multiplier).round() / multiplier
}

/// Convert an array of language ids into the langfield to use for GridKey or MatchKey
pub fn langarray_to_langfield(array: &[u32]) -> u128 {
    let mut out = 0u128;
    for lang in array {
        out = out | (1 << *lang as usize);
    }
    out
}

/// Mapping of GridKey to all of the grid entries to insert into a store for that GridKey
pub struct StoreEntryBuildingBlock {
    pub grid_key: GridKey,
    pub entries: Vec<GridEntry>,
}

/// Utility to create stores
/// Takes an vector, with each item mapping to a store to create
/// Each item is a vector with maps of grid keys to the entries to insert into the store for that grid key
pub fn create_store(store_entries: Vec<StoreEntryBuildingBlock>) -> GridStore {
    let directory: tempfile::TempDir = tempfile::tempdir().unwrap();
    let mut builder = GridStoreBuilder::new(directory.path()).unwrap();
    for build_block in store_entries {
        builder.insert(&build_block.grid_key, &build_block.entries).expect("Unable to insert");
    }
    builder.finish().unwrap();
    GridStore::new(directory.path()).unwrap()
}

/// Loads json from a file into a Vector of GridEntrys
/// The input file should be a json array of
pub fn load_grids_from_json(path: &Path) -> Result<Vec<GridEntry>, Error> {
    let f = File::open(path).expect("Error opening file");
    let file = io::BufReader::new(f);
    let entries: Vec<GridEntry> = file
        .lines()
        .filter_map(|l| match l.unwrap() {
            ref t if t.len() == 0 => None,
            t => {
                let deserialized: GridEntry =
                    serde_json::from_str(&t).expect("Error deserializing json from string");
                Some(deserialized)
            }
        })
        .collect::<Vec<GridEntry>>();

    Ok(entries)
    // TODO: error handling
}
