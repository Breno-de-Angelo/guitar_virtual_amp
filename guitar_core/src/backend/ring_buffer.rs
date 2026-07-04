use std::{
    fmt::{Debug, Display},
    ops::{Add, Index, IndexMut},
};

pub struct RingBuffer<const N: usize, T> {
    arr: [T; N],
    start: usize,
    len: usize,
}

impl<const N: usize, T> RingBuffer<N, T>
where
    T: Copy + Default + Debug + Display,
{
    pub fn new() -> Self {
        Self {
            arr: [T::default(); N],
            start: 0,
            len: 0,
        }
    }

    #[allow(dead_code)]
    pub fn push(&mut self, item: T) {
        if self.len >= N {
            self.arr[self.start] = item;
            self.start = (self.start + 1) % N;
        } else {
            self.arr[(self.start + self.len) % N] = item;
            self.len += 1;
        }
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> RingBufferIter<N, T> {
        RingBufferIter::new(self)
    }

    #[allow(dead_code)]
    pub fn last(&self) -> &T {
        &self.arr[(self.start + self.len - 1) % N]
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_empty() {
        let rb: RingBuffer<4, i32> = RingBuffer::new();
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn push_within_capacity() {
        let mut rb: RingBuffer<4, i32> = RingBuffer::new();
        rb.push(10);
        rb.push(20);
        assert_eq!(rb.len(), 2);
        assert_eq!(*rb.last(), 20);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![10, 20]);
    }

    #[test]
    fn push_exact_capacity() {
        let mut rb: RingBuffer<3, i32> = RingBuffer::new();
        rb.push(1);
        rb.push(2);
        rb.push(3);
        assert_eq!(rb.len(), 3);
        assert_eq!(*rb.last(), 3);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![1, 2, 3]);
    }

    #[test]
    fn push_over_capacity_overwrites_oldest() {
        let mut rb: RingBuffer<3, i32> = RingBuffer::new();
        rb.push(1);
        rb.push(2);
        rb.push(3);
        rb.push(4); // overwrites 1
        assert_eq!(rb.len(), 3);
        assert_eq!(*rb.last(), 4);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![2, 3, 4]);
    }

    #[test]
    fn multiple_overwrites_keep_order() {
        let mut rb: RingBuffer<3, i32> = RingBuffer::new();
        for i in 1..=6 {
            rb.push(i);
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(*rb.last(), 6);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![4, 5, 6]);
    }
}

impl<T, const N: usize> Index<usize> for RingBuffer<N, T>
where
    [T]: Index<usize>,
    T: Default + Copy,
{
    type Output = <[T] as Index<usize>>::Output;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.arr[(self.start + index) % N]
    }
}

impl<T, const N: usize> IndexMut<usize> for RingBuffer<N, T>
where
    [T]: Index<usize>,
    T: Default + Copy,
    usize: Add<usize>,
{
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.arr[(self.start + index) % N]
    }
}

pub struct RingBufferIter<'a, const N: usize, T: 'a> {
    ring_buffer: &'a RingBuffer<N, T>,
    index: usize,
}

impl<'a, const N: usize, T: 'a> RingBufferIter<'a, N, T> {
    #[allow(dead_code)]
    pub fn new(ring_buffer: &'a RingBuffer<N, T>) -> Self {
        RingBufferIter {
            ring_buffer,
            index: 0,
        }
    }
}
impl<'a, const N: usize, T> Iterator for RingBufferIter<'a, N, T>
where
    T: Default + Copy,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let r = if self.index < self.ring_buffer.len {
            Some(&self.ring_buffer[self.index])
        } else {
            None
        };
        self.index += 1;
        r
    }
}
impl<'a, const N: usize, T> DoubleEndedIterator for RingBufferIter<'a, N, T>
where
    T: Default + Copy,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        let r = if self.index < self.ring_buffer.len {
            Some(&self.ring_buffer[self.index])
        } else {
            None
        };
        if self.index == 0 {
            self.index = self.ring_buffer.len;
        } else {
            self.index -= 1;
        }
        r
    }
}
