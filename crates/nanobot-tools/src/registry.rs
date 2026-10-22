//! ToolRegistry: tool registration and lookup (design-harness 10.5).
//!
//! The registry rejects duplicate tool names and returns structured errors
//! for unknown tools. Tool definitions include a JSON Schema for parameter
//! validation and an approval requirement flag.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// A tool definition registered in the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters_schema: serde_json::Value,
    /// Whether this tool requires approval before execution.
    pub requires_approval: bool,
    /// Tier 1 structural contract this tool promises (design-harness 18).
    pub contract: crate::contract::ToolContract,
}

/// Registry of tool definitions.
///
/// Thread-safe (Mutex). Rejects duplicate names; unknown tool lookups
/// return a structured error listing available tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Mutex<HashMap<String, ToolDefinition>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool definition. Returns `Err(ToolError::DuplicateTool)`
    /// if a tool with the same name already exists.
    pub fn register(&self, def: ToolDefinition) -> Result<(), crate::error::ToolError> {
        let mut tools = self.tools.lock().unwrap();
        if tools.contains_key(&def.name) {
            return Err(crate::error::ToolError::DuplicateTool(def.name.clone()));
        }
        tools.insert(def.name.clone(), def);
        Ok(())
    }

    /// Deregister a tool by name.
    pub fn deregister(&self, name: &str) -> Option<ToolDefinition> {
        self.tools.lock().unwrap().remove(name)
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<ToolDefinition> {
        self.tools.lock().unwrap().get(name).cloned()
    }

    /// List all registered tool definitions.
    pub fn list(&self) -> Vec<ToolDefinition> {
        self.tools.lock().unwrap().values().cloned().collect()
    }

    /// Check whether a tool is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.lock().unwrap().contains_key(name)
    }

    /// Get the names of all registered tools.
    pub fn names(&self) -> Vec<String> {
        self.tools.lock().unwrap().keys().cloned().collect()
    }
}
