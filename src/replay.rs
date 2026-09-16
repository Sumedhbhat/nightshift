//! Synthetic load for the foundation milestone. No execution state lives here.
use std::{collections::VecDeque, time::Duration};
use tokio::{runtime::Handle, sync::mpsc, task::JoinHandle};

pub const QUEUE_CAPACITY: usize = 256;
pub const CACHE_CAPACITY: usize = 2_048;
pub const ISSUE_COUNT: usize = 10_000;
pub const UPDATE_INTERVAL: Duration = Duration::from_millis(33);

// Fixed-size messages bound queue memory by bytes as well as count.
#[derive(Clone, Copy, Debug)]
pub struct OutputBlock {
    pub stream: u8,
    pub sequence: u64,
    pub payload: [u8; 128],
}

pub struct OutputCache {
    pub blocks: VecDeque<OutputBlock>,
    pub received: u64,
}

impl Default for OutputCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputCache {
    pub fn new() -> Self {
        Self {
            blocks: VecDeque::with_capacity(CACHE_CAPACITY),
            received: 0,
        }
    }

    pub fn push(&mut self, block: OutputBlock) {
        if self.blocks.len() == CACHE_CAPACITY {
            self.blocks.pop_front();
        }
        self.blocks.push_back(block);
        self.received += 1;
    }
}

pub struct Replay {
    receiver: mpsc::Receiver<OutputBlock>,
    tasks: Vec<JoinHandle<()>>,
}

impl Replay {
    pub fn start(runtime: &Handle) -> Self {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let tasks = (1..=2)
            .map(|stream| {
                let sender = sender.clone();
                runtime.spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(10));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut sequence = 0;
                    loop {
                        interval.tick().await;
                        sequence += 1;
                        let block = OutputBlock {
                            stream,
                            sequence,
                            payload: [b'x'; 128],
                        };
                        // Backpressure is fine for synthetic data. Real output needs storage spooling.
                        if sender.send(block).await.is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        Self { receiver, tasks }
    }

    pub fn drain_into(&mut self, cache: &mut OutputCache) -> usize {
        let mut drained = 0;
        // Bound foreground work even if producers refill while we drain.
        while drained < QUEUE_CAPACITY {
            let Ok(block) = self.receiver.try_recv() else {
                break;
            };
            cache.push(block);
            drained += 1;
        }
        drained
    }
}

impl Drop for Replay {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keeps_recent_blocks_under_sustained_output() {
        let mut cache = OutputCache::new();
        for sequence in 0..120_000 {
            cache.push(OutputBlock {
                stream: 1,
                sequence,
                payload: [b'x'; 128],
            });
        }
        assert_eq!(cache.received, 120_000);
        assert_eq!(cache.blocks.len(), CACHE_CAPACITY);
        assert_eq!(
            cache.blocks.front().unwrap().sequence,
            120_000 - CACHE_CAPACITY as u64
        );
        assert_eq!(cache.blocks.back().unwrap().sequence, 119_999);
    }

    #[test]
    fn both_streams_deliver_and_dropping_replay_stops_producers() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut replay = Replay::start(runtime.handle());
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut cache = OutputCache::new();
            assert!(replay.drain_into(&mut cache) > 0);
            assert!(cache.blocks.iter().any(|b| b.stream == 1));
            assert!(cache.blocks.iter().any(|b| b.stream == 2));
            // Keep abort handles to check the actual Drop implementation.
            let handles: Vec<_> = replay.tasks.iter().map(|t| t.abort_handle()).collect();
            drop(replay);
            tokio::task::yield_now().await;
            assert!(handles.iter().all(|h| h.is_finished()));
        });
    }

    #[test]
    fn stalled_consumer_has_a_bounded_queue_and_recovers() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
            for sequence in 0..QUEUE_CAPACITY {
                sender
                    .try_send(OutputBlock {
                        stream: 1,
                        sequence: sequence as u64,
                        payload: [0; 128],
                    })
                    .unwrap();
            }
            assert!(matches!(
                sender.try_send(OutputBlock {
                    stream: 1,
                    sequence: 999,
                    payload: [0; 128]
                }),
                Err(mpsc::error::TrySendError::Full(_))
            ));
            let mut replay = Replay {
                receiver,
                tasks: vec![],
            };
            let mut cache = OutputCache::new();
            assert_eq!(replay.drain_into(&mut cache), QUEUE_CAPACITY);
            assert_eq!(replay.drain_into(&mut cache), 0);
            assert_eq!(cache.received, QUEUE_CAPACITY as u64);
        });
    }
}
