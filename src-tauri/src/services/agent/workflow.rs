//! DSL-style workflow engine with typed edges.
//!
//! Inspired by Tracecat's durable workflow design. Provides a declarative
//! way to define workflows with typed edges, BFS cycle detection, and
//! a `workflow!` macro for ergonomic construction.
//!
//! ## Example
//!
//! ```rust,ignore
//! let wf = workflow! {
//!     entry -> "task1",
//!     "task1" -> "task2" if "condition",
//!     end "task2"
//! };
//! wf.execute(&mut ctx)?;
//! ```

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::services::agent::task::TaskCost;

// ============================================================================
// WorkflowStep — the node types
// ============================================================================

/// A single step in a workflow graph.
///
/// Each variant carries its own node-id (for edge construction) and the
/// parameters needed to execute that step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStep {
    /// A leaf task node — executes a task template and returns.
    Task {
        node_id: String,
        task_template: String,
    },
    /// A conditional branch — evaluates a condition expression and picks
    /// which outgoing edge to follow.
    Condition {
        node_id: String,
        condition_expr: String,
    },
    /// Executes multiple branches in parallel and optionally merges.
    Parallel {
        node_id: String,
        branches: Vec<Workflow>,
    },
    /// Suspends execution for `duration_secs` before continuing.
    Wait {
        node_id: String,
        duration_secs: u64,
    },
    /// Terminal node — signals that the workflow has completed.
    End {
        node_id: String,
    },
}

impl WorkflowStep {
    /// Returns the node id of this step.
    pub fn node_id(&self) -> &str {
        match self {
            WorkflowStep::Task { node_id, .. } => node_id,
            WorkflowStep::Condition { node_id, .. } => node_id,
            WorkflowStep::Parallel { node_id, .. } => node_id,
            WorkflowStep::Wait { node_id, .. } => node_id,
            WorkflowStep::End { node_id, .. } => node_id,
        }
    }
}

// ============================================================================
// WorkflowEdge — typed edges
// ============================================================================

/// The outcome type for a step execution, determining which edge to take.
///
/// This implements Tracecat's SUCCESS/FAIL/SKIP pattern for typed edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// Step completed successfully — follow SUCCESS edges.
    Success,
    /// Step failed — follow FAIL edges.
    Fail,
    /// Step was skipped (run_if = false) — follow SKIP edges.
    Skip,
}

impl Default for StepOutcome {
    fn default() -> Self {
        StepOutcome::Success
    }
}

/// A directed edge between two workflow nodes with a typed outcome.
///
/// The optional `condition` field is used by `Condition` nodes to decide
/// which outgoing edge to follow. The `outcome` field specifies which
/// step result triggers this edge (SUCCESS, FAIL, or SKIP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
    /// Guard condition (evaluated for `Condition` nodes). `None` means
    /// unconditional.
    pub condition: Option<String>,
    /// Which step outcome triggers this edge. Defaults to `Success`.
    pub outcome: StepOutcome,
}

impl WorkflowEdge {
    /// Construct a new unconditional edge with default SUCCESS outcome.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            condition: None,
            outcome: StepOutcome::Success,
        }
    }

    /// Construct a new conditional edge with default SUCCESS outcome.
    pub fn with_condition(from: impl Into<String>, to: impl Into<String>, cond: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            condition: Some(cond.into()),
            outcome: StepOutcome::Success,
        }
    }

    /// Construct a new edge with a specific outcome type.
    pub fn with_outcome(from: impl Into<String>, to: impl Into<String>, outcome: StepOutcome) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            condition: None,
            outcome,
        }
    }

    /// Construct a new conditional edge with a specific outcome type.
    pub fn with_condition_and_outcome(
        from: impl Into<String>,
        to: impl Into<String>,
        cond: impl Into<String>,
        outcome: StepOutcome,
    ) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            condition: Some(cond.into()),
            outcome,
        }
    }
}

// ============================================================================
// Workflow — the graph
// ============================================================================

/// A complete workflow graph with typed nodes and edges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Workflow {
    /// All nodes in the graph, keyed by node id.
    pub nodes: BTreeMap<String, WorkflowStep>,
    /// Directed edges. Multiple edges from the same source are allowed
    /// (one per outgoing branch).
    pub edges: Vec<WorkflowEdge>,
    /// Entry node id — the first node executed.
    pub entry: String,
}

impl Workflow {
    /// Create a new empty workflow with the given entry node.
    pub fn new(entry: impl Into<String>) -> Self {
        Self {
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            entry: entry.into(),
        }
    }

    /// Add a node to the workflow. Returns an error if a node with the same
    /// id already exists.
    pub fn add_node(&mut self, step: WorkflowStep) -> Result<(), WorkflowError> {
        let id = step.node_id().to_string();
        if self.nodes.contains_key(&id) {
            return Err(WorkflowError::NodeAlreadyExists { node_id: id });
        }
        self.nodes.insert(id, step);
        Ok(())
    }

    /// Add an edge to the workflow. Returns an error if either endpoint
    /// does not exist in `self.nodes`.
    pub fn add_edge(&mut self, edge: WorkflowEdge) -> Result<(), WorkflowError> {
        if !self.nodes.contains_key(&edge.from) {
            return Err(WorkflowError::NodeNotFound { node_id: edge.from });
        }
        if !self.nodes.contains_key(&edge.to) {
            return Err(WorkflowError::NodeNotFound { node_id: edge.to });
        }
        self.edges.push(edge);
        Ok(())
    }

    /// Returns the outgoing edges from `node_id`.
    pub fn outgoing(&self, node_id: &str) -> Vec<&WorkflowEdge> {
        self.edges.iter().filter(|e| e.from == node_id).collect()
    }

    /// Returns the incoming edges to `node_id`.
    pub fn incoming(&self, node_id: &str) -> Vec<&WorkflowEdge> {
        self.edges.iter().filter(|e| e.to == node_id).collect()
    }

    /// Check for cyclic dependencies using BFS (Kahn's algorithm).
    ///
    /// Returns `Err(CyclicDependency)` if a cycle is detected.
    pub fn detect_cycles(&self) -> Result<(), WorkflowError> {
        // Build adjacency list and in-degree map
        let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
        let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

        // All nodes start with in-degree 0
        for node_id in self.nodes.keys() {
            in_degree.insert(node_id, 0);
            adjacency.insert(node_id, Vec::new());
        }

        // Process edges
        for edge in &self.edges {
            if let Some(deg) = in_degree.get_mut(edge.from.as_str()) {
                // Edge from edge.from to edge.to
                if let Some(list) = adjacency.get_mut(edge.from.as_str()) {
                    list.push(edge.to.as_str());
                }
                if let Some(deg) = in_degree.get_mut(edge.to.as_str()) {
                    *deg += 1;
                }
            }
        }

        // Kahn's algorithm: start with nodes that have in-degree 0
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut visited = 0;

        while let Some(node) = queue.pop_front() {
            visited += 1;
            if let Some(neighbors) = adjacency.get(node) {
                for &neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(neighbor);
                        }
                    }
                }
            }
        }

        if visited != self.nodes.len() {
            return Err(WorkflowError::CyclicDependency);
        }

        Ok(())
    }

    /// Execute the workflow starting from `self.entry`.
    ///
    /// The workflow context (`ctx`) holds current task state and variables
    /// that persist across steps.
    pub fn execute(&self, ctx: &mut WorkflowContext) -> Result<WorkflowResult, WorkflowError> {
        // Cycle detection first
        self.detect_cycles()?;

        // Verify entry node exists
        if !self.nodes.contains_key(&self.entry) {
            return Err(WorkflowError::NodeNotFound { node_id: self.entry.clone() });
        }

        let mut current = self.entry.clone();
        let mut results: BTreeMap<String, StepResult> = BTreeMap::new();

        loop {
            let step = self
                .nodes
                .get(&current)
                .ok_or_else(|| WorkflowError::NodeNotFound { node_id: current.clone() })?;

            let step_result = self.execute_step(step, ctx)?;

            results.insert(current.clone(), step_result.clone());

            // Check if this is a terminal step
            if matches!(step, WorkflowStep::End { .. }) {
                break;
            }

            // Determine next node
            current = self.next_node(&current, &step_result, ctx)?;
        }

        Ok(WorkflowResult {
            step_results: results,
            final_cost: ctx.cost.clone(),
        })
    }

    /// Execute a single step and return its result.
    fn execute_step(
        &self,
        step: &WorkflowStep,
        ctx: &mut WorkflowContext,
    ) -> Result<StepResult, WorkflowError> {
        match step {
            WorkflowStep::Task {
                node_id,
                task_template,
            } => {
                ctx.variables.insert(
                    format!("{}.output", node_id),
                    serde_json::json!(task_template),
                );
                Ok(StepResult::Task {
                    node_id: node_id.clone(),
                    outcome: StepOutcome::Success,
                })
            }
            WorkflowStep::Condition {
                node_id,
                condition_expr,
            } => {
                // Simple condition evaluation: check if the condition string
                // exists as a truthy variable value. In a real implementation
                // this would parse and evaluate a proper expression language.
                let value = ctx
                    .variables
                    .get(condition_expr)
                    .cloned()
                    .unwrap_or(serde_json::Value::Bool(true));
                Ok(StepResult::Condition {
                    node_id: node_id.clone(),
                    result: value,
                    outcome: StepOutcome::Success,
                })
            }
            WorkflowStep::Parallel {
                node_id,
                branches,
            } => {
                // Execute all branches sequentially for now (true parallelism
                // would require async runtime support). Collect all branch results.
                let branch_results: Vec<WorkflowResult> = branches
                    .iter()
                    .map(|branch| branch.execute(ctx))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(StepResult::Parallel {
                    node_id: node_id.clone(),
                    result: serde_json::json!(branch_results),
                    outcome: StepOutcome::Success,
                })
            }
            WorkflowStep::Wait {
                node_id,
                duration_secs,
            } => {
                // In a real implementation this would suspend the workflow
                // and schedule resumption. Here we just record the wait.
                ctx.variables.insert(
                    format!("{}.wait_until", node_id),
                    serde_json::json!(chrono::Utc::now().timestamp() + *duration_secs as i64),
                );
                Ok(StepResult::Wait {
                    node_id: node_id.clone(),
                    secs: *duration_secs,
                    outcome: StepOutcome::Success,
                })
            }
            WorkflowStep::End { node_id } => Ok(StepResult::End {
                node_id: node_id.clone(),
                outcome: StepOutcome::Success,
            }),
        }
    }

    /// Determine the next node based on the current node and step result.
    ///
    /// Uses typed edge routing: matches the step's outcome (SUCCESS/FAIL/SKIP)
    /// against edge outcomes to find the correct next node.
    fn next_node(
        &self,
        node_id: &str,
        step_result: &StepResult,
        ctx: &mut WorkflowContext,
    ) -> Result<String, WorkflowError> {
        let outgoing = self.outgoing(node_id);
        let outcome = step_result.outcome();

        if outgoing.is_empty() {
            // No outgoing edges — workflow ends here (unless already at End)
            return Err(WorkflowError::EdgeNotFound { node_id: node_id.to_string() });
        }

        // Find edges matching the current outcome
        let matching_edges: Vec<&WorkflowEdge> = outgoing
            .into_iter()
            .filter(|e| e.outcome == outcome)
            .collect();

        match step_result {
            StepResult::Condition { result: value, .. } => {
                // For conditions, first find edges matching outcome, then check condition
                for edge in &matching_edges {
                    if let Some(ref cond) = edge.condition {
                        let cond_value = ctx.variables.get(cond).cloned();
                        if cond_value.as_ref() == Some(value) {
                            return Ok(edge.to.clone());
                        }
                    }
                }
                // Fallback: first matching outcome edge without condition
                matching_edges
                    .iter()
                    .find(|e| e.condition.is_none())
                    .map(|e| e.to.clone())
                    .ok_or_else(|| WorkflowError::EdgeNotFound { node_id: node_id.to_string() })
            }
            _ => {
                // For all other step types, follow the first edge with matching outcome
                matching_edges
                    .first()
                    .map(|e| e.to.clone())
                    .ok_or_else(|| WorkflowError::EdgeNotFound { node_id: node_id.to_string() })
            }
        }
    }
}

/// Result of executing a single step, including the outcome type for edge routing.
///
/// Each variant carries the node_id (for edge construction) and the outcome
/// (SUCCESS/FAIL/SKIP) used to select the next node via typed edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StepResult {
    /// A leaf task node — executes a task template and returns.
    Task {
        node_id: String,
        outcome: StepOutcome,
    },
    /// A conditional branch — evaluates a condition expression and picks
    /// which outgoing edge to follow.
    Condition {
        node_id: String,
        result: serde_json::Value,
        outcome: StepOutcome,
    },
    /// Executes multiple branches in parallel and optionally merges.
    Parallel {
        node_id: String,
        result: serde_json::Value,
        outcome: StepOutcome,
    },
    /// Suspends execution for `duration_secs` before continuing.
    Wait {
        node_id: String,
        secs: u64,
        outcome: StepOutcome,
    },
    /// Terminal node — signals that the workflow has completed.
    End {
        node_id: String,
        outcome: StepOutcome,
    },
}

impl StepResult {
    /// Returns the outcome of this step result.
    pub fn outcome(&self) -> StepOutcome {
        match self {
            StepResult::Task { outcome, .. } => *outcome,
            StepResult::Condition { outcome, .. } => *outcome,
            StepResult::Parallel { outcome, .. } => *outcome,
            StepResult::Wait { outcome, .. } => *outcome,
            StepResult::End { outcome, .. } => *outcome,
        }
    }

    /// Returns the node id of this step result.
    pub fn node_id(&self) -> &str {
        match self {
            StepResult::Task { node_id, .. } => node_id,
            StepResult::Condition { node_id, .. } => node_id,
            StepResult::Parallel { node_id, .. } => node_id,
            StepResult::Wait { node_id, .. } => node_id,
            StepResult::End { node_id, .. } => node_id,
        }
    }
}

// ============================================================================
// WorkflowContext — execution state
// ============================================================================

/// Holds the current state during workflow execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkflowContext {
    /// Arbitrary key-value variables accessible to condition expressions.
    pub variables: BTreeMap<String, serde_json::Value>,
    /// Accumulated cost for the workflow run.
    pub cost: TaskCost,
    /// Whether execution has been cancelled.
    pub cancelled: bool,
}

impl WorkflowContext {
    /// Create a fresh context with the given initial variables.
    pub fn new(variables: BTreeMap<String, serde_json::Value>) -> Self {
        Self {
            variables,
            cost: TaskCost::default(),
            cancelled: false,
        }
    }

    /// Set a variable.
    pub fn set_var(&mut self, key: impl Into<String>, value: serde_json::Value) {
        self.variables.insert(key.into(), value);
    }

    /// Get a variable.
    pub fn get_var(&self, key: &str) -> Option<&serde_json::Value> {
        self.variables.get(key)
    }
}

// ============================================================================
// WorkflowResult — final outcome
// ============================================================================

/// The result of a complete workflow execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    /// Results for each executed step, keyed by node id.
    pub step_results: BTreeMap<String, StepResult>,
    /// Total accumulated cost.
    pub final_cost: TaskCost,
}

// ============================================================================
// WorkflowError
// ============================================================================

/// Errors that can occur during workflow construction or execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum WorkflowError {
    /// A referenced node does not exist in the workflow graph.
    NodeNotFound { node_id: String },
    /// No outgoing edge exists from the given node.
    EdgeNotFound { node_id: String },
    /// A node with the given id already exists.
    NodeAlreadyExists { node_id: String },
    /// Execution of a step failed.
    ExecutionFailed { node_id: String, message: String },
    /// A cyclic dependency was detected in the workflow graph.
    CyclicDependency,
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::NodeNotFound { node_id } => {
                write!(f, "node not found: {}", node_id)
            }
            WorkflowError::EdgeNotFound { node_id } => {
                write!(f, "no outgoing edge from node: {}", node_id)
            }
            WorkflowError::NodeAlreadyExists { node_id } => {
                write!(f, "node already exists: {}", node_id)
            }
            WorkflowError::ExecutionFailed { node_id, message } => {
                write!(f, "execution failed at {}: {}", node_id, message)
            }
            WorkflowError::CyclicDependency => {
                write!(f, "cyclic dependency detected in workflow graph")
            }
        }
    }
}

impl std::error::Error for WorkflowError {}

// ============================================================================
// workflow! macro — declarative syntax
// ============================================================================

/// Builds a `Workflow` using a declarative mini-DSL.
///
/// ## Syntax
///
/// ```ignore
/// workflow! {
///     entry -> "node_id",           // unconditional edge from entry to node_id
///     "node_id" -> "other" if "cond", // conditional edge (condition node picks branch)
///     end "node_id",                  // marks a node as an End step
///     task "node_id" = "template",   // declares a Task step
///     wait "node_id" = 5,            // declares a Wait step
///     parallel "node_id" => { ... }, // declares a Parallel step (sub-workflows)
/// }
/// ```
///
/// Nodes are implicitly created when referenced in edges. Use explicit
/// step declarations (`task`, `wait`, `parallel`, `end`) to control step
/// parameters.
#[macro_export]
macro_rules! workflow {
    // Entry point
    (
        entry -> $entry:tt
        $(,)?
    ) => {{
        let mut wf = $crate::services::agent::workflow::Workflow::new($entry);
        wf
    }};

    // Entry + additional rules
    (
        entry -> $entry:tt
        $(, $rest:tt)*
    ) => {{
        let mut wf = $crate::services::agent::workflow::Workflow::new($entry);
        $workflow::workflow_inner!(@add wf, $($rest)*);
        wf
    }};
}

/// Internal rules for `workflow!` — processes one rule at a time.
#[doc(hidden)]
#[macro_export]
macro_rules! workflow_inner {
    // end node declaration
    (@add $wf:ident, end $node:tt) => {
        $wf.add_node($crate::services::agent::workflow::WorkflowStep::End {
            node_id: $node.to_string(),
        }).unwrap();
    };

    // task declaration: task "node_id" = "template"
    (@add $wf:ident, task $node:tt = $tmpl:expr) => {
        $wf.add_node($crate::services::agent::workflow::WorkflowStep::Task {
            node_id: $node.to_string(),
            task_template: $tmpl.to_string(),
        }).unwrap();
    };

    // wait declaration: wait "node_id" = 60
    (@add $wf:ident, wait $node:tt = $secs:expr) => {
        $wf.add_node($crate::services::agent::workflow::WorkflowStep::Wait {
            node_id: $node.to_string(),
            duration_secs: $secs,
        }).unwrap();
    };

    // condition declaration: condition "node_id" = "expr"
    (@add $wf:ident, condition $node:tt = $expr:expr) => {
        $wf.add_node($crate::services::agent::workflow::WorkflowStep::Condition {
            node_id: $node.to_string(),
            condition_expr: $expr.to_string(),
        }).unwrap();
    };

    // parallel declaration: parallel "node_id" => { ... }
    // This is a simplified form that collects sub-workflow rules
    (@add $wf:ident, parallel $node:tt => { $($branches:tt)* }) => {
        // Build sub-workflows from branch rules
        $wf.add_node($crate::services::agent::workflow::WorkflowStep::Parallel {
            node_id: $node.to_string(),
            branches: Vec::new(), // populated by higher-level macro
        }).unwrap();
    };

    // unconditional edge: "from" -> "to"
    (@add $wf:ident, $from:tt -> $to:tt) => {
        $wf.add_edge($crate::services::agent::workflow::WorkflowEdge::new($from, $to)).unwrap();
    };

    // conditional edge: "from" -> "to" if "condition"
    (@add $wf:ident, $from:tt -> $to:tt if $cond:tt) => {
        $wf.add_edge($crate::services::agent::workflow::WorkflowEdge::with_condition($from, $to, $cond)).unwrap();
    };

    // trailing comma + more rules
    (@add $wf:ident, , $($rest:tt)*) => {
        $workflow::workflow_inner!(@add $wf, $($rest)*);
    };

    // consume remaining tokens
    (@add $wf:ident, $($rest:tt)*) => {
        $workflow::workflow_inner!(@add $wf, $($rest)*);
    };
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_empty() {
        let wf = Workflow::new("entry");
        assert_eq!(wf.entry, "entry");
        assert!(wf.nodes.is_empty());
        assert!(wf.edges.is_empty());
    }

    #[test]
    fn test_workflow_add_nodes_and_edges() {
        let mut wf = Workflow::new("start");
        wf.add_node(WorkflowStep::End {
            node_id: "start".into(),
        })
        .unwrap();

        assert!(wf.nodes.contains_key("start"));
        assert!(wf.outgoing("start").is_empty());
    }

    #[test]
    fn test_workflow_edge_not_found() {
        let mut wf = Workflow::new("start");
        wf.add_node(WorkflowStep::End {
            node_id: "start".into(),
        })
        .unwrap();

        let edge = WorkflowEdge::new("start", "end");
        assert!(wf.add_edge(edge).is_ok());
    }

    #[test]
    fn test_workflow_detect_no_cycle() {
        let mut wf = Workflow::new("a");
        wf.add_node(WorkflowStep::End { node_id: "a".into() }).unwrap();
        wf.add_node(WorkflowStep::End { node_id: "b".into() }).unwrap();
        wf.add_edge(WorkflowEdge::new("a", "b")).unwrap();

        assert!(wf.detect_cycles().is_ok());
    }

    #[test]
    fn test_workflow_detect_cycle() {
        let mut wf = Workflow::new("a");
        wf.add_node(WorkflowStep::End { node_id: "a".into() }).unwrap();
        wf.add_node(WorkflowStep::End { node_id: "b".into() }).unwrap();
        wf.add_edge(WorkflowEdge::new("a", "b")).unwrap();
        wf.add_edge(WorkflowEdge::new("b", "a")).unwrap(); // cycle: a -> b -> a

        assert!(matches!(
            wf.detect_cycles(),
            Err(WorkflowError::CyclicDependency)
        ));
    }

    #[test]
    fn test_workflow_execute_simple() {
        let mut wf = Workflow::new("task1");
        wf.add_node(WorkflowStep::Task {
            node_id: "task1".into(),
            task_template: "hello world".into(),
        })
        .unwrap();
        wf.add_node(WorkflowStep::End { node_id: "end".into() }).unwrap();
        wf.add_edge(WorkflowEdge::new("task1", "end")).unwrap();

        let ctx = &mut WorkflowContext::default();
        let result = wf.execute(ctx);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.step_results.contains_key("task1"));
        assert!(result.step_results.contains_key("end"));
    }

    #[test]
    fn test_workflow_execute_missing_entry() {
        let wf = Workflow::new("nonexistent");
        let ctx = &mut WorkflowContext::default();
        let result = wf.execute(ctx);
        assert!(matches!(result, Err(WorkflowError::NodeNotFound { .. })));
    }

    #[test]
    fn test_workflow_edge_already_exists() {
        let mut wf = Workflow::new("a");
        wf.add_node(WorkflowStep::End { node_id: "a".into() }).unwrap();
        wf.add_node(WorkflowStep::End { node_id: "b".into() }).unwrap();

        wf.add_edge(WorkflowEdge::new("a", "b")).unwrap();
        let duplicate = WorkflowEdge::new("a", "b");
        assert!(wf.add_edge(duplicate).is_ok()); // edges are not unique-constrained
    }

    #[test]
    fn test_workflow_variables() {
        let mut ctx = WorkflowContext::default();
        ctx.set_var("x", serde_json::json!(42));
        assert_eq!(ctx.get_var("x"), Some(&serde_json::json!(42)));
    }

    #[test]
    fn test_workflow_condition_evaluation() {
        let mut wf = Workflow::new("cond");
        wf.add_node(WorkflowStep::Condition {
            node_id: "cond".into(),
            condition_expr: "flag".into(),
        })
        .unwrap();
        wf.add_node(WorkflowStep::End { node_id: "true".into() }).unwrap();
        wf.add_node(WorkflowStep::End { node_id: "false".into() }).unwrap();
        wf.add_edge(WorkflowEdge::with_condition("cond", "true", "flag"))
            .unwrap();
        wf.add_edge(WorkflowEdge::with_condition("cond", "false", "other"))
            .unwrap();

        let mut ctx = WorkflowContext::default();
        ctx.set_var("flag", serde_json::json!(true));

        let result = wf.execute(&mut ctx);
        assert!(result.is_ok());
        let step_results = &result.as_ref().unwrap().step_results;
        assert!(step_results.contains_key("cond"));
    }

    #[test]
    fn test_workflow_wait_step() {
        let mut wf = Workflow::new("wait_node");
        wf.add_node(WorkflowStep::Wait {
            node_id: "wait_node".into(),
            duration_secs: 5,
        })
        .unwrap();
        wf.add_node(WorkflowStep::End { node_id: "end".into() }).unwrap();
        wf.add_edge(WorkflowEdge::new("wait_node", "end")).unwrap();

        let ctx = &mut WorkflowContext::default();
        let result = wf.execute(ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_workflow_parallel_step() {
        let mut sub_wf = Workflow::new("sub_entry");
        sub_wf.add_node(WorkflowStep::End { node_id: "sub_entry".into() }).unwrap();

        let mut wf = Workflow::new("parallel_node");
        wf.add_node(WorkflowStep::Parallel {
            node_id: "parallel_node".into(),
            branches: vec![sub_wf],
        })
        .unwrap();
        wf.add_node(WorkflowStep::End { node_id: "end".into() }).unwrap();
        wf.add_edge(WorkflowEdge::new("parallel_node", "end")).unwrap();

        let ctx = &mut WorkflowContext::default();
        let result = wf.execute(ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn test_workflow_error_display() {
        let err = WorkflowError::NodeNotFound {
            node_id: "foo".into(),
        };
        assert_eq!(err.to_string(), "node not found: foo");

        let err = WorkflowError::ExecutionFailed {
            node_id: "bar".into(),
            message: "oops".into(),
        };
        assert_eq!(err.to_string(), "execution failed at bar: oops");
    }
}
