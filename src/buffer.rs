use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const BUFFER_SIZE: usize = 4096;

pub struct AudioBuffer<T> {
    data: Arc<Vec<T>>,
    index: AtomicUsize,
}

impl<T> AudioBuffer<T>
where
    T: Copy + Default,
{
    pub fn new() -> Self {
        Self {
            data: vec![T::default(); BUFFER_SIZE],
            index: AtomicUsize::new(0),
        }
    }

    pub fn push(&mut self, sample: T) {
        let i = self.index.fetch_add(1, Ordering::Relaxed) % BUFFER_SIZE;
        self.data[i] = sample;
    }

    /// Returns the latest `n` samples and the start index split if wrapped.
    /// If the buffer hasn't wrapped yet, the second slice will be empty.
    /// If `n` is larger than the buffer size, it will return the entire buffer
    pub fn latest<'a>(&'a self, n: usize) -> (&'a [T], &'a [T]) {
        let head = self.index.load(Ordering::Relaxed);
        if n > BUFFER_SIZE {
            return (&self.data, &[]);
        }
        if head >= n {
            (&self.data[head - n..head], &[])
        } else {
            let part1 = &self.data[BUFFER_SIZE + head - n..BUFFER_SIZE];
            let part2 = &self.data[0..head];
            (part1, part2)
        }
    }
}
