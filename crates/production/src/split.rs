//! Splitting one dough into batches by mixer capacity (F14a, ADR 0042). Pure: no I/O.
//!
//! A batch holds whole pieces. The split takes the fewest batches for which dealing the
//! pieces fits: pieces go out heaviest first, each to the batch with the least dough so
//! far (the earlier batch on a tie). No batch then exceeds the capacity, and two batches
//! differ by at most the dough of the heaviest piece.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use domain::ProductId;

/// Pieces of one product still to plan, with the dough one piece needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Demand {
    pub product_id: ProductId,
    pub grams_per_piece: u32,
    pub pieces: u32,
}

/// One planned batch: pieces per product, by product id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub outputs: Vec<(ProductId, u32)>,
    pub grams: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SplitError {
    /// One piece of this product needs more dough than the mixer holds.
    #[error("one piece of product {0:?} needs more dough than the mixer holds")]
    PieceTooHeavy(ProductId),
}

/// Splits the demands of one dough into batches of at most `capacity_g` grams of dough.
/// Demands without pieces or without dough weight are left out.
///
/// # Errors
/// [`SplitError::PieceTooHeavy`] when a single piece needs more than `capacity_g`.
pub fn split(demands: &[Demand], capacity_g: u32) -> Result<Vec<Planned>, SplitError> {
    let mut pieces: Vec<Demand> = demands
        .iter()
        .copied()
        .filter(|d| d.pieces > 0 && d.grams_per_piece > 0)
        .collect();
    if let Some(heavy) = pieces.iter().find(|d| d.grams_per_piece > capacity_g) {
        return Err(SplitError::PieceTooHeavy(heavy.product_id));
    }
    pieces.sort_by_key(|d| (Reverse(d.grams_per_piece), d.product_id));
    let total: u64 = pieces
        .iter()
        .map(|d| u64::from(d.grams_per_piece) * u64::from(d.pieces))
        .sum();
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut count = usize::try_from(total.div_ceil(u64::from(capacity_g))).unwrap_or(usize::MAX);
    loop {
        let planned = deal(&pieces, count);
        if planned.iter().all(|b| b.grams <= u64::from(capacity_g)) {
            return Ok(planned);
        }
        count += 1;
    }
}

fn deal(pieces: &[Demand], count: usize) -> Vec<Planned> {
    let mut loads: Vec<u64> = vec![0; count];
    let mut outputs: Vec<BTreeMap<ProductId, u32>> = vec![BTreeMap::new(); count];
    let mut lightest: BinaryHeap<Reverse<(u64, usize)>> =
        (0..count).map(|index| Reverse((0, index))).collect();
    for demand in pieces {
        for _ in 0..demand.pieces {
            let Some(Reverse((load, index))) = lightest.pop() else {
                return Vec::new();
            };
            let load = load + u64::from(demand.grams_per_piece);
            loads[index] = load;
            *outputs[index].entry(demand.product_id).or_default() += 1;
            lightest.push(Reverse((load, index)));
        }
    }
    loads
        .into_iter()
        .zip(outputs)
        .filter(|(grams, _)| *grams > 0)
        .map(|(grams, outputs)| Planned {
            outputs: outputs.into_iter().collect(),
            grams,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn demand(product: i64, grams_per_piece: u32, pieces: u32) -> Demand {
        Demand {
            product_id: ProductId::new(product),
            grams_per_piece,
            pieces,
        }
    }

    #[test]
    fn one_product_splits_into_even_batches() {
        let planned = split(&[demand(1, 900, 30)], 10_000).expect("split");
        let counts: Vec<u32> = planned.iter().map(|b| b.outputs[0].1).collect();
        assert_eq!(counts, vec![10, 10, 10]);
        assert!(planned.iter().all(|b| b.grams == 9_000));
    }

    #[test]
    fn the_first_batches_take_the_odd_pieces() {
        let planned = split(&[demand(1, 1_000, 25)], 10_000).expect("split");
        let counts: Vec<u32> = planned.iter().map(|b| b.outputs[0].1).collect();
        assert_eq!(counts, vec![9, 8, 8]);
    }

    #[test]
    fn a_dough_that_fits_is_one_batch() {
        let planned = split(&[demand(2, 500, 4), demand(1, 900, 3)], 10_000).expect("split");
        assert_eq!(
            planned,
            vec![Planned {
                outputs: vec![(ProductId::new(1), 3), (ProductId::new(2), 4)],
                grams: 4_700,
            }]
        );
    }

    #[test]
    fn whole_pieces_can_need_an_extra_batch() {
        // 3 × 6 kg is 18 kg, but two 10 kg batches hold only one 6 kg piece each.
        let planned = split(&[demand(1, 6_000, 3)], 10_000).expect("split");
        assert_eq!(planned.len(), 3);
    }

    #[test]
    fn nothing_to_bake_is_no_batch() {
        assert_eq!(split(&[], 10_000), Ok(Vec::new()));
        assert_eq!(
            split(&[demand(1, 900, 0), demand(2, 0, 5)], 10_000),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_piece_heavier_than_the_mixer_is_refused() {
        assert_eq!(
            split(&[demand(1, 900, 2), demand(7, 12_000, 1)], 10_000),
            Err(SplitError::PieceTooHeavy(ProductId::new(7)))
        );
    }

    fn demands() -> impl Strategy<Value = Vec<Demand>> {
        prop::collection::vec((1..=5_000_u32, 0..=60_u32), 1..=5).prop_map(|rows| {
            rows.into_iter()
                .enumerate()
                .map(|(i, (grams, pieces))| {
                    demand(i64::try_from(i).unwrap_or(0) + 1, grams, pieces)
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn batches_add_up_to_the_dough_and_respect_the_mixer(
            demands in demands(),
            capacity in 5_000..=40_000_u32,
        ) {
            let planned = split(&demands, capacity).expect("every piece fits");
            let needed: u64 = demands
                .iter()
                .map(|d| u64::from(d.grams_per_piece) * u64::from(d.pieces))
                .sum();
            prop_assert_eq!(planned.iter().map(|b| b.grams).sum::<u64>(), needed);
            for batch in &planned {
                prop_assert!(batch.grams <= u64::from(capacity));
                let grams: u64 = batch
                    .outputs
                    .iter()
                    .map(|(id, n)| {
                        let d = demands.iter().find(|d| d.product_id == *id).expect("known");
                        u64::from(d.grams_per_piece) * u64::from(*n)
                    })
                    .sum();
                prop_assert_eq!(grams, batch.grams);
            }
            for d in &demands {
                let pieces: u32 = planned
                    .iter()
                    .flat_map(|b| &b.outputs)
                    .filter(|(id, _)| *id == d.product_id)
                    .map(|(_, n)| *n)
                    .sum();
                prop_assert_eq!(pieces, d.pieces);
            }
        }

        #[test]
        fn batches_differ_by_at_most_the_heaviest_piece(
            demands in demands(),
            capacity in 5_000..=40_000_u32,
        ) {
            let planned = split(&demands, capacity).expect("every piece fits");
            let heaviest = demands
                .iter()
                .filter(|d| d.pieces > 0)
                .map(|d| u64::from(d.grams_per_piece))
                .max()
                .unwrap_or(0);
            if let (Some(max), Some(min)) = (
                planned.iter().map(|b| b.grams).max(),
                planned.iter().map(|b| b.grams).min(),
            ) {
                prop_assert!(max - min <= heaviest, "{max} - {min} > {heaviest}");
            }
        }

        #[test]
        fn one_product_takes_the_fewest_batches(
            grams in 1..=5_000_u32,
            pieces in 1..=400_u32,
            capacity in 5_000..=40_000_u32,
        ) {
            let planned = split(&[demand(1, grams, pieces)], capacity).expect("fits");
            let per_batch = capacity / grams;
            let fewest = pieces.div_ceil(per_batch);
            prop_assert_eq!(planned.len(), usize::try_from(fewest).unwrap_or(0));
            let counts: Vec<u32> = planned.iter().map(|b| b.outputs[0].1).collect();
            prop_assert!(counts.iter().max().unwrap_or(&0) - counts.iter().min().unwrap_or(&0) <= 1);
        }
    }
}
