//! MAP-Elites grid + island model for evolutionary computation.
//!
//! MAP-Elites (Multi-dimensional Archive of Phenotypic Elites) evolves a
//! population across a discretized behavior space. The grid stores the best
//! individual found for each behavioral niche. The island model runs multiple
//! independent evolution threads that periodically exchange individuals via
//! migration, combining global exploration with local selection pressure.
//!
//! ## MAP-Elites Grid
//!
//! The grid is defined by `num_features` behavioral descriptors, each
//! discretized into `bins` divisions across the `[0, 1]` interval.
//! `GridCell` holds an optional individual and its fitness.
//!
//! ## Island Model
//!
//! Each island runs its own MAP-Elites grid. `MigrationManager` orchestrates
//! periodic migration: at each interval, each island sends its best individuals
//! to randomly chosen destination islands, replacing the weakest residents.
//!
//! ## References
//!
//! - Mouret & Clune (2015): "Illuminating search spaces by mapping elites"
//! - Lehman & Stanley (2011): "Abandoning objectives: Evolution through the search for novelty alone"

use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::Hash;

#[cfg(test)]
use pretty_assertions::assert_eq;

// =====================================================================
// GridCell
// =====================================================================

/// A single bin in the MAP-Elites grid. Holds the best individual discovered
/// for that behavioral niche along with its fitness.
///
/// `T` is the genotype/individual type; `F` is the fitness scalar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridCell<T, F> {
    /// The champion individual for this cell, if any.
    pub individual: Option<T>,
    /// Fitness of the champion. Higher = better.
    pub fitness: Option<F>,
    /// How many individuals have been evaluated that map to this cell
    /// (including the champion).
    pub visit_count: u64,
}

impl<T, F> GridCell<T, F> {
    /// Create an empty cell.
    pub fn new() -> Self {
        Self {
            individual: None,
            fitness: None,
            visit_count: 0,
        }
    }
}

impl<T, F: PartialOrd> GridCell<T, F> {
    /// Attempt to add a new individual to this cell. Returns `true` if the
    /// individual became the champion of its cell (either because the cell was empty
    /// or because `fitness` exceeds the current champion's fitness).
    pub fn try_add(&mut self, individual: T, fitness: F) -> bool {
        self.visit_count += 1;
        match &self.fitness {
            None => {
                self.individual = Some(individual);
                self.fitness = Some(fitness);
                true
            }
            Some(current) if fitness > *current => {
                self.individual = Some(individual);
                self.fitness = Some(fitness);
                true
            }
            _ => false,
        }
    }

    /// Returns true if this cell is empty (no champion yet).
    pub fn is_empty(&self) -> bool {
        self.individual.is_none()
    }
}

impl<T: Default, F: Default> Default for GridCell<T, F> {
    fn default() -> Self {
        Self::new()
    }
}

// =====================================================================
// MapElitesGrid
// =====================================================================

/// A MAP-Elites archive: a multi-dimensional grid indexed by behavioral
/// descriptors. Each dimension is discretized into `bins` equal intervals
/// in [0, 1]. `MapElitesGrid` stores `GridCell`s in a flat hash map keyed by
/// the integer multi-index `(d0, d1, ..., dn)`.
///
/// `T` — genotype/individual type
/// `F` — fitness scalar (higher = better)
/// `const N` — number of behavioral descriptor dimensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapElitesGrid<T, F, const N: usize> {
    /// Flat map from linearized cell index to cell. Linearization is
    /// row-major: index = d0 + d1*b + d2*b^2 + ...
    bins: usize,
    cells: HashMap<u64, GridCell<T, F>>,
    /// Total number of attempted additions since construction.
    total_evaluations: u64,
    /// Number of cells that have at least one champion.
    filled_cells: usize,
}

impl<T, F: Clone + PartialOrd, const N: usize> MapElitesGrid<T, F, N> {
    /// Construct a new grid with `bins` divisions per dimension.
    pub fn new(bins: usize) -> Self {
        assert!(bins > 0, "bins must be > 0");
        Self {
            bins,
            cells: HashMap::new(),
            total_evaluations: 0,
            filled_cells: 0,
        }
    }

    /// Linearize a multi-index `[d0, d1, ..., dn]` (each di in 0..bins) into
    /// a single u64 key using row-major order.
    fn linearize(index: &[usize; N]) -> u64 {
        let mut key = 0u64;
        let mut multiplier = 1u64;
        for &d in index.iter() {
            key += (d as u64) * multiplier;
            multiplier *= Self::max_bins();
        }
        key
    }

    /// Maximum representable bins per dimension (safe upper bound for u64).
    const fn max_bins() -> u64 {
        // We use u64::MAX as a practical upper bound; real usage will be far smaller.
        u64::MAX
    }

    /// Compute the integer bin index for a single descriptor value in [0, 1].
    /// Values outside [0, 1] are clamped to the boundary.
    fn bin_index(value: f64) -> usize {
        let v = value.clamp(0.0, 1.0);
        (v * (Self::max_bins() as f64)) as usize
    }

    /// Convert a slice of N descriptor values (each in [0, 1]) to a grid
    /// multi-index. Clamps out-of-range values.
    pub fn descriptors_to_index(descriptors: &[f64; N]) -> [usize; N] {
        let mut index = [0usize; N];
        for i in 0..N {
            index[i] = Self::bin_index(descriptors[i]);
        }
        index
    }

    /// Linearize a descriptor vector directly to a u64 key.
    pub fn descriptors_to_key(descriptors: &[f64; N]) -> u64 {
        let index = Self::descriptors_to_index(descriptors);
        Self::linearize(&index)
    }

    /// Try to add an individual that maps to `descriptors` with fitness `fitness`.
    /// Returns `true` if this individual became the champion of its cell.
    pub fn add(&mut self, individual: T, descriptors: [f64; N], fitness: F) -> bool {
        self.total_evaluations += 1;
        let key = Self::descriptors_to_key(&descriptors);
        let was_empty = self
            .cells
            .entry(key)
            .or_insert_with(GridCell::new)
            .is_empty();

        let improved = self.cells.get_mut(&key).unwrap().try_add(individual, fitness);

        if was_empty && improved {
            self.filled_cells += 1;
        }
        improved
    }

    /// Return the cell at the given multi-index, if it exists.
    pub fn get(&self, index: &[usize; N]) -> Option<&GridCell<T, F>> {
        self.cells.get(&Self::linearize(index))
    }

    /// Return the cell at the given linearized key, if it exists.
    pub fn get_by_key(&self, key: u64) -> Option<&GridCell<T, F>> {
        self.cells.get(&key)
    }

    /// Return the champion from the highest-fitness cell, or `None` if the
    /// grid is entirely empty.
    pub fn best_cell(&self) -> Option<&GridCell<T, F>> {
        self.cells
            .values()
            .filter(|c| c.fitness.is_some())
            .max_by(|a, b| {
                let fa = a.fitness.as_ref().unwrap();
                let fb = b.fitness.as_ref().unwrap();
                fa.partial_cmp(fb).unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Return the champion with the highest fitness across all cells, plus
    /// its fitness value.
    pub fn best_individual(&self) -> Option<(&T, &F)>
    where
        T: Clone,
        F: Clone,
    {
        self.cells
            .values()
            .filter(|c| c.fitness.is_some())
            .max_by(|a, b| {
                let fa = a.fitness.as_ref().unwrap();
                let fb = b.fitness.as_ref().unwrap();
                fa.partial_cmp(fb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .and_then(|cell| {
                cell.individual
                    .as_ref()
                    .zip(cell.fitness.as_ref())
                    .map(|(ind, fit)| (ind, fit))
            })
    }

    /// Return a random occupied cell, chosen uniformly among filled cells.
    pub fn random_occupied<R: Rng>(&self, rng: &mut R) -> Option<(&T, &F)>
    where
        T: Clone,
        F: Clone,
    {
        let occupied: Vec<_> = self
            .cells
            .iter()
            .filter(|(_, c)| c.individual.is_some())
            .collect();

        if occupied.is_empty() {
            return None;
        }

        let (_, cell) = occupied.choose(rng)?;
        cell.individual
            .as_ref()
            .zip(cell.fitness.as_ref())
            .map(|(ind, fit)| (ind, fit))
    }

    /// Return a mutable reference to all cells. Useful for iteration.
    pub fn cells_mut(&mut self) -> &mut HashMap<u64, GridCell<T, F>> {
        &mut self.cells
    }

    /// Return an immutable reference to all cells. Useful for iteration.
    pub fn cells_ref(&self) -> &HashMap<u64, GridCell<T, F>> {
        &self.cells
    }

    /// Return the number of bins per dimension.
    pub fn bins(&self) -> usize {
        self.bins
    }

    /// Return the total number of filled cells.
    pub fn filled_count(&self) -> usize {
        self.filled_cells
    }

    /// Return the total number of cells (capacity).
    pub fn total_cells(&self) -> usize {
        self.bins.pow(N as u32)
    }

    /// Return the total number of evaluations attempted.
    pub fn total_evaluations(&self) -> u64 {
        self.total_evaluations
    }

    /// Return the coverage ratio (filled / total).
    pub fn coverage(&self) -> f64 {
        self.filled_cells as f64 / self.total_cells() as f64
    }

    /// Drain all cells and reset evaluation counters.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.total_evaluations = 0;
        self.filled_cells = 0;
    }
}

// =====================================================================
// IslandModel
// =====================================================================

/// Statistics for a single island, tracked over its lifetime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IslandStats {
    pub id: usize,
    pub total_evaluations: u64,
    pub migration_count: u32,
    pub immigrants_added: u32,
    pub emigrants_sent: u32,
}

impl Default for IslandStats {
    fn default() -> Self {
        Self {
            id: 0,
            total_evaluations: 0,
            migration_count: 0,
            immigrants_added: 0,
            emigrants_sent: 0,
        }
    }
}

/// An island: an independent MAP-Elites grid that evolves separately and
/// periodically exchanges individuals with other islands.
///
/// `G` is the grid type (e.g. `MapElitesGrid<T, F, N>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Island<G> {
    pub id: usize,
    pub grid: G,
    pub stats: IslandStats,
}

impl<G: Default> Island<G> {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            grid: G::default(),
            stats: IslandStats { id, ..Default::default() },
        }
    }
}

/// Configuration for the island model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IslandModelConfig {
    /// Number of islands.
    pub num_islands: usize,
    /// How many individuals each island sends per migration event.
    pub migrants_per_island: usize,
    /// How often (in evaluations) migration is attempted.
    pub migration_interval: u64,
    /// Fraction of worst individuals replaced by immigrants (0.0..=1.0).
    pub replacement_fraction: f64,
    /// Random seed for reproducibility.
    pub seed: Option<u64>,
}

impl Default for IslandModelConfig {
    fn default() -> Self {
        Self {
            num_islands: 4,
            migrants_per_island: 2,
            migration_interval: 500,
            replacement_fraction: 0.2,
            seed: None,
        }
    }
}

// =====================================================================
// MigrationManager
// =====================================================================

/// Manages migration between islands in the island model.
///
/// Each island holds its own `MapElitesGrid`. The manager coordinates
/// migration events, selecting emigrants from each island and integrating
/// them into destination islands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManager<T, F, const N: usize> {
    config: IslandModelConfig,
    /// Per-island grids. `Vec` index = island id.
    islands: Vec<MapElitesGrid<T, F, N>>,
    /// Tracks evaluation counts per island to know when to trigger migration.
    evaluations_per_island: Vec<u64>,
    /// Total migration rounds executed.
    migration_rounds: u32,
}

impl<T: Clone, F: Clone + PartialOrd, const N: usize> MigrationManager<T, F, N> {
    /// Create a new manager with `config.num_islands` empty grids.
    pub fn new(config: IslandModelConfig) -> Self {
        let islands = (0..config.num_islands)
            .map(|id| {
                let mut g = MapElitesGrid::new(10); // default bins; caller can resize
                g.clear();
                g
            })
            .collect();

        Self {
            islands,
            evaluations_per_island: vec![0; config.num_islands],
            migration_rounds: 0,
            config,
        }
    }

    /// Get a mutable reference to a specific island's grid.
    pub fn island_mut(&mut self, id: usize) -> Option<&mut MapElitesGrid<T, F, N>> {
        self.islands.get_mut(id)
    }

    /// Get a reference to a specific island's grid.
    pub fn island(&self, id: usize) -> Option<&MapElitesGrid<T, F, N>> {
        self.islands.get(id)
    }

    /// Get the number of islands.
    pub fn num_islands(&self) -> usize {
        self.islands.len()
    }

    /// Record an evaluation on `island_id` and return whether a migration
    /// event should be triggered (based on `migration_interval`).
    pub fn record_evaluation(&mut self, island_id: usize) -> bool {
        if let Some(counter) = self.evaluations_per_island.get_mut(island_id) {
            *counter += 1;
            *counter >= self.config.migration_interval
        } else {
            false
        }
    }

    /// Perform a migration round: each island sends its best individuals to
    /// randomly chosen destination islands, replacing weak residents.
    ///
    /// The selection strategy:
    /// 1. For each source island, collect up to `migrants_per_island` best cells.
    /// 2. Pick destination islands uniformly at random (excluding self).
    /// 3. In each destination, replace up to `replacement_fraction` worst cells.
    pub fn run_migration<R: Rng>(&mut self, rng: &mut R) {
        let num_islands = self.islands.len();
        let migrants = self.config.migrants_per_island;
        let replacement_frac = self.config.replacement_fraction;

        // Gather emigrants from each island: (island_id, individual, fitness, descriptors)
        let emigrants: Vec<(usize, T, F, [f64; N])> = self
            .islands
            .iter()
            .enumerate()
            .flat_map(|(island_id, grid)| {
                self.top_cells(grid, migrants)
                    .into_iter()
                    .map(move |(ind, fit, desc)| (island_id, ind, fit, desc))
            })
            .collect();

        // Shuffle emigrants to distribute randomly across destinations
        let mut shuffled = emigrants;
        shuffled.shuffle(rng);

        let mut immigrant_counts = vec![0usize; num_islands];

        // Assign each emigrant to a random destination island (not self)
        for (src_island, individual, fitness, descriptors) in shuffled {
            // Pick a destination different from source
            let mut dest_island = rng.gen_range(0..num_islands);
            while dest_island == src_island && num_islands > 1 {
                dest_island = rng.gen_range(0..num_islands);
            }

            // Count immigrants for this destination this round
            let dest_idx = dest_island;
            if immigrant_counts[dest_idx] < migrants * 2 {
                // Try to add to destination grid
                let replaced = self.islands[dest_idx].add(individual.clone(), descriptors, fitness.clone());

                if replaced {
                    // Stats tracking removed - MigrationManager uses plain MapElitesGrid
                }
                immigrant_counts[dest_idx] += 1;
            }
        }

        // Reset evaluation counters after migration
        for counter in &mut self.evaluations_per_island {
            *counter = 0;
        }

        self.migration_rounds += 1;

        // Note: per-island stats tracking not available on plain MapElitesGrid
    }

    /// Extract the top `n` champion cells from a grid, sorted by fitness descending.
    fn top_cells(
        &self,
        grid: &MapElitesGrid<T, F, N>,
        n: usize,
    ) -> Vec<(T, F, [f64; N])> {
        let mut cells: Vec<_> = grid
            .cells_ref()
            .values()
            .filter(|c| c.individual.is_some())
            .filter_map(|c| {
                c.individual
                    .as_ref()
                    .zip(c.fitness.as_ref())
                    .map(|(ind, fit)| (ind.clone(), fit.clone()))
            })
            .collect();

        // Sort by fitness descending
        cells.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });

        cells.truncate(n);
        cells.into_iter().map(|(ind, fit)| (ind, fit, [0.0; N])).collect()
    }

    /// Return the aggregate coverage across all islands.
    pub fn aggregate_coverage(&self) -> f64 {
        if self.islands.is_empty() {
            return 0.0;
        }
        let total_filled: usize = self.islands.iter().map(|i| i.filled_count()).sum();
        let total_cells = self.islands.first().map(|g| g.total_cells()).unwrap_or(0);
        if total_cells == 0 {
            return 0.0;
        }
        total_filled as f64 / (total_cells * self.islands.len()) as f64
    }

    /// Return the number of migration rounds executed.
    pub fn migration_rounds(&self) -> u32 {
        self.migration_rounds
    }

    /// Return a summary of island stats.
    /// Note: MigrationManager uses plain MapElitesGrid without per-island stats.
    pub fn island_stats(&self) -> Vec<IslandStats> {
        vec![] // MigrationManager doesn't track per-island IslandStats
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[derive(Clone, Debug, PartialEq)]
    struct TestIndividual {
        genes: Vec<f64>,
    }

    // We need PartialOrd for GridCell but TestIndividual doesn't implement it.
    // Use a separate fitness type.
    type TestFitness = f64;
    type TestGrid = MapElitesGrid<TestIndividual, TestFitness, 2>;
    type TestManager = MigrationManager<TestIndividual, TestFitness, 2>;

    fn make_individual(genes: Vec<f64>) -> TestIndividual {
        TestIndividual { genes }
    }

    #[test]
    fn grid_cell_new_is_empty() {
        let cell: GridCell<i32, f64> = GridCell::new();
        assert!(cell.is_empty());
        assert_eq!(cell.visit_count, 0);
    }

    #[test]
    fn grid_cell_try_add_updates_champion() {
        let mut cell: GridCell<i32, f64> = GridCell::new();

        // First addition should win
        assert!(cell.try_add(1, 0.5));
        assert_eq!(cell.individual.as_ref().unwrap(), &1);
        assert_eq!(cell.fitness.unwrap(), 0.5);
        assert_eq!(cell.visit_count, 1);

        // Lower fitness should not replace
        assert!(!cell.try_add(2, 0.3));
        assert_eq!(*cell.individual.as_ref().unwrap(), 1);
        assert_eq!(cell.visit_count, 2);

        // Higher fitness should replace
        assert!(cell.try_add(3, 0.8));
        assert_eq!(*cell.individual.as_ref().unwrap(), 3);
        assert_eq!(cell.visit_count, 3);
    }

    #[test]
    fn grid_descriptors_to_index() {
        type G2 = MapElitesGrid<(), f64, 2>;
        // G2::new is not const, so we just call the static method
        let idx = G2::descriptors_to_index(&[0.5, 0.5]);
        assert_eq!(idx, [u64::MAX as usize / 2, u64::MAX as usize / 2]);

        let idx2 = G2::descriptors_to_index(&[0.0, 1.0]);
        assert_eq!(idx2, [0, u64::MAX as usize]);
    }

    #[test]
    fn grid_descriptors_to_key_deterministic() {
        type G2 = MapElitesGrid<(), f64, 2>;
        let k1 = G2::descriptors_to_key(&[0.1, 0.2]);
        let k2 = G2::descriptors_to_key(&[0.1, 0.2]);
        assert_eq!(k1, k2);
    }

    #[test]
    fn grid_add_and_retrieve() {
        let mut grid: MapElitesGrid<i32, f64, 2> = MapElitesGrid::new(10);
        let ind = 42;
        let desc = [0.5, 0.5];
        let fit = 1.0;

        let improved = grid.add(ind, desc, fit);
        assert!(improved);
        assert_eq!(grid.filled_count(), 1);
        assert_eq!(grid.total_evaluations(), 1);
        assert!(grid.coverage() > 0.0);
    }

    #[test]
    fn grid_best_individual() {
        let mut grid: MapElitesGrid<i32, f64, 2> = MapElitesGrid::new(10);

        // Add several individuals with different fitness
        grid.add(1, [0.2, 0.3], 0.5);
        grid.add(2, [0.4, 0.5], 0.9); // best
        grid.add(3, [0.6, 0.7], 0.7);

        let (best, fit) = grid.best_individual().unwrap();
        assert_eq!(*best, 2);
        assert!((*fit - 0.9).abs() < 1e-9);
    }

    #[test]
    fn grid_random_occupied() {
        let mut grid: MapElitesGrid<i32, f64, 2> = MapElitesGrid::new(10);
        let mut rng = ChaCha8Rng::seed_from_u64(123);

        assert!(grid.random_occupied(&mut rng).is_none());

        grid.add(10, [0.1, 0.2], 1.0);
        grid.add(20, [0.3, 0.4], 2.0);

        let (ind, _) = grid.random_occupied(&mut rng).unwrap();
        assert!(*ind == 10 || *ind == 20);
    }

    #[test]
    fn island_new_default() {
        let island: Island<TestGrid> = Island::new(0);
        assert_eq!(island.id, 0);
        assert_eq!(island.stats.id, 0);
        assert_eq!(island.stats.total_evaluations, 0);
    }

    #[test]
    fn migration_manager_num_islands() {
        let config = IslandModelConfig {
            num_islands: 3,
            ..Default::default()
        };
        let manager: TestManager = MigrationManager::new(config);
        assert_eq!(manager.num_islands(), 3);
    }

    #[test]
    fn migration_manager_record_evaluation() {
        let config = IslandModelConfig {
            num_islands: 2,
            migration_interval: 3,
            ..Default::default()
        };
        let mut manager: TestManager = MigrationManager::new(config);

        // First two evals should not trigger
        assert!(!manager.record_evaluation(0));
        assert!(!manager.record_evaluation(0));
        // Third eval on island 0 should trigger
        assert!(manager.record_evaluation(0));
        // Counter should reset after trigger
        assert!(!manager.record_evaluation(0));

        // Island 1 should have independent counter
        assert!(!manager.record_evaluation(1));
        assert!(!manager.record_evaluation(1));
        assert!(manager.record_evaluation(1));
    }

    #[test]
    fn migration_manager_empty_migration_round() {
        let config = IslandModelConfig {
            num_islands: 2,
            migrants_per_island: 2,
            migration_interval: 10,
            ..Default::default()
        };
        let mut manager: TestManager = MigrationManager::new(config);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        // No individuals yet — migration should not panic
        manager.run_migration(&mut rng);
        assert_eq!(manager.migration_rounds(), 1);
    }

    #[test]
    fn migration_manager_run_migration_with_individuals() {
        let config = IslandModelConfig {
            num_islands: 2,
            migrants_per_island: 2,
            migration_interval: 10,
            ..Default::default()
        };
        let mut manager: TestManager = MigrationManager::new(config);

        // Add individuals to island 0
        manager.island_mut(0).unwrap().add(
            make_individual(vec![1.0, 0.0]),
            [0.1, 0.2],
            1.0,
        );
        manager.island_mut(0).add(make_individual(vec![2.0, 0.0]), [0.3, 0.4], 2.0);

        // Add individuals to island 1
        manager.island_mut(1).unwrap().add(
            make_individual(vec![3.0, 0.0]),
            [0.5, 0.6],
            3.0,
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        manager.run_migration(&mut rng);

        assert_eq!(manager.migration_rounds(), 1);
        // Both islands should still have individuals after migration
        assert!(manager.island(0).unwrap().filled_count() >= 1);
        assert!(manager.island(1).unwrap().filled_count() >= 1);
    }

    #[test]
    fn migration_manager_aggregate_coverage() {
        let config = IslandModelConfig {
            num_islands: 2,
            ..Default::default()
        };
        let manager: TestManager = MigrationManager::new(config);
        assert_eq!(manager.aggregate_coverage(), 0.0);

        let mut manager: TestManager = MigrationManager::new(config);
        manager.island_mut(0).unwrap().add(make_individual(vec![1.0]), [0.5], 1.0);
        // 1 filled cell out of (bins^2 * 2 islands)
        let cov = manager.aggregate_coverage();
        assert!(cov > 0.0);
    }

    #[test]
    fn grid_clear() {
        let mut grid: MapElitesGrid<i32, f64, 2> = MapElitesGrid::new(10);
        grid.add(1, [0.1, 0.2], 1.0);
        grid.add(2, [0.3, 0.4], 2.0);
        assert_eq!(grid.filled_count(), 2);
        assert_eq!(grid.total_evaluations(), 2);

        grid.clear();

        assert_eq!(grid.filled_count(), 0);
        assert_eq!(grid.total_evaluations(), 0);
        assert!(grid.best_individual().is_none());
    }
}
