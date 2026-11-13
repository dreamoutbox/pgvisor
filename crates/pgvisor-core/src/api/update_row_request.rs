use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request payload to update columns for a single row identified by its primary key(s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRowRequest {
    pub primary_keys: HashMap<String, serde_json::Value>,
    pub values: HashMap<String, serde_json::Value>,
}
