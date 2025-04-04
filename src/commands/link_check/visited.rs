use std::{collections::HashSet, sync::Mutex};

use url::Url;

use super::utils::normalize_url;

#[derive(Debug, Default)]
pub(super) struct Visited {
    visited: Mutex<HashSet<Url>>,
}

impl Visited {
    /// Mark a URL as visited.
    ///
    /// ## Returns
    /// Returns `true` if the URL was not already visited, `false` otherwise.
    pub(super) fn mark_visited(&self, url: &Url) -> bool {
        let normalized_url = normalize_url(url);

        {
            let mut visited = self.visited.lock().unwrap();
            if visited.contains(&normalized_url) {
                return true;
            }
            visited.insert(normalized_url);
        }

        false
    }
}
