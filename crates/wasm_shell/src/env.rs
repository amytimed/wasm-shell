use std::collections::HashMap;

/// A simple environment variable map.
#[derive(Debug, Clone, Default)]
pub struct EnvMap(HashMap<String, String>);

impl EnvMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }

    pub fn unset(&mut self, key: &str) {
        self.0.remove(key);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn to_vec(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = self.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }
}
