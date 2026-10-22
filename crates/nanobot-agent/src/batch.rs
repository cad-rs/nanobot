//! Batch / DOE primitives: N variants, deterministic seeds, dedup, aggregation.
//!
//! Yield, corner and sensitivity analysis all run the *same* job over a sweep
//! of parameters — N variants, each needing a reproducible seed, deduplicated
//! so a repeated corner is not run twice, and aggregated so the caller sees
//! which corner failed rather than a pile of unrelated results.
//!
//! The framework provides the sweep and the aggregation; what a variant *is*
//! and what running it means belong to the product (see [`crate::job`]).

use serde_json::{Map, Value};

/// One sweep axis: a parameter name and the values to sweep it over.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    /// Parameter name.
    pub name: String,
    /// Values to sweep, in order.
    pub values: Vec<Value>,
}

impl Axis {
    /// An axis named `name` over `values`.
    #[must_use]
    pub fn new(name: impl Into<String>, values: Vec<Value>) -> Self {
        Self {
            name: name.into(),
            values,
        }
    }
}

/// A batch: a base argument object plus sweep axes.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSpec {
    /// The arguments every variant shares.
    pub base: Value,
    /// The axes to sweep.
    pub axes: Vec<Axis>,
}

/// One variant of a batch.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    /// Position in the deterministic variant order.
    pub index: usize,
    /// A seed derived from the variant's arguments, so a variant is
    /// reproducible regardless of when or where it runs.
    pub seed: u64,
    /// The arguments for this variant.
    pub arguments: Value,
    /// The axis values that produced it, for reporting.
    pub point: Vec<(String, Value)>,
}

impl Variant {
    /// One-line rendering for logs and reports.
    #[must_use]
    pub fn headline(&self) -> String {
        let point: Vec<String> = self.point.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("#{} seed={} {}", self.index, self.seed, point.join(" "))
    }
}

impl BatchSpec {
    /// A batch over `base` with no axes (a single variant).
    #[must_use]
    pub fn new(base: Value) -> Self {
        Self {
            base,
            axes: Vec::new(),
        }
    }

    /// Add a sweep axis.
    #[must_use]
    pub fn with_axis(mut self, name: impl Into<String>, values: Vec<Value>) -> Self {
        self.axes.push(Axis::new(name, values));
        self
    }

    /// The variants, in deterministic order, deduplicated.
    ///
    /// Order is row-major over the axes (the last axis varies fastest), and the
    /// base object is the first variant when there are no axes. Duplicate
    /// argument objects collapse to their first occurrence, so a corner listed
    /// on two axes is run once.
    #[must_use]
    pub fn variants(&self) -> Vec<Variant> {
        let mut combos: Vec<Vec<(String, Value)>> = vec![Vec::new()];
        for axis in &self.axes {
            let mut next = Vec::with_capacity(combos.len() * axis.values.len().max(1));
            for combo in &combos {
                for value in &axis.values {
                    let mut extended = combo.clone();
                    extended.push((axis.name.clone(), value.clone()));
                    next.push(extended);
                }
            }
            combos = next;
        }

        let mut seen = std::collections::HashSet::new();
        let mut variants = Vec::new();
        for combo in combos {
            let arguments = merge(&self.base, &combo);
            if !seen.insert(arguments.to_string()) {
                continue;
            }
            let seed = seed_of(&arguments);
            variants.push(Variant {
                index: variants.len(),
                seed,
                arguments,
                point: combo,
            });
        }
        variants
    }

    /// The number of distinct variants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.variants().len()
    }

    /// Whether the batch is empty (no variants).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The outcome of one variant.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantOutcome {
    /// Which variant ran.
    pub variant: Variant,
    /// Whether it succeeded.
    pub succeeded: bool,
    /// The result handle on success.
    pub result: Option<String>,
    /// The error on failure.
    pub error: Option<String>,
}

/// The aggregate over a batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchOutcome {
    /// Every variant's outcome, in variant order.
    pub outcomes: Vec<VariantOutcome>,
}

impl BatchOutcome {
    /// The number of variants.
    #[must_use]
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    /// How many succeeded.
    #[must_use]
    pub fn succeeded(&self) -> usize {
        self.outcomes.iter().filter(|o| o.succeeded).count()
    }

    /// How many failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.total() - self.succeeded()
    }

    /// Whether every variant succeeded.
    #[must_use]
    pub fn all_succeeded(&self) -> bool {
        self.failed() == 0
    }

    /// The failing variants, for a report that says which corner broke.
    #[must_use]
    pub fn failures(&self) -> Vec<&VariantOutcome> {
        self.outcomes.iter().filter(|o| !o.succeeded).collect()
    }
}

/// Merge axis values into a copy of `base`.
fn merge(base: &Value, point: &[(String, Value)]) -> Value {
    let Value::Object(map) = base else {
        // A non-object base cannot carry parameters; the point wins.
        return Value::Object(point.iter().cloned().collect());
    };
    let mut out = Map::new();
    for (key, value) in map {
        out.insert(key.clone(), value.clone());
    }
    for (key, value) in point {
        out.insert(key.clone(), value.clone());
    }
    Value::Object(out)
}

/// A deterministic 64-bit seed from a variant's arguments.
fn seed_of(arguments: &Value) -> u64 {
    // FNV-1a over the canonical JSON text: stable across processes and
    // platforms, unlike a randomized hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in arguments.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_axes_yields_the_base_as_one_variant() {
        let spec = BatchSpec::new(json!({"a": 1}));
        let variants = spec.variants();
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].arguments, json!({"a": 1}));
        assert!(variants[0].point.is_empty());
    }

    #[test]
    fn the_cartesian_product_is_row_major() {
        let spec = BatchSpec::new(json!({"base": true}))
            .with_axis("jc", vec![json!(100), json!(120)])
            .with_axis("temp", vec![json!(4.2), json!(20.0)]);
        let variants = spec.variants();
        assert_eq!(variants.len(), 4);
        // Last axis varies fastest.
        assert_eq!(variants[0].arguments["temp"], json!(4.2));
        assert_eq!(variants[1].arguments["temp"], json!(20.0));
        assert_eq!(variants[1].arguments["jc"], json!(100));
        assert_eq!(variants[3].arguments["jc"], json!(120));
        // Indices are dense and ordered.
        let indices: Vec<usize> = variants.iter().map(|v| v.index).collect();
        assert_eq!(indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn duplicate_corners_collapse() {
        // The same sweep listed twice produces the same argument objects; each
        // distinct corner is run once.
        let spec = BatchSpec::new(json!({}))
            .with_axis("a", vec![json!(1), json!(2)])
            .with_axis("a", vec![json!(1), json!(2)]);
        let variants = spec.variants();
        let mut values: Vec<i64> = variants
            .iter()
            .map(|v| v.arguments["a"].as_i64().unwrap())
            .collect();
        values.sort_unstable();
        assert_eq!(values, vec![1, 2], "the repeated sweep is deduplicated");
        assert_eq!(spec.len(), 2);
    }

    #[test]
    fn seeds_are_deterministic_and_differ_per_variant() {
        let spec =
            BatchSpec::new(json!({})).with_axis("jc", vec![json!(100), json!(120), json!(140)]);
        let variants = spec.variants();
        let seeds: Vec<u64> = variants.iter().map(|v| v.seed).collect();
        // Same spec, same seeds.
        assert_eq!(
            seeds,
            spec.variants().iter().map(|v| v.seed).collect::<Vec<_>>()
        );
        // Distinct variants get distinct seeds.
        let unique: std::collections::HashSet<u64> = seeds.iter().copied().collect();
        assert_eq!(unique.len(), seeds.len());
    }

    #[test]
    fn the_base_is_not_mutated_and_axis_values_win() {
        let base = json!({"jc": 1, "shared": true});
        let spec = BatchSpec::new(base.clone()).with_axis("jc", vec![json!(999)]);
        let variants = spec.variants();
        assert_eq!(variants[0].arguments, json!({"jc": 999, "shared": true}));
        assert_eq!(base, json!({"jc": 1, "shared": true}), "base is untouched");
    }

    #[test]
    fn aggregation_reports_which_corner_failed() {
        let spec = BatchSpec::new(json!({})).with_axis("jc", vec![json!(1), json!(2)]);
        let variants = spec.variants();
        let outcome = BatchOutcome {
            outcomes: vec![
                VariantOutcome {
                    variant: variants[0].clone(),
                    succeeded: true,
                    result: Some("h1".into()),
                    error: None,
                },
                VariantOutcome {
                    variant: variants[1].clone(),
                    succeeded: false,
                    result: None,
                    error: Some("jc=2 violated the window".into()),
                },
            ],
        };
        assert_eq!(outcome.total(), 2);
        assert_eq!(outcome.succeeded(), 1);
        assert_eq!(outcome.failed(), 1);
        assert!(!outcome.all_succeeded());
        let failures = outcome.failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].variant.arguments["jc"], json!(2));
    }
}
