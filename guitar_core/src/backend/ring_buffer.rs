use std::{
    fmt::{Debug, Display},
    ops::{Add, Index, IndexMut},
};

pub struct RingBuffer<T> {
    buf: Vec<T>,
    capacity: usize,
    start: usize,
    len: usize,
}

impl<T> RingBuffer<T>
where
    T: Copy + Default + Debug + Display,
{
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buf: vec![T::default(); capacity],
            capacity,
            start: 0,
            len: 0,
        }
    }

    #[allow(dead_code)]
    pub fn push(&mut self, item: T) {
        if self.len >= self.capacity {
            self.buf[self.start] = item;
            self.start = (self.start + 1) % self.capacity;
        } else {
            self.buf[(self.start + self.len) % self.capacity] = item;
            self.len += 1;
        }
    }

    #[allow(dead_code)]
    pub fn iter(&self) -> RingBufferIter<T> {
        RingBufferIter::new(self)
    }

    #[allow(dead_code)]
    pub fn last(&self) -> &T {
        &self.buf[(self.start + self.len - 1) % self.capacity]
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
        let rb: RingBuffer<i32> = RingBuffer::new(4);
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn push_within_capacity() {
        let mut rb: RingBuffer<i32> = RingBuffer::new(4);
        rb.push(10);
        rb.push(20);
        assert_eq!(rb.len(), 2);
        assert_eq!(*rb.last(), 20);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![10, 20]);
    }

    #[test]
    fn push_exact_capacity() {
        let mut rb: RingBuffer<i32> = RingBuffer::new(3);
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
        let mut rb: RingBuffer<i32> = RingBuffer::new(3);
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
        let mut rb: RingBuffer<i32> = RingBuffer::new(3);
        for i in 1..=6 {
            rb.push(i);
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(*rb.last(), 6);

        let collected: Vec<_> = rb.iter().copied().collect();
        assert_eq!(collected, vec![4, 5, 6]);
    }
}

impl<T> Index<usize> for RingBuffer<T>
where
    T: Default + Copy,
{
    type Output = T;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.buf[(self.start + index) % self.capacity]
    }
}

impl<T> IndexMut<usize> for RingBuffer<T>
where
    T: Default + Copy,
    usize: Add<usize>,
{
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.buf[(self.start + index) % self.capacity]
    }
}

pub struct RingBufferIter<'a, T: 'a> {
    ring_buffer: &'a RingBuffer<T>,
    index: usize,
}

impl<'a, T: 'a> RingBufferIter<'a, T> {
    #[allow(dead_code)]
    pub fn new(ring_buffer: &'a RingBuffer<T>) -> Self {
        RingBufferIter {
            ring_buffer,
            index: 0,
        }
    }
}
impl<'a, T> Iterator for RingBufferIter<'a, T>
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
impl<'a, T> DoubleEndedIterator for RingBufferIter<'a, T>
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
