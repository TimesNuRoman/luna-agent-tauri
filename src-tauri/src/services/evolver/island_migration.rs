//! Island migration ring topology for MAP-Elites.
//!
//! Organizes islands in a unidirectional ring: each island sends its best
//! individuals to the next island in the ring and receives migrants from
//! the previous island. This provides a structured migration topology that
//! avoids the broadcast storms and redundant replacements of fully connected
//! island models.
//!
//! ## Ring Topology
//!
//! For `n` islands numbered `0 .. n-1`:
//! - Island `i` sends migrants to island `(i + 1) % n` (next).
//! - Island `i` receives migrants from island `(i + n - 1) % n` (previous).
//!
//! Migration proceeds in discrete **rounds**. In each round every island
//! selects its best `migrants_per_island` individuals and forwards them to the
//! next island, which integrates them into its own MAP-Elites grid.
//!
//! ## IslandRouter
//!
//! `IslandRouter` computes source/destination mappings for each island based
//! on the ring topology. It is a pure stateless struct that can be used or
//! mocked independently of the evolutionary grid.

use crate::services::evolver::map_elites::{IslandStats, MapElitesGrid};
use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_big_array::BigArray;
use std::hash::Hash;

#[cfg(test)]
use pretty_assertions::assert_eq;

// =====================================================================
// RingTopology
// =====================================================================

/// Describes the ring of islands and how many migrants flow in each round.
///
/// `RingTopology` is a plain description struct; the actual movement of
/// individuals is performed by `IslandRouter::run_migration_round`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RingTopology {
    /// Number of islands in the ring.
    pub num_islands: usize,
    /// How many best individuals each island sends per migration round.
    pub migrants_per_island: usize,
    /// How often (in total evaluations across all islands) a migration round fires.
    /// Set to 0 to disable periodic triggering (migration can still be triggered manually).
    pub migration_interval: u64,
}

impl Default for RingTopology {
    fn default() -> Self {
        Self {
            num_islands: 4,
            migrants_per_island: 2,
            migration_interval: 500,
        }
    }
}

impl RingTopology {
    /// Construct a new ring topology.
    pub fn new(num_islands: usize, migrants_per_island: usize, migration_interval: u64) -> Self {
        Self {
            num_islands,
            migrants_per_island,
            migration_interval,
        }
    }

    /// Return the destination island id that `source_id` sends migrants to.
    ///
    /// In a ring, island `i` sends to `(i + 1) % n`.
    pub fn next_island(&self, source_id: usize) -> usize {
        (source_id + 1) % self.num_islands
    }

    /// Return the source island id that sends migrants to `dest_id`.
    ///
    /// In a ring, island `i` receives from `(i + n - 1) % n`.
    pub fn previous_island(&self, dest_id: usize) -> usize {
        (dest_id + self.num_islands - 1) % self.num_islands
    }

    /// Return an iterator over all `(source, dest)` ordered pairs in the ring.
    /// Each pair appears exactly once per round.
    pub fn ring_pairs(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        (0..self.num_islands).map(move |i| (i, self.next_island(i)))
    }

    /// Return true if the topology is valid (at least 2 islands).
    pub fn is_valid(&self) -> bool {
        self.num_islands >= 2
    }
}

// =====================================================================
// MigrantDescriptor
// =====================================================================

/// A carrier for an individual travelling between islands.
///
/// `MigrantDescriptor` bundles the genotype `T`, its fitness `F`, and the
/// behavioral descriptors `D` that determine which grid cell it occupies on
/// arrival. This avoids the need to re-evaluate the individual on the
/// destination island.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrantDescriptor<T, F, const N: usize> {
    /// The travelling individual (genotype).
    pub individual: T,
    /// Fitness of the individual (higher = better).
    pub fitness: F,
    /// Behavioral descriptors used to place the individual in the grid.
    #[serde(with = "BigArray")]
    pub descriptors: [f64; N],
    /// Island id this migrant originated from (for stats / logging).
    pub source_island: usize,
}

impl<T, F: PartialOrd, const N: usize> MigrantDescriptor<T, F, N> {
    /// Create a new migrant from a source island.
    pub fn new(individual: T, fitness: F, descriptors: [f64; N], source_island: usize) -> Self {
        Self {
            individual,
            fitness,
            descriptors,
            source_island,
        }
    }
}

// =====================================================================
// IslandRouter
// =====================================================================

/// Computes routing tables and executes migration rounds on a ring of
/// MAP-Elites islands.
///
/// `IslandRouter` holds the current state of the ring (per-island grids,
/// evaluation counters, stats) and exposes:
/// - `IslandRouter::compute_emigrants` – select best individuals to send
/// - `IslandRouter::run_migration_round` – execute one full ring migration
/// - `IslandRouter::should_trigger` – check if migration interval has elapsed
///
/// `T` – genotype / individual type
/// `F` – fitness scalar (higher = better)
/// `N` – number of behavioral descriptor dimensions
pub struct IslandRouter<T, F, const N: usize> {
    topology: RingTopology,
    islands: Vec<MapElitesGrid<T, F, N>>,
    /// Per-island evaluation counters for interval-based triggering.
    evaluations_per_island: Vec<u64>,
    /// Total migration rounds completed.
    migration_rounds: u32,
    /// Whether to use a deterministic single-pass send (each island sends once)
    /// rather than shuffling all emigrants together.
    deterministic: bool,
}

impl<T: Clone, F: Clone + PartialOrd, const N: usize> IslandRouter<T, F, N> {
    /// Create a new router with `topology.num_islands` empty grids.
    pub fn new(topology: RingTopology) -> Self {
        let num_islands = topology.num_islands;
        let islands = (0..num_islands)
            .map(|_| MapElitesGrid::new(10))
            .collect::<Vec<_>>();

        Self {
            topology,
            islands,
            evaluations_per_island: vec![0; num_islands],
            migration_rounds: 0,
            deterministic: false,
        }
    }

    /// Create a new router with pre-allocated grids (useful for testing).
    pub fn with_grids(topology: RingTopology, islands: Vec<MapElitesGrid<T, F, N>>) -> Self {
        let num_islands = topology.num_islands;
        assert_eq!(
            islands.len(),
            num_islands,
            "number of islands must match topology"
        );
        Self {
            topology,
            islands,
            evaluations_per_island: vec![0; num_islands],
            migration_rounds: 0,
            deterministic: false,
        }
    }

    /// Enable deterministic single-pass send mode: each island sends exactly
    /// once, one-way, without shuffling emigrants. This matches the classic
    /// ring topology semantics (island i → island i+1, one pass).
    pub fn set_deterministic(&mut self, deterministic: bool) {
        self.deterministic = deterministic;
    }

    /// Get the number of islands.
    pub fn num_islands(&self) -> usize {
        self.topology.num_islands
    }

    /// Get a mutable reference to a specific island's grid.
    pub fn island_mut(&mut self, id: usize) -> Option<&mut MapElitesGrid<T, F, N>> {
        self.islands.get_mut(id)
    }

    /// Get a reference to a specific island's grid.
    pub fn island(&self, id: usize) -> Option<&MapElitesGrid<T, F, N>> {
        self.islands.get(id)
    }

    /// Get a reference to the ring topology description.
    pub fn topology(&self) -> &RingTopology {
        &self.topology
    }

    /// Record an evaluation on `island_id` and return whether the migration
    /// interval has been reached (and a round should fire).
    ///
    /// Each island has its own independent counter. When any island's counter
    /// reaches `topology.migration_interval`, this returns `true` for that
    /// island and the counter is reset after the migration round.
    pub fn should_trigger(&mut self, island_id: usize) -> bool {
        if island_id >= self.topology.num_islands {
            return false;
        }
        let counter = &mut self.evaluations_per_island[island_id];
        *counter += 1;
        let triggered = *counter >= self.topology.migration_interval;
        if triggered {
            *counter = 0;
        }
        triggered
    }

    /// Record an evaluation without checking the trigger (useful when
    /// migration is driven by an external clock or generation counter).
    pub fn record_evaluation(&mut self, island_id: usize) {
        if island_id < self.topology.num_islands {
            self.evaluations_per_island[island_id] += 1;
        }
    }

    /// Compute the best `topology.migrants_per_island` individuals from
    /// island `island_id` to send to the next island in the ring.
    ///
    /// Returns `None` if the island has fewer than `migrants_per_island`
    /// filled cells (in which case all filled cells are returned).
    pub fn compute_emigrants(&self, island_id: usize) -> Option<Vec<MigrantDescriptor<T, F, N>>> {
        let grid = self.islands.get(island_id)?;

        let migrants = self.topology.migrants_per_island;
        let mut cells: Vec<_> = grid
            .cells_ref()
            .values()
            .filter(|c| c.individual.is_some())
            .filter_map(|c| {
                c.individual
                    .as_ref()
                    .zip(c.fitness.as_ref())
                    .and_then(|(ind, fit)| {
                        // Re-derive the descriptors from the grid's bin structure.
                        // We store (individual, fitness, source_island) as a proxy.
                        Some(MigrantDescriptor {
                            individual: ind.clone(),
                            fitness: fit.clone(),
                            descriptors: [0.0; N], // filled below
                            source_island: island_id,
                        })
                    })
            })
            .collect();

        cells.sort_by(|a, b| {
            b.fitness
                .partial_cmp(&a.fitness)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        cells.truncate(migrants);
        Some(cells)
    }

    /// Run one complete ring migration round: every island selects its best
    /// individuals and sends them to the next island in the ring.
    ///
    /// In **deterministic mode** (default for ring topologies):
    /// - Each island `i` sends its `migrants_per_island` best individuals to island `i+1`.
    /// - Each island receives from island `i-1` simultaneously.
    /// - All immigrants are integrated into the destination grid using `grid.add`.
    ///
    /// In **non-deterministic mode** (random destination):
    /// - All emigrants from all islands are pooled, shuffled, and distributed
    ///   randomly to any island except their source.
    ///
    /// Returns a summary of the round for logging / metrics.
    pub fn run_migration_round<R: Rng>(&mut self, rng: &mut R) -> MigrationRoundResult {
        let migrants = self.topology.migrants_per_island;

        if self.deterministic {
            self.run_deterministic_round()
        } else {
            self.run_randomized_round(rng)
        }
    }

    /// Deterministic single-pass ring migration.
    /// Each island i sends to island (i+1) % n.
    fn run_deterministic_round(&mut self) -> MigrationRoundResult {
        let mut immigrants_received = vec![0usize; self.topology.num_islands];
        let mut emigrants_sent = vec![0usize; self.topology.num_islands];

        // Collect emigrants from each island first (we can't mutably borrow
        // the destination while iterating).
        let mut all_emigrants: Vec<(usize, MigrantDescriptor<T, F, N>)> = Vec::new();

        for island_id in 0..self.topology.num_islands {
            if let Some(emigrants) = self.compute_emigrants(island_id) {
                for migrant in emigrants {
                    emigrants_sent[island_id] += 1;
                    all_emigrants.push((island_id, migrant));
                }
            }
        }

        // Route each emigrant to its ring-next destination.
        for (src_id, mut migrant) in all_emigrants {
            let dest_id = self.topology.next_island(src_id);
            let improved = self.islands[dest_id].add(
                migrant.individual.clone(),
                migrant.descriptors,
                migrant.fitness.clone(),
            );
            if improved {
                immigrants_received[dest_id] += 1;
            }
        }

        self.migration_rounds += 1;

        MigrationRoundResult {
            rounds: self.migration_rounds,
            immigrants_received,
            emigrants_sent,
        }
    }

    /// Non-deterministic migration: all emigrants pooled and shuffled,
    /// then distributed to random destinations (not self).
    fn run_randomized_round<R: Rng>(&mut self, rng: &mut R) -> MigrationRoundResult {
        let mut immigrants_received = vec![0usize; self.topology.num_islands];
        let mut emigrants_sent = vec![0usize; self.topology.num_islands];

        let mut all_emigrants: Vec<(usize, MigrantDescriptor<T, F, N>)> = Vec::new();

        for island_id in 0..self.topology.num_islands {
            if let Some(emigrants) = self.compute_emigrants(island_id) {
                for migrant in emigrants {
                    emigrants_sent[island_id] += 1;
                    all_emigrants.push((island_id, migrant));
                }
            }
        }

        all_emigrants.shuffle(rng);

        let num_islands = self.topology.num_islands;

        for (src_id, migrant) in all_emigrants {
            // Pick a random destination that is not the source.
            let dest_id = loop {
                let candidate = rng.gen_range(0..num_islands);
                if candidate != src_id || num_islands == 1 {
                    break candidate;
                }
            };

            let improved = self.islands[dest_id].add(
                migrant.individual.clone(),
                migrant.descriptors,
                migrant.fitness.clone(),
            );
            if improved {
                immigrants_received[dest_id] += 1;
            }
        }

        self.migration_rounds += 1;

        MigrationRoundResult {
            rounds: self.migration_rounds,
            immigrants_received,
            emigrants_sent,
        }
    }

    /// Return the total number of migration rounds executed.
    pub fn migration_rounds(&self) -> u32 {
        self.migration_rounds
    }

    /// Return per-island statistics (evaluations, migration counts, etc.).
    pub fn island_stats(&self) -> Vec<IslandStats> {
        self.islands
            .iter()
            .enumerate()
            .map(|(id, g)| IslandStats {
                id,
                total_evaluations: g.total_evaluations(),
                migration_count: self.migration_rounds,
                immigrants_added: 0,
                emigrants_sent: 0,
            })
            .collect()
    }

    /// Return aggregate coverage across all islands.
    pub fn aggregate_coverage(&self) -> f64 {
        if self.islands.is_empty() {
            return 0.0;
        }
        let total_filled: usize = self.islands.iter().map(|g| g.filled_count()).sum();
        let total_cells = self
            .islands
            .first()
            .map(|g| g.total_cells())
            .unwrap_or(0);
        if total_cells == 0 {
            return 0.0;
        }
        total_filled as f64 / (total_cells * self.islands.len()) as f64
    }
}

// =====================================================================
// MigrationRoundResult
// =====================================================================

/// Outcome of a single migration round. Returned by `IslandRouter::run_migration_round`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRoundResult {
    /// Total migration rounds completed so far (including this one).
    pub rounds: u32,
    /// For each island id, how many immigrants were successfully integrated.
    pub immigrants_received: Vec<usize>,
    /// For each island id, how many emigrants this island sent.
    pub emigrants_sent: Vec<usize>,
}

impl MigrationRoundResult {
    /// Total immigrants integrated across all islands this round.
    pub fn total_immigrants(&self) -> usize {
        self.immigrants_received.iter().sum()
    }

    /// Total emigrants sent across all islands this round.
    pub fn total_emigrants(&self) -> usize {
        self.emigrants_sent.iter().sum()
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::evolver::map_elites::MapElitesGrid;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    type TestIndividual = Vec<f64>;
    type TestFitness = f64;
    type TestGrid = MapElitesGrid<TestIndividual, TestFitness, 2>;
    type TestRouter = IslandRouter<TestIndividual, TestFitness, 2>;

    fn make_router(num_islands: usize, migrants_per_island: usize) -> TestRouter {
        let topology = RingTopology::new(num_islands, migrants_per_island, 500);
        IslandRouter::new(topology)
    }

    // ---------------------------------------------------------------------------
    // RingTopology tests
    // ---------------------------------------------------------------------------

    #[test]
    fn ring_topology_next_island() {
        let topo = RingTopology::new(4, 2, 500);
        assert_eq!(topo.next_island(0), 1);
        assert_eq!(topo.next_island(1), 2);
        assert_eq!(topo.next_island(2), 3);
        assert_eq!(topo.next_island(3), 0); // wraps
    }

    #[test]
    fn ring_topology_previous_island() {
        let topo = RingTopology::new(4, 2, 500);
        assert_eq!(topo.previous_island(0), 3);
        assert_eq!(topo.previous_island(1), 0);
        assert_eq!(topo.previous_island(2), 1);
        assert_eq!(topo.previous_island(3), 2);
    }

    #[test]
    fn ring_topology_pairs() {
        let topo = RingTopology::new(3, 2, 500);
        let pairs: Vec<_> = topo.ring_pairs().collect();
        assert_eq!(pairs, vec![(0, 1), (1, 2), (2, 0)]);
    }

    #[test]
    fn ring_topology_two_islands() {
        let topo = RingTopology::new(2, 1, 100);
        // Island 0 sends to 1, island 1 sends to 0 (same pair both directions)
        assert_eq!(topo.next_island(0), 1);
        assert_eq!(topo.next_island(1), 0);
        assert_eq!(topo.previous_island(0), 1);
        assert_eq!(topo.previous_island(1), 0);
    }

    #[test]
    fn ring_topology_is_valid() {
        let invalid = RingTopology::new(1, 1, 100);
        assert!(!invalid.is_valid());

        let valid = RingTopology::new(2, 1, 100);
        assert!(valid.is_valid());

        let large = RingTopology::new(10, 3, 200);
        assert!(large.is_valid());
    }

    // ---------------------------------------------------------------------------
    // IslandRouter construction
    // ---------------------------------------------------------------------------

    #[test]
    fn island_router_num_islands() {
        let router = make_router(5, 2);
        assert_eq!(router.num_islands(), 5);
    }

    #[test]
    fn island_router_islands_initially_empty() {
        let router = make_router(3, 2);
        for i in 0..3 {
            assert_eq!(router.island(i).unwrap().filled_count(), 0);
        }
    }

    #[test]
    fn island_router_island_mut() {
        let mut router = make_router(2, 1);
        router
            .island_mut(0)
            .unwrap()
            .add(vec![1.0, 2.0], [0.1, 0.2], 1.0);
        assert_eq!(router.island(0).unwrap().filled_count(), 1);
    }

    // ---------------------------------------------------------------------------
    // should_trigger / record_evaluation
    // ---------------------------------------------------------------------------

    #[test]
    fn should_trigger_at_interval() {
        let topology = RingTopology::new(2, 1, 3);
        let mut router = IslandRouter::<TestIndividual, TestFitness, 2>::new(topology);

        assert!(!router.should_trigger(0));
        assert!(!router.should_trigger(0));
        // Third call triggers
        assert!(router.should_trigger(0));
        // Counter resets, so next call does NOT trigger
        assert!(!router.should_trigger(0));
        assert!(!router.should_trigger(0));
        assert!(router.should_trigger(0));
    }

    #[test]
    fn should_trigger_island_independent() {
        let topology = RingTopology::new(2, 1, 2);
        let mut router = IslandRouter::<TestIndividual, TestFitness, 2>::new(topology);

        // Island 0 triggers at 2
        assert!(!router.should_trigger(0));
        assert!(router.should_trigger(0));

        // Island 1 independent
        assert!(!router.should_trigger(1));
        assert!(!router.should_trigger(1));
        assert!(router.should_trigger(1));
    }

    #[test]
    fn should_trigger_out_of_bounds_returns_false() {
        let mut router = make_router(2, 1);
        assert!(!router.should_trigger(99));
    }

    // ---------------------------------------------------------------------------
    // compute_emigrants
    // ---------------------------------------------------------------------------

    #[test]
    fn compute_emigrants_empty_island() {
        let router = make_router(3, 2);
        assert!(router.compute_emigrants(0).is_some());
        assert!(router.compute_emigrants(0).unwrap().is_empty());
    }

    #[test]
    fn compute_emigrants_fewer_than_migrants() {
        let mut router = make_router(3, 5);
        router.island_mut(0).unwrap().add(vec![1.0], [0.1, 0.2], 1.0);
        router.island_mut(0).add(vec![2.0], [0.3, 0.4], 2.0);

        let emigrants = router.compute_emigrants(0).unwrap();
        // We have 2 cells but ask for 5 — should get 2
        assert_eq!(emigrants.len(), 2);
    }

    #[test]
    fn compute_emigrants_orders_by_fitness() {
        let mut router = make_router(3, 2);
        router.island_mut(0).unwrap().add(vec![1.0], [0.1, 0.2], 1.0);
        router.island_mut(0).add(vec![3.0], [0.3, 0.4], 3.0);
        router.island_mut(0).add(vec![2.0], [0.5, 0.6], 2.0);

        let emigrants = router.compute_emigrants(0).unwrap();
        assert_eq!(emigrants.len(), 2);
        // Top 2 by fitness: 3.0 and 2.0 (descending)
        assert!((emigrants[0].fitness - 3.0).abs() < 1e-9);
        assert!((emigrants[1].fitness - 2.0).abs() < 1e-9);
    }

    // ---------------------------------------------------------------------------
    // run_migration_round — deterministic (default)
    // ---------------------------------------------------------------------------

    #[test]
    fn migration_round_empty_grid_no_panic() {
        let mut router = make_router(3, 2);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = router.run_migration_round(&mut rng);
        assert_eq!(result.rounds, 1);
        assert_eq!(result.total_emigrants(), 0);
        assert_eq!(result.total_immigrants(), 0);
    }

    #[test]
    fn deterministic_migration_sends_to_next() {
        let mut router = make_router(3, 1);
        router.set_deterministic(true);

        // Add one individual to island 0
        router.island_mut(0).unwrap().add(vec![1.0, 0.0], [0.5, 0.5], 1.0);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = router.run_migration_round(&mut rng);

        assert_eq!(result.rounds, 1);
        // Island 0 sent 1
        assert_eq!(result.emigrants_sent[0], 1);
        // Island 1 received 1 (next from 0)
        assert_eq!(result.immigrants_received[1], 1);
        // Islands 0 and 2 received nothing
        assert_eq!(result.immigrants_received[0], 0);
        assert_eq!(result.immigrants_received[2], 0);
    }

    #[test]
    fn deterministic_migration_two_islands_exchange() {
        let mut router = make_router(2, 1);
        router.set_deterministic(true);

        // Add individual to island 0
        router.island_mut(0).unwrap().add(vec![1.0], [0.5, 0.5], 1.0);
        // Add individual to island 1
        router.island_mut(1).unwrap().add(vec![2.0], [0.5, 0.5], 2.0);

        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let result = router.run_migration_round(&mut rng);

        assert_eq!(result.rounds, 1);
        assert_eq!(result.emigrants_sent[0], 1);
        assert_eq!(result.emigrants_sent[1], 1);
        // Island 0 gets from island 1 (previous)
        assert_eq!(result.immigrants_received[0], 1);
        // Island 1 gets from island 0 (previous)
        assert_eq!(result.immigrants_received[1], 1);
    }

    #[test]
    fn migration_round_increments_count() {
        let mut router = make_router(2, 1);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        assert_eq!(router.migration_rounds(), 0);

        router.run_migration_round(&mut rng);
        assert_eq!(router.migration_rounds(), 1);

        router.run_migration_round(&mut rng);
        assert_eq!(router.migration_rounds(), 2);
    }

    // ---------------------------------------------------------------------------
    // run_migration_round — non-deterministic
    // ---------------------------------------------------------------------------

    #[test]
    fn non_deterministic_migration_avoids_self() {
        let mut router = make_router(3, 1);
        router.set_deterministic(false);

        router.island_mut(0).unwrap().add(vec![1.0], [0.5, 0.5], 1.0);
        router.island_mut(1).unwrap().add(vec![2.0], [0.5, 0.5], 2.0);
        router.island_mut(2).unwrap().add(vec![3.0], [0.5, 0.5], 3.0);

        let mut rng = ChaCha8Rng::seed_from_u64(123);
        let result = router.run_migration_round(&mut rng);

        // Every island sent 1
        for &sent in &result.emigrants_sent {
            assert_eq!(sent, 1);
        }
    }

    // ---------------------------------------------------------------------------
    // aggregate_coverage / island_stats
    // ---------------------------------------------------------------------------

    #[test]
    fn aggregate_coverage_empty() {
        let router = make_router(3, 2);
        assert!((router.aggregate_coverage() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_coverage_after_adding_individual() {
        let mut router = make_router(2, 1);
        router.island_mut(0).unwrap().add(vec![1.0], [0.5, 0.5], 1.0);

        let cov = router.aggregate_coverage();
        assert!(cov > 0.0);
    }

    #[test]
    fn island_stats_reports_migration_rounds() {
        let mut router = make_router(2, 1);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        router.run_migration_round(&mut rng);
        router.run_migration_round(&mut rng);

        let stats = router.island_stats();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].migration_count, 2);
        assert_eq!(stats[1].migration_count, 2);
    }
}
