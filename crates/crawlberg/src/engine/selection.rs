//! Frontier selection: how a [`CrawlStrategy`](crate::traits::CrawlStrategy) turns the
//! working set into the next URL to fetch.

/// Ask `strategy` for the next entry and remove it from `working_set`, preserving the
/// order of the entries left behind.
///
/// ~keep Order preservation is the whole contract. `Vec::swap_remove` moves the last
/// element into the vacated slot, so a strategy that selects by position — `BfsStrategy`
/// always picks index 0 — would see the newest entry there on the next call and drain the
/// set as first, last, second-to-last, ... instead of FIFO. Shared by the native and wasm
/// loops so the two cannot drift, as `DEFAULT_MAX_LINKS_PER_PAGE` above is.
/// Returns the entry together with the index it came from, so a caller that decides not to
/// fetch it after all can put it back where it was.
pub(crate) fn take_selected(
    strategy: &dyn crate::traits::CrawlStrategy,
    working_set: &mut Vec<crate::traits::FrontierEntry>,
) -> Option<(usize, crate::traits::FrontierEntry)> {
    let index = strategy.select_next(working_set)?;
    if index >= working_set.len() {
        return None;
    }
    Some((index, working_set.remove(index)))
}

#[cfg(test)]
mod take_selected_tests {
    use super::*;
    use crate::defaults::{BfsStrategy, DfsStrategy};
    use crate::traits::FrontierEntry;

    fn entry(url: &str) -> FrontierEntry {
        FrontierEntry {
            url: url.to_owned(),
            depth: 0,
            doc_depth: 0,
            priority: 1.0,
        }
    }

    fn working_set(urls: &[&str]) -> Vec<FrontierEntry> {
        urls.iter().map(|url| entry(url)).collect()
    }

    fn drain(strategy: &dyn crate::traits::CrawlStrategy, set: &mut Vec<FrontierEntry>) -> Vec<String> {
        let mut order = Vec::new();
        while let Some((_index, taken)) = take_selected(strategy, set) {
            order.push(taken.url);
        }
        order
    }

    #[test]
    fn should_drain_working_set_in_fifo_order_when_strategy_is_bfs() {
        let mut set = working_set(&["a", "b", "c", "d"]);
        assert_eq!(drain(&BfsStrategy, &mut set), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn should_drain_working_set_in_lifo_order_when_strategy_is_dfs() {
        let mut set = working_set(&["a", "b", "c", "d"]);
        assert_eq!(drain(&DfsStrategy, &mut set), vec!["d", "c", "b", "a"]);
    }

    #[test]
    fn should_return_the_entry_the_strategy_selected() {
        let mut set = working_set(&["a", "b", "c"]);
        let (index, taken) = take_selected(&BfsStrategy, &mut set).expect("non-empty set must yield an entry");

        assert_eq!(index, 0);
        assert_eq!(taken.url, "a");
        assert_eq!(
            set.iter().map(|e| e.url.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"],
            "removal must preserve the relative order of the remaining entries"
        );
    }

    #[test]
    fn should_return_none_when_working_set_is_empty() {
        let mut set: Vec<FrontierEntry> = Vec::new();
        assert!(take_selected(&BfsStrategy, &mut set).is_none());
    }
}
