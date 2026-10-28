//! `/models` 列表缓存的状态机（上游 `ModelListingCache` 的等价核心，#39c）。
//!
//! 语义（rc.3 `listing-cache.ts`）：
//! - **凭据 + 端点作用域**：作用域变化即弃列表并作废在途加载（凭据原文
//!   不留在键里，只存 sha256 摘要）；
//! - **首次访问等待**：作用域内第一次访问等这次加载完成；之后立即返回
//!   （过期则在后台刷新）；
//! - **失败保留上次好列表**：settled 时间照常推进（同一年龄后重试），
//!   连续失败只报第一条；
//! - **invalidate** 弃列表并让下一次访问重新等待；**close** 之后结果与
//!   访问都不再改变状态。
//!
//! 本模块只落状态机与决策；发起请求/后台任务由调用方按决策执行
//! （用 `mark_loading` 取代，完成时 `finish` 回填）。

use crate::listing::ModelListing;
use sha2::{Digest, Sha256};

/// 列表缓存的作用域键：端点 + 凭据的 sha256 摘要。
pub fn listing_scope(url: &str, credential: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(credential.as_bytes());
    format!("{url}\n{:x}", hasher.finalize())
}

/// 一次访问的缓存决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingDecision {
    /// 首次访问（或刚 invalidate/换作用域）：调用方同步加载，完成前无值。
    Wait,
    /// 有可返回的列表（可能过旧但可用）：直接返回。
    Ready,
    /// 列表已过期：先返回旧值，调用方在后台刷新。
    Refresh,
}

/// 加载结束的结果摘要（诊断文案用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingSettle {
    /// 本次装载成功。
    pub loaded: bool,
    /// 失败但保留了上次好列表。
    pub retained: bool,
    /// 本次失败是连续失败里的第一条（上游只在第一条上报 warn）。
    pub first_failure: bool,
}

/// 缓存状态机（单作用域；线程安全由调用方持有 Mutex 保证）。
pub struct ListingCacheState {
    scope: Option<String>,
    listing: Option<ModelListing>,
    /// 最近一次加载落定时刻（成功或失败都推进——失败也按同一年龄重试）。
    settled_at: Option<u64>,
    /// 有在途加载（single-flight：同作用域不重复发起）。
    loading: bool,
    /// 连续失败中（用于「只报第一条」）。
    failing: bool,
    /// 作用域代际：换作用域/失效都会递增，过期代的回填被丢弃。
    generation: u64,
    closed: bool,
}

impl Default for ListingCacheState {
    fn default() -> Self {
        Self::new()
    }
}

impl ListingCacheState {
    pub fn new() -> Self {
        Self {
            scope: None,
            listing: None,
            settled_at: None,
            loading: false,
            failing: false,
            generation: 0,
            closed: false,
        }
    }

    /// 进入一个作用域。作用域变化时弃列表、作废在途加载并重置失败态；
    /// 返回是否发生了变化（变化 = 调用方需要重新等待加载）。
    pub fn enter_scope(&mut self, scope: &str) -> bool {
        if self.scope.as_deref() == Some(scope) {
            return false;
        }
        self.scope = Some(scope.to_string());
        self.listing = None;
        self.settled_at = None;
        self.loading = false;
        self.failing = false;
        self.generation += 1;
        true
    }

    /// 当前可返回的列表（无值 = 尚未成功加载过）。
    pub fn current(&self) -> Option<&ModelListing> {
        self.listing.as_ref()
    }

    /// 访问决策。`Wait` 时调用方应同步加载并 `mark_loading`；
    /// `Refresh` 时后台加载并 `mark_loading`；`Ready` 直接返回。
    pub fn view(&mut self, now_ms: u64, max_age_ms: u64) -> ListingDecision {
        if self.closed || self.loading {
            // 在途加载：首次（settled 为空）等待，其余立即返回旧值
            return if self.settled_at.is_none() && !self.closed {
                ListingDecision::Wait
            } else {
                ListingDecision::Ready
            };
        }
        match self.settled_at {
            None => ListingDecision::Wait,
            Some(settled) if listing_stale(now_ms.saturating_sub(settled), max_age_ms) => {
                ListingDecision::Refresh
            }
            Some(_) => ListingDecision::Ready,
        }
    }

    /// 取当前代并置在途（single-flight）。
    pub fn mark_loading(&mut self) -> u64 {
        self.loading = true;
        self.generation
    }

    /// 回填一次加载结果。过期代（已被换作用域/invalidate 作废）返回
    /// None，调用方据此丢弃结果。
    pub fn finish(
        &mut self,
        generation: u64,
        now_ms: u64,
        result: Result<ModelListing, ()>,
    ) -> Option<ListingSettle> {
        self.loading = false;
        if self.closed || generation != self.generation {
            return None;
        }
        self.settled_at = Some(now_ms);
        match result {
            Ok(listing) => {
                self.listing = Some(listing);
                self.failing = false;
                Some(ListingSettle { loaded: true, retained: false, first_failure: false })
            }
            Err(()) => {
                let retained = self.listing.is_some();
                let first_failure = !self.failing;
                self.failing = true;
                Some(ListingSettle { loaded: false, retained, first_failure })
            }
        }
    }

    /// 弃列表并作废在途；下一次访问像首次一样等待加载。
    pub fn invalidate(&mut self) {
        self.listing = None;
        self.settled_at = None;
        self.loading = false;
        self.failing = false;
        self.generation += 1;
    }

    /// 关闭：之后的结果与访问都不再改变状态。
    pub fn close(&mut self) {
        self.closed = true;
        self.loading = false;
        self.generation += 1;
    }
}

fn listing_stale(age_ms: u64, max_age_ms: u64) -> bool {
    crate::listing::listing_stale(age_ms, max_age_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(id: &str) -> ModelListing {
        ModelListing {
            entries: vec![crate::listing::ListingEntry {
                id: id.into(),
                name: None,
                requirement: crate::listing::ClientRequirement::None,
                capabilities: crate::listing::ListedCapabilities::Unusable("x".into()),
            }],
            skipped: 0,
        }
    }

    fn ids(listing: &ModelListing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn scope_key_hashes_the_credential() {
        let a = listing_scope("https://api.deepseek.com/models", "key-a");
        let b = listing_scope("https://api.deepseek.com/models", "key-b");
        let c = listing_scope("https://api.deepseek.com/models", "key-a");
        assert_ne!(a, b);
        assert_eq!(a, c);
        assert!(!a.contains("key-a"), "凭据原文不留在作用域键里");
    }

    #[test]
    fn first_access_waits_and_then_returns_the_listing() {
        let mut cache = ListingCacheState::new();
        assert!(cache.enter_scope("s1"));
        assert_eq!(cache.view(0, 600_000), ListingDecision::Wait);
        // 在途期间再次访问：仍等待（尚无值）
        let generation = cache.mark_loading();
        assert_eq!(cache.view(0, 600_000), ListingDecision::Wait);
        let settle = cache.finish(generation, 1_000, Ok(listing("m1"))).unwrap();
        assert_eq!(settle, ListingSettle { loaded: true, retained: false, first_failure: false });
        assert_eq!(cache.view(1_000, 600_000), ListingDecision::Ready);
        assert_eq!(ids(cache.current().unwrap()), vec!["m1"]);
    }

    #[test]
    fn stale_listing_refreshes_in_the_background() {
        let mut cache = ListingCacheState::new();
        cache.enter_scope("s1");
        let g = cache.mark_loading();
        cache.finish(g, 1_000, Ok(listing("m1"))).unwrap();
        // 未过期：Ready
        assert_eq!(cache.view(600_999, 600_000), ListingDecision::Ready);
        // 过期：Refresh（先返回旧值，后台刷新）
        assert_eq!(cache.view(601_000, 600_000), ListingDecision::Refresh);
        let g = cache.mark_loading();
        // 在途期间立即返回旧值
        assert_eq!(cache.view(601_001, 600_000), ListingDecision::Ready);
        cache.finish(g, 602_000, Ok(listing("m2"))).unwrap();
        assert_eq!(ids(cache.current().unwrap()), vec!["m2"]);
        assert_eq!(cache.view(602_000, 600_000), ListingDecision::Ready);
    }

    #[test]
    fn failure_retains_last_good_listing_and_reports_first_failure_once() {
        let mut cache = ListingCacheState::new();
        cache.enter_scope("s1");
        let g = cache.mark_loading();
        cache.finish(g, 1_000, Ok(listing("m1"))).unwrap();
        // 第一次失败：保留旧列表，报 first_failure
        let g = cache.mark_loading();
        let settle = cache.finish(g, 2_000, Err(())).unwrap();
        assert_eq!(settle, ListingSettle { loaded: false, retained: true, first_failure: true });
        assert_eq!(ids(cache.current().unwrap()), vec!["m1"]);
        // 失败也推进 settled：年龄未到不重试
        assert_eq!(cache.view(2_000, 600_000), ListingDecision::Ready);
        // 年龄到了重试，再次失败：first_failure=false
        assert_eq!(cache.view(602_000, 600_000), ListingDecision::Refresh);
        let g = cache.mark_loading();
        let settle = cache.finish(g, 602_001, Err(())).unwrap();
        assert_eq!(settle, ListingSettle { loaded: false, retained: true, first_failure: false });
        // 无旧列表的失败：retained=false
        let mut fresh = ListingCacheState::new();
        fresh.enter_scope("s1");
        let g = fresh.mark_loading();
        let settle = fresh.finish(g, 1_000, Err(())).unwrap();
        assert_eq!(settle, ListingSettle { loaded: false, retained: false, first_failure: true });
    }

    #[test]
    fn scope_change_discards_the_listing_and_drops_inflight_results() {
        let mut cache = ListingCacheState::new();
        cache.enter_scope("s1");
        let g = cache.mark_loading();
        cache.finish(g, 1_000, Ok(listing("m1"))).unwrap();
        // 换凭据/端点：弃列表，回到等待态
        assert!(cache.enter_scope("s2"));
        assert_eq!(cache.view(1_000, 600_000), ListingDecision::Wait);
        assert!(cache.current().is_none());
        // 旧代的回填被丢弃（在途结果不落进新作用域）
        let stale_generation = g;
        assert_eq!(cache.finish(stale_generation, 2_000, Ok(listing("old"))), None);
        assert!(cache.current().is_none());
        // 同作用域再次进入不影响状态
        let g = cache.mark_loading();
        cache.finish(g, 3_000, Ok(listing("m2"))).unwrap();
        assert!(!cache.enter_scope("s2"));
        assert_eq!(ids(cache.current().unwrap()), vec!["m2"]);
    }

    #[test]
    fn invalidate_forces_a_first_load_again() {
        let mut cache = ListingCacheState::new();
        cache.enter_scope("s1");
        let g = cache.mark_loading();
        cache.finish(g, 1_000, Ok(listing("m1"))).unwrap();
        cache.invalidate();
        assert!(cache.current().is_none());
        assert_eq!(cache.view(1_000, 600_000), ListingDecision::Wait);
        // 作废前的在途结果不再生效
        assert_eq!(cache.finish(g, 2_000, Ok(listing("old"))), None);
    }

    #[test]
    fn closed_cache_changes_nothing() {
        let mut cache = ListingCacheState::new();
        cache.enter_scope("s1");
        let g = cache.mark_loading();
        cache.finish(g, 1_000, Ok(listing("m1"))).unwrap();
        cache.close();
        assert_eq!(cache.view(10_000_000, 600_000), ListingDecision::Ready);
        assert_eq!(cache.finish(g, 2_000, Ok(listing("ignored"))), None);
        assert_eq!(ids(cache.current().unwrap()), vec!["m1"]);
    }
}