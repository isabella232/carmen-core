use std::borrow::Borrow;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use failure::Error;
use itertools::Itertools;
use ordered_float::OrderedFloat;

use crate::gridstore::common::*;
use crate::gridstore::store::GridStore;

/// Takes a vector of phrasematch subqueries (stack) and match options, gets matching grids, sorts the grids,
/// and returns a result of a sorted vector of contexts (lists of grids with added metadata)
pub fn coalesce<T: Borrow<GridStore> + Clone>(
    stack: Vec<PhrasematchSubquery<T>>,
    match_opts: &MatchOpts,
) -> Result<Vec<CoalesceContext>, Error> {
    let contexts = if stack.len() <= 1 {
        coalesce_single(&stack[0], match_opts)?
    } else {
        coalesce_multi(stack, match_opts)?
    };

    let mut out = Vec::with_capacity(MAX_CONTEXTS);
    if !contexts.is_empty() {
        let relev_max = contexts[0].relev;
        let mut sets: HashSet<u64> = HashSet::new();
        for context in contexts {
            if out.len() >= MAX_CONTEXTS {
                break;
            }
            // 0.25 is the smallest allowed relevance
            if relev_max - context.relev >= 0.25 {
                break;
            }
            let inserted = sets.insert(context.entries[0].tmp_id.into());
            if inserted {
                out.push(context);
            }
        }
    }
    Ok(out)
}

fn grid_to_coalesce_entry<T: Borrow<GridStore> + Clone>(
    grid: &MatchEntry,
    subquery: &PhrasematchSubquery<T>,
    match_opts: &MatchOpts,
) -> CoalesceEntry {
    // Zoom has been adjusted in coalesce_multi, or correct zoom has been passed in for coalesce_single
    debug_assert!(match_opts.zoom == subquery.zoom);
    // TODO: do we need to check for bbox here?
    let relev = grid.grid_entry.relev * subquery.weight;

    CoalesceEntry {
        grid_entry: GridEntry { relev, ..grid.grid_entry },
        matches_language: grid.matches_language,
        idx: subquery.idx,
        tmp_id: ((subquery.idx as u32) << 25) + grid.grid_entry.id,
        mask: subquery.mask,
        distance: grid.distance,
        scoredist: grid.scoredist,
    }
}

fn coalesce_single<T: Borrow<GridStore> + Clone>(
    subquery: &PhrasematchSubquery<T>,
    match_opts: &MatchOpts,
) -> Result<Vec<CoalesceContext>, Error> {
    let grids = subquery.store.borrow().get_matching(&subquery.match_key, match_opts)?;
    let mut contexts: Vec<CoalesceContext> = Vec::new();
    let mut max_relev: f64 = 0.;
    // TODO: rename all of the last things to previous things
    let mut last_id: u32 = 0;
    let mut last_relev: f64 = 0.;
    let mut last_scoredist: f64 = 0.;
    let mut min_scoredist = std::f64::MAX;
    let mut feature_count: usize = 0;
    let bigger_max = 2 * MAX_CONTEXTS;

    for grid in grids {
        let coalesce_entry = grid_to_coalesce_entry(&grid, subquery, match_opts);

        // If it's the same feature as the last one, but a lower scoredist don't add it
        if last_id == coalesce_entry.grid_entry.id && coalesce_entry.scoredist <= last_scoredist {
            continue;
        }

        if feature_count > bigger_max {
            if coalesce_entry.scoredist < min_scoredist {
                continue;
            } else if coalesce_entry.grid_entry.relev < last_relev {
                // Grids should be sorted by relevance coming out of get_matching,
                // so if it's lower than the last relevance, stop
                break;
            }
        }

        if max_relev - coalesce_entry.grid_entry.relev >= 0.25 {
            break;
        }
        if coalesce_entry.grid_entry.relev > max_relev {
            max_relev = coalesce_entry.grid_entry.relev;
        }
        // For coalesce single, there is only one coalesce entry per context
        contexts.push(CoalesceContext {
            mask: coalesce_entry.mask,
            relev: coalesce_entry.grid_entry.relev,
            entries: vec![coalesce_entry.clone()],
        });

        if last_id != coalesce_entry.grid_entry.id {
            feature_count += 1;
        }
        if match_opts.proximity.is_none() && feature_count > bigger_max {
            break;
        }
        if coalesce_entry.scoredist < min_scoredist {
            min_scoredist = coalesce_entry.scoredist;
        }
        last_id = coalesce_entry.grid_entry.id;
        last_relev = coalesce_entry.grid_entry.relev;
        last_scoredist = coalesce_entry.scoredist;
    }

    contexts.sort_by_key(|context| {
        (
            Reverse(OrderedFloat(context.relev)),
            Reverse(OrderedFloat(context.entries[0].scoredist)),
            context.entries[0].grid_entry.id,
            context.entries[0].grid_entry.x,
            context.entries[0].grid_entry.y,
        )
    });

    contexts.dedup_by_key(|context| context.entries[0].grid_entry.id);
    contexts.truncate(MAX_CONTEXTS);
    Ok(contexts)
}

fn coalesce_multi<T: Borrow<GridStore> + Clone>(
    mut stack: Vec<PhrasematchSubquery<T>>,
    match_opts: &MatchOpts,
) -> Result<Vec<CoalesceContext>, Error> {
    stack.sort_by_key(|subquery| (subquery.zoom, subquery.idx));

    let mut coalesced: HashMap<(u16, u16, u16), Vec<CoalesceContext>> = HashMap::new();
    let mut contexts: Vec<CoalesceContext> = Vec::new();

    let mut max_relev: f64 = 0.;

    for (i, subquery) in stack.iter().enumerate() {
        let compatible_zooms: Vec<u16> = stack
            .iter()
            .filter_map(|subquery_b| {
                if subquery.idx == subquery_b.idx || subquery.zoom < subquery_b.zoom {
                    None
                } else {
                    Some(subquery_b.zoom)
                }
            })
            .dedup()
            .collect();
        // TODO: check if zooms are equivalent here, and only call adjust_to_zoom if they arent?
        // That way we could avoid a function call and creating a cloned object in the common case where the zooms are the same
        let adjusted_match_opts = match_opts.adjust_to_zoom(subquery.zoom);
        let grids =
            subquery.store.borrow().get_matching(&subquery.match_key, &adjusted_match_opts)?;

        // limit to 100,000 records -- we may want to experiment with this number; it was 500k in
        // carmen-cache, but hopefully we're sorting more intelligently on the way in here so
        // shouldn't need as many records. Still, we should limit it somehow.
        for grid in grids.take(100_000) {
            let coalesce_entry = grid_to_coalesce_entry(&grid, subquery, &adjusted_match_opts);

            let zxy = (subquery.zoom, grid.grid_entry.x, grid.grid_entry.y);

            let mut context_mask = coalesce_entry.mask;
            let mut context_relev = coalesce_entry.grid_entry.relev;
            let mut entries: Vec<CoalesceEntry> = vec![coalesce_entry];

            // See which other zooms are compatible.
            // These should all be lower zooms, so "zoom out" by dividing by 2^(difference in zooms)
            for other_zoom in compatible_zooms.iter() {
                let scale_factor: u16 = 1 << (subquery.zoom - other_zoom);
                let other_zxy = (
                    *other_zoom,
                    entries[0].grid_entry.x / scale_factor,
                    entries[0].grid_entry.y / scale_factor,
                );

                if let Some(already_coalesced) = coalesced.get(&other_zxy) {
                    let mut prev_mask = 0;
                    let mut prev_relev: f64 = 0.;
                    for parent_context in already_coalesced {
                        for parent_entry in &parent_context.entries {
                            // this cover is functionally identical with previous and
                            // is more relevant, replace the previous.
                            if parent_entry.mask == prev_mask
                                && parent_entry.grid_entry.relev > prev_relev
                            {
                                entries.pop();
                                entries.push(parent_entry.clone());
                                // Update the context-level aggregate relev
                                context_relev -= prev_relev;
                                context_relev += parent_entry.grid_entry.relev;

                                prev_mask = parent_entry.mask;
                                prev_relev = parent_entry.grid_entry.relev;
                            } else if context_mask & parent_entry.mask == 0 {
                                entries.push(parent_entry.clone());

                                context_relev += parent_entry.grid_entry.relev;
                                context_mask = context_mask | parent_entry.mask;

                                prev_mask = parent_entry.mask;
                                prev_relev = parent_entry.grid_entry.relev;
                            }
                        }
                    }
                }
            }
            if context_relev > max_relev {
                max_relev = context_relev;
            }

            if i == (stack.len() - 1) {
                if entries.len() == 1 {
                    // Slightly penalize contexts that have no stacking
                    context_relev -= 0.01;
                } else if entries[0].mask > entries[1].mask {
                    // Slightly penalize contexts in ascending order
                    context_relev -= 0.01
                }

                if max_relev - context_relev < 0.25 {
                    contexts.push(CoalesceContext {
                        entries,
                        mask: context_mask,
                        relev: context_relev,
                    });
                }
            } else if i == 0 || entries.len() > 1 {
                if let Some(already_coalesced) = coalesced.get_mut(&zxy) {
                    already_coalesced.push(CoalesceContext {
                        entries,
                        mask: context_mask,
                        relev: context_relev,
                    });
                } else {
                    coalesced.insert(
                        zxy,
                        vec![CoalesceContext { entries, mask: context_mask, relev: context_relev }],
                    );
                }
            }
        }
    }

    for (_, matched) in coalesced {
        for context in matched {
            if max_relev - context.relev < 0.25 {
                contexts.push(context);
            }
        }
    }

    contexts.sort_by_key(|context| {
        (
            Reverse(OrderedFloat(context.relev)),
            Reverse(OrderedFloat(context.entries[0].scoredist)),
            context.entries[0].idx,
            context.entries[0].grid_entry.id,
            context.entries[0].grid_entry.x,
            context.entries[0].grid_entry.y,
        )
    });

    Ok(contexts)
}