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

    pub fn push(&mut self, item: T) {
        if self.len >= N {
            self.arr[self.start] = item;
            self.start = (self.start + 1) % N;
        } else {
            self.arr[(self.start + self.len) % N] = item;
            self.len += 1;
        }
    }

    pub fn iter(&self) -> RingBufferIter<N, T> {
        RingBufferIter::new(&self)
    }

    pub fn last(&self) -> &T {
        &self.arr[(self.start + self.len - 1) % N]
    }

    pub fn len(&self) -> usize {
        self.len
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
        let r = if self.index >= 0 {
            Some(&self.ring_buffer[self.index])
        } else {
            None
        };
        self.index -= 1;
        r
    }
}
