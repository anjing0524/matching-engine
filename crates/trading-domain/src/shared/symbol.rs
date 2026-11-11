//! Process-level symbol interning.
//!
//! The wire protocol carries symbol strings, but the matching core carries a
//! compact [`SymbolId`]. Decoding an existing symbol does not allocate: the
//! reader borrows the frame bytes and the global table returns the existing id.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

/// Compact process-local symbol identifier.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolId(u32);

struct SymbolTable {
    names: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, SymbolId>,
}

impl SymbolTable {
    fn new() -> Self {
        let empty: Arc<str> = Arc::from("");
        let mut ids = HashMap::new();
        ids.insert(Arc::clone(&empty), SymbolId(0));
        Self {
            names: vec![empty],
            ids,
        }
    }
}

static SYMBOLS: LazyLock<RwLock<SymbolTable>> = LazyLock::new(|| RwLock::new(SymbolTable::new()));

impl SymbolId {
    /// Raw numeric id (for errors/diagnostics).
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Interns `symbol`, allocating only the first time it is seen.
    pub fn intern(symbol: &str) -> Self {
        {
            let table = SYMBOLS
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(id) = table.ids.get(symbol) {
                return *id;
            }
        }

        let mut table = SYMBOLS
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(id) = table.ids.get(symbol) {
            return *id;
        }
        let name: Arc<str> = Arc::from(symbol);
        let id = SymbolId(table.names.len() as u32);
        table.names.push(Arc::clone(&name));
        table.ids.insert(name, id);
        id
    }

    /// Returns the symbol name, if this id came from this process's table.
    #[must_use]
    pub fn name(self) -> Option<Arc<str>> {
        let table = SYMBOLS
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        table.names.get(self.0 as usize).map(Arc::clone)
    }
}

impl Serialize for SymbolId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.name() {
            Some(name) => serializer.serialize_str(&name),
            None => serializer.serialize_str(""),
        }
    }
}

impl<'de> Deserialize<'de> for SymbolId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let symbol = String::deserialize(deserializer)?;
        Ok(Self::intern(&symbol))
    }
}

impl std::fmt::Debug for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.name() {
            Some(name) => write!(f, "SymbolId({name:?})"),
            None => write!(f, "SymbolId(#{})", self.0),
        }
    }
}

impl std::fmt::Display for SymbolId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.name() {
            Some(name) => write!(f, "{name}"),
            None => write!(f, "#{}", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_is_stable() {
        let first = SymbolId::intern("BTCUSDT");
        let second = SymbolId::intern("BTCUSDT");
        assert_eq!(first, second);
        assert_eq!(first.name().as_deref(), Some("BTCUSDT"));
    }

    #[test]
    fn test_different_symbols_get_different_ids() {
        assert_ne!(SymbolId::intern("BTCUSDT"), SymbolId::intern("ETHUSDT"));
    }
}
