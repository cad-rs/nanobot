//! Model catalog: a logical model id -> the provider it routes to (U29).
//!
//! Before the catalog, a harness could only route to the provider key it
//! registered its adapter under, and the agent asked for the adapter key
//! `"default"` regardless of which model the product configured. That made
//! multi-model routing impossible and left the cost gate (which prices per
//! model) with nothing truthful to price.
//!
//! A [`ModelCatalog`] declares the routable models: a model id, the adapter
//! key to route it to, and the wire model id the provider is asked for. The
//! model id and the wire model may differ — an alias like `"fast"` can route
//! to a provider under a different wire id, which is how a harness offers
//! cost/latency tiers without naming a concrete provider model.

use std::collections::HashMap;

/// The provider key every harness falls back to when it declares no route for
/// a model id. The agent's default model id is this same string, so a harness
/// that registers one adapter under `"default"` and sets a model id keeps
/// working exactly as it did before the catalog existed.
pub const DEFAULT_PROVIDER: &str = "default";

/// Where a model id routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    /// The adapter key the request is routed to.
    pub provider: String,
    /// The model id the provider is asked for (the wire model).
    pub model: String,
}

impl ModelRoute {
    /// A route to `provider`, asking it for wire model `model`.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

/// The routable models a harness declares.
///
/// Cheap to clone (it is a `HashMap`), so the runtime and the harness builder
/// can share one.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    routes: HashMap<String, ModelRoute>,
}

impl ModelCatalog {
    /// An empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare that `id` routes to `provider`, asking it for wire model `model`.
    ///
    /// Returns the previous route for `id`, if any.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Option<ModelRoute> {
        self.routes.insert(id.into(), ModelRoute::new(provider, model))
    }

    /// Declare an existing route under `id`.
    pub fn insert(&mut self, id: impl Into<String>, route: ModelRoute) -> Option<ModelRoute> {
        self.routes.insert(id.into(), route)
    }

    /// Look up a model id.
    pub fn get(&self, id: &str) -> Option<&ModelRoute> {
        self.routes.get(id)
    }

    /// The declared model ids.
    pub fn ids(&self) -> impl Iterator<Item = &String> {
        self.routes.keys()
    }

    /// How many routes are declared.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether no route is declared.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_maps_an_id_to_a_provider_and_a_wire_model() {
        let mut catalog = ModelCatalog::new();
        assert!(catalog.register("fast", "openai", "gpt-6-mini").is_none());

        assert_eq!(
            catalog.get("fast"),
            Some(&ModelRoute::new("openai", "gpt-6-mini")),
            "the model id and the wire model are allowed to differ"
        );
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.ids().collect::<Vec<_>>(), vec!["fast"]);
    }

    #[test]
    fn re_registering_replaces_and_returns_the_previous_route() {
        let mut catalog = ModelCatalog::new();
        catalog.register("fast", "openai", "gpt-6-mini");
        let previous = catalog.register("fast", "anthropic", "claude-sonnet-4");

        assert_eq!(previous, Some(ModelRoute::new("openai", "gpt-6-mini")));
        assert_eq!(catalog.get("fast"), Some(&ModelRoute::new("anthropic", "claude-sonnet-4")));
    }

    #[test]
    fn an_empty_catalog_is_empty() {
        let catalog = ModelCatalog::new();
        assert!(catalog.is_empty());
        assert_eq!(catalog.get("anything"), None);
    }
}
