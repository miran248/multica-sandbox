//! The per-epic loopback port block.
//!
//! Every epic owns 16 consecutive `127.0.0.1` ports from 23000 upward. The
//! block is recorded in the state file on the first `up`, reused unchanged on
//! later runs, and released by `gc` (or `rm`). A block is only ever handed out
//! when all 16 ports are free to bind; a recorded block is stable even if a
//! container is not running, because a restart must keep the same ports.

use crate::state::{now, Entry, State, Store};
use std::fmt;
use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;

/// Ports per epic block.
pub const BLOCK_SIZE: u16 = 16;
/// The first port the allocator considers.
pub const FIRST_PORT: u16 = 23000;
/// The last port a block may reach. Kept below the ephemeral ranges on both
/// macOS (49152) and Linux (32768) with room for many epics.
pub const LAST_PORT: u16 = 32000;
/// `gc` releases a block after this many idle seconds (14 days).
pub const IDLE_SECS: u64 = 14 * 24 * 60 * 60;

/// One epic's 16 ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub start: u16,
}

impl Block {
    /// The last port in the block.
    pub fn end(&self) -> u16 {
        self.start + BLOCK_SIZE - 1
    }

    /// Every port in the block, in order.
    pub fn ports(&self) -> impl Iterator<Item = u16> {
        self.start..=self.end()
    }

    /// The port at `index` in `publish` order.
    pub fn port(&self, index: usize) -> Option<u16> {
        if index < usize::from(BLOCK_SIZE) {
            Some(self.start + index as u16)
        } else {
            None
        }
    }

    pub fn contains(&self, port: u16) -> bool {
        (self.start..=self.end()).contains(&port)
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.start, self.end())
    }
}

/// Why an allocation failed. Every variant is loud: a collision is never
/// resolved by silently picking another port behind the caller's back.
#[derive(Debug)]
pub enum PortError {
    Io(io::Error),
    /// No free block remains in the configured range.
    Exhausted {
        first: u16,
        last: u16,
    },
}

impl fmt::Display for PortError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortError::Io(error) => write!(f, "{error}"),
            PortError::Exhausted { first, last } => write!(
                f,
                "no free {BLOCK_SIZE}-port block between {first} and {last}; release one with `msbx gc`"
            ),
        }
    }
}

impl std::error::Error for PortError {}

impl From<io::Error> for PortError {
    fn from(error: io::Error) -> Self {
        PortError::Io(error)
    }
}

/// Reads and writes blocks in one state file.
#[derive(Debug, Clone)]
pub struct Allocator {
    store: Store,
    first: u16,
    last: u16,
}

impl Allocator {
    /// The allocator the installed binary uses.
    pub fn default_path() -> Self {
        Self::at(Store::default_path().path().to_path_buf())
    }

    /// The production range: 23000 through 32000.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::with_range(path, FIRST_PORT, LAST_PORT)
    }

    /// An allocator over one state file and an explicit range. The range is
    /// inclusive and must fit a whole block; `with_range` is what tests and
    /// `init` use to keep their synthetic blocks away from real ones.
    pub fn with_range(path: impl Into<PathBuf>, first: u16, last: u16) -> Self {
        debug_assert!(first + BLOCK_SIZE - 1 <= last);
        Allocator {
            store: Store::new(path),
            first,
            last,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The recorded block for `key`, without allocating or touching it.
    pub fn block(&self, key: &str) -> Result<Option<Block>, PortError> {
        let state = self.store.load()?;
        Ok(state
            .blocks
            .get(key)
            .map(|entry| Block { start: entry.start }))
    }

    /// The block for `key`, allocating one on first use.
    ///
    /// - Same key twice: the recorded block, with `last_used` refreshed.
    /// - New key: the first block whose 16 ports are all free to bind on
    ///   `127.0.0.1`, skipping blocks other entries own.
    pub fn allocate(&self, key: &str, at: u64) -> Result<Block, PortError> {
        let _lock = self.store.lock()?;
        let mut state = self.store.load()?;

        if let Some(entry) = state.blocks.get_mut(key) {
            entry.last_used = at;
            let block = Block { start: entry.start };
            self.store.save(&state)?;
            return Ok(block);
        }

        let mut taken = Vec::with_capacity(state.blocks.len());
        for entry in state.blocks.values() {
            taken.push(Block { start: entry.start });
        }

        let mut start = self.first;
        while start
            .checked_add(BLOCK_SIZE - 1)
            .is_some_and(|end| end <= self.last)
        {
            let block = Block { start };
            if !taken.iter().any(|other| overlaps(*other, block)) && block_is_free(block) {
                state.blocks.insert(
                    key.to_string(),
                    Entry {
                        start,
                        assigned: at,
                        last_used: at,
                    },
                );
                self.store.save(&state)?;
                return Ok(block);
            }
            match start.checked_add(BLOCK_SIZE) {
                Some(next) => start = next,
                None => break,
            }
        }

        Err(PortError::Exhausted {
            first: self.first,
            last: self.last,
        })
    }

    /// Drop `key`'s block. Returns the block that was released.
    pub fn release(&self, key: &str) -> Result<Option<Block>, PortError> {
        let _lock = self.store.lock()?;
        let mut state = self.store.load()?;
        match state.blocks.remove(key) {
            Some(entry) => {
                self.store.save(&state)?;
                Ok(Some(Block { start: entry.start }))
            }
            None => Ok(None),
        }
    }

    /// Release every key idle for at least `idle` seconds. With `dry_run`, the
    /// state file is read and not written.
    pub fn release_idle(
        &self,
        idle: u64,
        at: u64,
        dry_run: bool,
    ) -> Result<Vec<(String, Block)>, PortError> {
        if dry_run {
            let state = self.store.load()?;
            return Ok(idle_entries(&state, idle, at));
        }

        let _lock = self.store.lock()?;
        let mut state = self.store.load()?;
        let stale = idle_entries(&state, idle, at);
        if stale.is_empty() {
            return Ok(stale);
        }
        for (key, _) in &stale {
            state.blocks.remove(key);
        }
        self.store.save(&state)?;
        Ok(stale)
    }
}

fn idle_entries(state: &State, idle: u64, at: u64) -> Vec<(String, Block)> {
    state
        .blocks
        .iter()
        .filter(|(_, entry)| at.saturating_sub(entry.last_used) >= idle)
        .map(|(key, entry)| (key.clone(), Block { start: entry.start }))
        .collect()
}

fn overlaps(left: Block, right: Block) -> bool {
    left.start <= right.end() && right.start <= left.end()
}

/// `true` when every port in `block` can be bound on `127.0.0.1`. The listeners
/// are held together so a transiently free port inside the block is still
/// caught; they are dropped before the caller starts anything.
fn block_is_free(block: Block) -> bool {
    let mut listeners = Vec::with_capacity(usize::from(BLOCK_SIZE));
    for port in block.ports() {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(listener) => listeners.push(listener),
            Err(_) => return false,
        }
    }
    !listeners.is_empty()
}

/// Everything `gc` needs: the idle window and the clock are explicit so the
/// behaviour is testable without waiting 14 days.
pub fn gc(allocator: &Allocator, dry_run: bool) -> Result<Vec<(String, Block)>, PortError> {
    allocator.release_idle(IDLE_SECS, now(), dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::net::TcpListener;

    fn scratch(tag: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-scratch")
            .join(format!("ports-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Blocks are 16 wide; tests use a range far from both the production base
    /// and the ephemeral ranges.
    fn test_allocator(tag: &str, first: u16) -> (Allocator, PathBuf) {
        let dir = scratch(tag);
        let allocator = Allocator::with_range(dir.join("state.json"), first, first + 64);
        (allocator, dir)
    }

    #[test]
    fn allocates_consecutive_blocks() {
        let (allocator, dir) = test_allocator("consecutive", 41000);
        let first = allocator.allocate("demo-epic-a", 10).unwrap();
        let second = allocator.allocate("demo-epic-b", 10).unwrap();
        assert_eq!(first.start, 41000);
        assert_eq!(second.start, 41016);
        assert!(!overlaps(first, second));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn is_idempotent_for_the_same_epic() {
        let (allocator, dir) = test_allocator("idempotent", 41100);
        let first = allocator.allocate("demo-epic-a", 10).unwrap();
        let again = allocator.allocate("demo-epic-a", 20).unwrap();
        assert_eq!(first, again);
        assert_eq!(allocator.block("demo-epic-a").unwrap(), Some(first));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn skips_a_block_with_a_bound_port() {
        let (allocator, dir) = test_allocator("bound", 41200);
        // Hold the first port of the first candidate block; the allocator must
        // skip the whole block, not just that port.
        let holder = TcpListener::bind((Ipv4Addr::LOCALHOST, 41200)).unwrap();
        let block = allocator.allocate("demo-epic-a", 10).unwrap();
        assert_eq!(block.start, 41216);
        drop(holder);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn release_returns_the_block_to_the_pool() {
        let (allocator, dir) = test_allocator("release", 41300);
        let first = allocator.allocate("demo-epic-a", 10).unwrap();
        assert_eq!(allocator.release("demo-epic-a").unwrap(), Some(first));
        assert_eq!(allocator.release("demo-epic-a").unwrap(), None);
        let reused = allocator.allocate("demo-epic-b", 10).unwrap();
        assert_eq!(reused, first);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn release_idle_honours_the_window_and_dry_run() {
        let (allocator, dir) = test_allocator("idle", 41400);
        let block = allocator.allocate("demo-epic-a", 1_000).unwrap();
        assert!(allocator
            .release_idle(IDLE_SECS, 1_000, true)
            .unwrap()
            .is_empty());
        let stale = allocator
            .release_idle(IDLE_SECS, 1_000 + IDLE_SECS, true)
            .unwrap();
        assert_eq!(stale, vec![("demo-epic-a".to_string(), block)]);
        // A dry run changes nothing.
        assert_eq!(allocator.block("demo-epic-a").unwrap(), Some(block));
        let released = allocator
            .release_idle(IDLE_SECS, 1_000 + IDLE_SECS + 1, false)
            .unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(allocator.block("demo-epic-a").unwrap(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reports_exhaustion_instead_of_reusing_a_ported_block() {
        let dir = scratch("exhausted");
        let allocator = Allocator::with_range(dir.join("state.json"), 42000, 42016);
        assert!(allocator.allocate("demo-epic-a", 1).is_ok());
        let error = allocator.allocate("demo-epic-b", 1).unwrap_err();
        assert!(matches!(error, PortError::Exhausted { .. }));
        let _ = fs::remove_dir_all(dir);
    }
}
