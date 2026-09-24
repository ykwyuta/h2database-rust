use std::cell::RefCell;
use std::time::{Duration, Instant};
use crate::error::{H2Error, H2Result};

thread_local! {
    static QUERY_DEADLINE: RefCell<Option<Instant>> = const { RefCell::new(None) };
}

/// クエリタイムアウトのスコープ管理ガード（RAII）
#[must_use = "TimeoutGuard must be held for the duration of the query"]
pub struct TimeoutGuard {
    _priv: (),
}

impl Drop for TimeoutGuard {
    fn drop(&mut self) {
        QUERY_DEADLINE.with(|cell| {
            *cell.borrow_mut() = None;
        });
    }
}

/// カレントスレッドのクエリタイムアウト（デッドライン）を設定
pub fn set_query_timeout(timeout: Option<Duration>) -> TimeoutGuard {
    QUERY_DEADLINE.with(|cell| {
        *cell.borrow_mut() = timeout.map(|t| Instant::now() + t);
    });
    TimeoutGuard { _priv: () }
}

/// 現在設定されているクエリタイムアウトの残り時間を取得
pub fn remaining_query_timeout() -> Option<Duration> {
    QUERY_DEADLINE.with(|cell| {
        (*cell.borrow()).map(|deadline| {
            let now = Instant::now();
            if now >= deadline {
                Duration::from_millis(0)
            } else {
                deadline - now
            }
        })
    })
}

/// クエリタイムアウトに達しているかチェック。超過していれば Err(H2Error::QueryTimeout) を返却
#[inline]
pub fn check_query_timeout() -> H2Result<()> {
    QUERY_DEADLINE.with(|cell| {
        if let Some(deadline) = *cell.borrow() {
            if Instant::now() >= deadline {
                return Err(H2Error::QueryTimeout("Query execution timed out".to_string()));
            }
        }
        Ok(())
    })
}
