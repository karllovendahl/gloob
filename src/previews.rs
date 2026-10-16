use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::Result;

use crate::preview::Animation;

pub(crate) const MEMORY_BUDGET: usize = 64 * 1024 * 1024;
// sixteen maximum-sized decoded previews, including one displayed frame each, fit the budget.
const WINDOW_SIZE: usize = 16;

pub(crate) struct Resident {
    pub animation: Animation,
    pub frame: usize,
    pub next_frame: Instant,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct Previews {
    generation: u64,
    wanted: Vec<usize>,
    pending: HashSet<usize>,
    failures: HashMap<usize, String>,
    pub residents: HashMap<usize, Resident>,
    bytes: usize,
}

impl Previews {
    pub fn reset(&mut self, generation: u64) {
        *self = Self {
            generation,
            ..Self::default()
        };
    }

    // the caller supplies visible tiles first, then nearby tiles in distance order.
    pub fn set_wanted(&mut self, mut wanted: Vec<usize>) -> Vec<usize> {
        wanted.truncate(WINDOW_SIZE);
        self.wanted = wanted;
        self.failures.retain(|index, _| self.wanted.contains(index));
        let evicted: Vec<_> = self
            .residents
            .keys()
            .copied()
            .filter(|index| !self.wanted.contains(index))
            .collect();
        for index in &evicted {
            self.bytes -= self.residents.remove(index).unwrap().bytes;
        }
        evicted
    }

    pub fn wanted(&self) -> &[usize] {
        &self.wanted
    }

    pub fn needs(&self, index: usize) -> bool {
        self.wanted.contains(&index)
            && !self.pending.contains(&index)
            && !self.residents.contains_key(&index)
            && !self.failures.contains_key(&index)
    }

    pub fn requested(&mut self, index: usize) {
        self.pending.insert(index);
    }

    pub fn complete(&mut self, generation: u64, index: usize, result: Result<Animation>) {
        if generation != self.generation
            || !self.pending.remove(&index)
            || !self.wanted.contains(&index)
        {
            return;
        }
        let animation = match result {
            Ok(animation) => animation,
            Err(error) => {
                self.failures.insert(index, format!("{error:#}"));
                return;
            }
        };
        // charge vector capacity and a separate rgba copy backing the gtk memory texture.
        let bytes = animation
            .frames
            .iter()
            .fold(0usize, |total, frame| {
                total.saturating_add(frame.rgba.capacity())
            })
            .saturating_add(
                animation
                    .frames
                    .capacity()
                    .saturating_mul(std::mem::size_of::<crate::preview::Frame>()),
            )
            .saturating_add(
                animation
                    .width
                    .saturating_mul(animation.height)
                    .saturating_mul(4),
            );
        if self.bytes.saturating_add(bytes) > MEMORY_BUDGET {
            self.failures
                .insert(index, "preview memory limit reached".into());
            return;
        }
        self.bytes += bytes;
        self.residents.insert(
            index,
            Resident {
                animation,
                frame: 0,
                next_frame: Instant::now(),
                bytes,
            },
        );
    }

    pub fn failure(&self, index: usize) -> Option<&str> {
        self.failures.get(&index).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::Frame;
    use std::time::Duration;

    fn animation() -> Animation {
        Animation {
            width: 160,
            height: 120,
            frames: (0..48)
                .map(|_| Frame {
                    rgba: vec![1; 160 * 120 * 4],
                    delay: Duration::from_millis(100),
                })
                .collect(),
        }
    }

    #[test]
    fn scrolling_evicts_residents_and_reload_waits_for_pending_completion() {
        let mut previews = Previews::default();
        previews.reset(1);
        previews.set_wanted(vec![0, 1]);
        previews.requested(0);
        previews.complete(1, 0, Ok(animation()));
        previews.requested(1);
        assert!(!previews.needs(0) && !previews.needs(1));
        assert_eq!(previews.set_wanted(vec![2]), vec![0]);
        assert!(previews.residents.is_empty() && previews.bytes == 0);
        previews.set_wanted(vec![0, 1]);
        assert!(previews.needs(0));
        assert!(
            !previews.needs(1),
            "returning before completion must not enqueue a duplicate"
        );
        previews.complete(1, 1, Ok(animation()));
        assert!(previews.residents.contains_key(&1));
    }

    #[test]
    fn late_offscreen_and_old_query_results_are_dropped_and_failures_do_not_loop() {
        let mut previews = Previews::default();
        previews.reset(1);
        previews.set_wanted(vec![0]);
        previews.requested(0);
        previews.set_wanted(vec![1]);
        previews.complete(1, 0, Ok(animation()));
        assert!(previews.residents.is_empty());
        previews.set_wanted(vec![0]);
        assert!(previews.needs(0));
        previews.requested(0);
        previews.reset(2);
        previews.set_wanted(vec![0]);
        previews.requested(0);
        previews.complete(1, 0, Ok(animation()));
        assert!(!previews.needs(0) && previews.residents.is_empty());
        previews.complete(2, 0, Err(anyhow::anyhow!("bad preview")));
        assert_eq!(previews.failure(0), Some("bad preview"));
        assert!(!previews.needs(0));
        previews.set_wanted(vec![1]);
        previews.set_wanted(vec![0]);
        assert!(previews.needs(0));
    }

    #[test]
    fn rapid_scroll_and_late_delivery_keep_resident_bytes_bounded() {
        let mut previews = Previews::default();
        previews.reset(1);
        for start in (0..256).step_by(4) {
            previews.set_wanted((start..start + 24).collect());
            for index in previews.wanted().to_vec() {
                if previews.needs(index) {
                    previews.requested(index);
                    previews.complete(1, index, Ok(animation()));
                    assert!(previews.bytes <= MEMORY_BUDGET);
                }
            }
            let late = start + 16;
            previews.requested(late);
            previews.complete(1, late, Ok(animation()));
            assert!(!previews.residents.contains_key(&late));
        }
        assert_eq!(previews.residents.len(), WINDOW_SIZE);
        previews.reset(2);
        assert!(previews.residents.is_empty() && previews.bytes == 0);
    }
}
