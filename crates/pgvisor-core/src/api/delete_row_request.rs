use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request payload to delete a single row identified by its primary key(s).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRowRequest {
    pub primary_keys: HashMap<String, serde_json::Value>,
}
