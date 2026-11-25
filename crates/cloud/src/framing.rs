//! Pipe v3 application framing (semantics unchanged from transport v2): one
//! fragmented logical Attach message per direction, plus the per-direction
//! credit window.
//!
//! Pure, no I/O. The actor owns one [`FragmentAssembler`] and one
//! [`CreditLedger`] per Attach stream; this module only encodes the rules.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::constants;
use crate::frames::StreamMsg;
use crate::{Error, Result};

/// Aggregate fragments in flight for one session (all its Attach streams).
/// The actor clones this handle into every stream assembler; a reservation
/// lives until its assembler completes, resets, or drops.
#[derive(Clone, Debug)]
pub struct ReassemblyBudget {
    used: Arc<AtomicUsize>,
    max: usize,
}

impl ReassemblyBudget {
    pub fn new(max: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    /// Reserve `len` for one message; false when the aggregate would exceed
    /// the budget. Nothing is allocated on failure.
    pub fn try_reserve(&self, len: usize) -> bool {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let Some(next) = used.checked_add(len).filter(|n| *n <= self.max) else {
                return false;
            };
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => used = actual,
            }
        }
    }

    fn release(&self, len: usize) {
        let mut used = self.used.load(Ordering::Relaxed);
        loop {
            let next = used.saturating_sub(len);
            match self
                .used
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(actual) => used = actual,
            }
        }
    }
}

impl Default for ReassemblyBudget {
    fn default() -> Self {
        Self::new(constants::MAX_LOGICAL_MESSAGE)
    }
}

/// Reassembles `FrameBegin{len}`, `Chunk*`, `FrameEnd` into exact UTF-8.
/// Rejects malformed sequences and an over-budget declared length before
/// allocating its buffer.
pub struct FragmentAssembler {
    /// Declared length, `None` while idle.
    expected: Option<usize>,
    buf: Vec<u8>,
    budget: ReassemblyBudget,
    /// Bytes currently held against `budget` (released exactly once).
    reserved: usize,
}

impl FragmentAssembler {
    pub fn new() -> Self {
        Self::with_budget(ReassemblyBudget::default())
    }

    pub fn with_budget(budget: ReassemblyBudget) -> Self {
        Self {
            expected: None,
            buf: Vec::new(),
            budget,
            reserved: 0,
        }
    }

    /// True while a fragmented message is in flight. A complete `Frame`
    /// arriving now is a nested message and must be rejected.
    pub fn is_assembling(&self) -> bool {
        self.expected.is_some()
    }

    /// Feed one fragment record. `Ok(Some(text))` is a completed message;
    /// any `Err` resets the assembler and releases its reservation.
    pub fn feed(&mut self, msg: StreamMsg) -> Result<Option<String>> {
        match msg {
            StreamMsg::FrameBegin { len } => {
                if self.expected.is_some() {
                    return self.fail(Error::Protocol);
                }
                let len = len as usize;
                if len > constants::MAX_LOGICAL_MESSAGE {
                    return self.fail(Error::Protocol);
                }
                if !self.budget.try_reserve(len) {
                    return self.fail(Error::Limit);
                }
                self.expected = Some(len);
                self.reserved = len;
                self.buf.clear();
                Ok(None)
            }
            StreamMsg::Chunk(chunk) => {
                let Some(expected) = self.expected else {
                    return self.fail(Error::Protocol);
                };
                if chunk.len() > constants::MAX_CHUNK || self.buf.len() + chunk.len() > expected {
                    return self.fail(Error::Protocol);
                }
                self.buf.extend_from_slice(&chunk);
                Ok(None)
            }
            StreamMsg::FrameEnd => {
                let Some(expected) = self.expected else {
                    return self.fail(Error::Protocol);
                };
                if self.buf.len() != expected {
                    return self.fail(Error::Protocol);
                }
                self.release();
                self.expected = None;
                match String::from_utf8(std::mem::take(&mut self.buf)) {
                    Ok(text) => Ok(Some(text)),
                    Err(_) => Err(Error::Protocol),
                }
            }
            // A complete message is not a fragment record; the caller must
            // treat it as a protocol fault while assembling.
            _ => self.fail(Error::Protocol),
        }
    }

    fn fail(&mut self, e: Error) -> Result<Option<String>> {
        self.expected = None;
        self.buf.clear();
        self.release();
        Err(e)
    }

    fn release(&mut self) {
        if self.reserved > 0 {
            self.budget.release(self.reserved);
            self.reserved = 0;
        }
    }
}

impl Default for FragmentAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FragmentAssembler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FragmentAssembler")
            .field("expected", &self.expected)
            .field("received", &self.buf.len())
            .finish()
    }
}

impl Drop for FragmentAssembler {
    fn drop(&mut self) {
        self.release();
    }
}

/// One direction's credit window. `consume` never lets the sender go
/// negative; `grant` never raises the window above `max`.
#[derive(Debug, Clone)]
pub struct CreditLedger {
    available: u32,
    max: u32,
}

impl CreditLedger {
    pub fn new(initial: u32, max: u32) -> Self {
        Self {
            available: initial.min(max),
            max,
        }
    }

    pub fn available(&self) -> u32 {
        self.available
    }

    /// Spend `bytes` from the window; `Error::Credit` when insufficient.
    pub fn consume(&mut self, bytes: u32) -> Result<()> {
        if u64::from(bytes) > u64::from(self.available) {
            return Err(Error::Credit);
        }
        self.available -= bytes;
        Ok(())
    }

    /// Peer window update. Zero is malformed; an oversize grant clamps.
    pub fn grant(&mut self, bytes: u32) -> Result<()> {
        if bytes == 0 {
            return Err(Error::Protocol);
        }
        self.available = self.available.saturating_add(bytes).min(self.max);
        Ok(())
    }
}
