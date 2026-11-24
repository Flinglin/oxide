use std::{
    collections::VecDeque,
    sync::{Mutex, TryLockError},
};

use cpal::{FromSample, Sample};

const PCM_BYTES_PER_SAMPLE: usize = size_of::<i16>();

/// Buffers complete PCM frames without waiting in producers or callbacks.
#[derive(Debug)]
pub(crate) struct BoundedPcmQueue {
    samples: Mutex<VecDeque<i16>>,
    capacity_samples: usize,
    channels: usize,
}

impl BoundedPcmQueue {
    /// Creates a queue whose capacity cannot split an interleaved frame.
    pub(crate) fn new(capacity_samples: usize, channels: usize) -> Self {
        let channels = channels.max(1);
        let aligned_capacity = capacity_samples - capacity_samples % channels;
        Self {
            samples: Mutex::new(VecDeque::with_capacity(aligned_capacity)),
            capacity_samples: aligned_capacity,
            channels,
        }
    }

    /// Appends complete frames and drops the oldest complete frames on overflow.
    pub(crate) fn push_pcm(&self, bytes: &[u8]) {
        let mut samples = match self.samples.try_lock() {
            Ok(samples) => samples,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let bytes_per_frame = self.channels * PCM_BYTES_PER_SAMPLE;
        let complete_bytes = bytes.len() - bytes.len() % bytes_per_frame;
        let retained_bytes = self.capacity_samples * PCM_BYTES_PER_SAMPLE;
        let first_byte = complete_bytes.saturating_sub(retained_bytes);
        let incoming_samples = (complete_bytes - first_byte) / PCM_BYTES_PER_SAMPLE;
        let overflow_samples = samples
            .len()
            .saturating_add(incoming_samples)
            .saturating_sub(self.capacity_samples);
        let samples_to_drop = overflow_samples.div_ceil(self.channels) * self.channels;

        for _ in 0..samples_to_drop.min(samples.len()) {
            samples.pop_front();
        }
        for sample in bytes[first_byte..complete_bytes].chunks_exact(PCM_BYTES_PER_SAMPLE) {
            samples.push_back(i16::from_le_bytes([sample[0], sample[1]]));
        }
    }

    /// Fills the real-time callback and writes silence instead of waiting.
    pub(crate) fn fill<T>(&self, output: &mut [T])
    where
        T: Sample + FromSample<i16>,
    {
        let mut samples = match self.samples.try_lock() {
            Ok(samples) => Some(samples),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        for output_sample in output {
            let pcm_sample = samples
                .as_mut()
                .and_then(|samples| samples.pop_front())
                .unwrap_or(0);
            *output_sample = T::from_sample(pcm_sample);
        }
    }

    /// Clears pending frames after the device callback has stopped.
    pub(crate) fn clear(&self) {
        match self.samples.lock() {
            Ok(mut samples) => samples.clear(),
            Err(error) => error.into_inner().clear(),
        }
    }
}
