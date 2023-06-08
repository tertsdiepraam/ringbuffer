use core::ops::{Index, IndexMut};

use crate::ringbuffer_trait::{RingBuffer, RingBufferExt, RingBufferRead, RingBufferWrite};

extern crate alloc;

// We need boxes, so depend on alloc
use crate::GrowableAllocRingBuffer;
use core::iter::FromIterator;
use core::marker::PhantomData;
use core::ptr;

#[derive(Debug, Copy, Clone)]
pub struct PowerOfTwo;

#[derive(Debug, Copy, Clone)]
pub struct NonPowerOfTwo;
mod private {
    use crate::with_alloc::alloc_ringbuffer::{NonPowerOfTwo, PowerOfTwo};

    pub trait Sealed {}

    impl Sealed for PowerOfTwo {}
    impl Sealed for NonPowerOfTwo {}
}

pub trait RingbufferSize: private::Sealed {
    fn mask(cap: usize, index: usize) -> usize;
    fn must_be_power_of_two() -> bool;
}

impl RingbufferSize for PowerOfTwo {
    #[inline]
    fn mask(cap: usize, index: usize) -> usize {
        crate::mask(cap, index)
    }

    fn must_be_power_of_two() -> bool {
        true
    }
}

impl RingbufferSize for NonPowerOfTwo {
    #[inline]
    fn mask(cap: usize, index: usize) -> usize {
        crate::mask_modulo(cap, index)
    }

    fn must_be_power_of_two() -> bool {
        false
    }
}

/// The `AllocRingBuffer` is a `RingBufferExt` which is based on a Vec. This means it allocates at runtime
/// on the heap, and therefore needs the [`alloc`] crate. This struct and therefore the dependency on
/// alloc can be disabled by disabling the `alloc` (default) feature.
///
/// # Example
/// ```
/// use ringbuffer::{AllocRingBuffer, RingBuffer, RingBufferExt, RingBufferWrite};
///
/// let mut buffer = AllocRingBuffer::with_capacity(2);
///
/// // First entry of the buffer is now 5.
/// buffer.push(5);
///
/// // The last item we pushed is 5
/// assert_eq!(buffer.get(-1), Some(&5));
///
/// // Second entry is now 42.
/// buffer.push(42);
///
/// assert_eq!(buffer.peek(), Some(&5));
/// assert!(buffer.is_full());
///
/// // Because capacity is reached the next push will be the first item of the buffer.
/// buffer.push(1);
/// assert_eq!(buffer.to_vec(), vec![42, 1]);
/// ```
#[derive(Debug)]
pub struct AllocRingBuffer<T, SIZE: RingbufferSize = PowerOfTwo> {
    buf: *mut T,
    capacity: usize,
    readptr: usize,
    writeptr: usize,
    mode: PhantomData<SIZE>,
}

impl<T, const N: usize> From<[T; N]> for AllocRingBuffer<T, NonPowerOfTwo> {
    fn from(value: [T; N]) -> Self {
        let mut rb = Self::with_capacity_non_power_of_two(value.len());
        rb.extend(value.into_iter());
        rb
    }
}

impl<T: Clone, const N: usize> From<&[T; N]> for AllocRingBuffer<T, NonPowerOfTwo> {
    // the cast here is actually not trivial
    #[allow(trivial_casts)]
    fn from(value: &[T; N]) -> Self {
        Self::from(value as &[T])
    }
}

impl<T: Clone> From<&[T]> for AllocRingBuffer<T, NonPowerOfTwo> {
    fn from(value: &[T]) -> Self {
        let mut rb = Self::with_capacity_non_power_of_two(value.len());
        rb.extend(value.iter().cloned());
        rb
    }
}

impl<T> From<GrowableAllocRingBuffer<T>> for AllocRingBuffer<T, NonPowerOfTwo> {
    fn from(mut v: GrowableAllocRingBuffer<T>) -> AllocRingBuffer<T, NonPowerOfTwo> {
        let mut rb = AllocRingBuffer::with_capacity_non_power_of_two(v.len());
        rb.extend(v.drain());
        rb
    }
}

impl<T: Clone> From<&mut [T]> for AllocRingBuffer<T, NonPowerOfTwo> {
    fn from(value: &mut [T]) -> Self {
        Self::from(&*value)
    }
}

impl<T, SIZE: RingbufferSize> Drop for AllocRingBuffer<T, SIZE> {
    fn drop(&mut self) {
        self.drain().for_each(drop);

        let layout = alloc::alloc::Layout::array::<T>(self.capacity).unwrap();
        unsafe {
            alloc::alloc::dealloc(self.buf as *mut u8, layout);
        }
    }
}

impl<T: Clone, SIZE: RingbufferSize> Clone for AllocRingBuffer<T, SIZE> {
    fn clone(&self) -> Self {
        debug_assert_ne!(self.capacity, 0);
        debug_assert!(!SIZE::must_be_power_of_two() || self.capacity.is_power_of_two());

        // whatever the previous capacity was, we can just use the same one again.
        // It should be valid.
        let mut new = unsafe { Self::with_capacity_unchecked(self.capacity) };
        self.iter().cloned().for_each(|i| new.push(i));
        new
    }
}

impl<T: PartialEq, SIZE: RingbufferSize> PartialEq for AllocRingBuffer<T, SIZE> {
    fn eq(&self, other: &Self) -> bool {
        self.capacity == other.capacity
            && self.len() == other.len()
            && self.iter().zip(other.iter()).all(|(a, b)| a == b)
    }
}

impl<T: Eq + PartialEq, SIZE: RingbufferSize> Eq for AllocRingBuffer<T, SIZE> {}

/// The capacity of a `RingBuffer` created by new or default (`1024`).
// must be a power of 2
pub const RINGBUFFER_DEFAULT_CAPACITY: usize = 1024;

unsafe impl<T, SIZE: RingbufferSize> RingBufferExt<T> for AllocRingBuffer<T, SIZE> {
    impl_ringbuffer_ext!(
        get_unchecked,
        get_unchecked_mut,
        readptr,
        writeptr,
        SIZE::mask
    );

    #[inline]
    fn fill_with<F: FnMut() -> T>(&mut self, mut f: F) {
        self.clear();

        self.readptr = 0;
        self.writeptr = self.capacity;

        for i in 0..self.capacity {
            unsafe { ptr::write(get_unchecked_mut(self, i), f()) };
        }
    }
}

impl<T, SIZE: RingbufferSize> RingBufferRead<T> for AllocRingBuffer<T, SIZE> {
    fn dequeue(&mut self) -> Option<T> {
        if self.is_empty() {
            None
        } else {
            let index = SIZE::mask(self.capacity, self.readptr);
            let res = unsafe { get_unchecked_mut(self, index) };
            self.readptr += 1;

            // Safety: the fact that we got this maybeuninit from the buffer (with mask) means that
            // it's initialized. If it wasn't the is_empty call would have caught it. Values
            // are always initialized when inserted so this is safe.
            unsafe { Some(ptr::read(res)) }
        }
    }

    impl_ringbuffer_read!();
}

impl<T, SIZE: RingbufferSize> Extend<T> for AllocRingBuffer<T, SIZE> {
    fn extend<A: IntoIterator<Item = T>>(&mut self, iter: A) {
        let iter = iter.into_iter();

        for i in iter {
            self.push(i);
        }
    }
}

impl<T, SIZE: RingbufferSize> RingBufferWrite<T> for AllocRingBuffer<T, SIZE> {
    #[inline]
    fn push(&mut self, value: T) {
        if self.is_full() {
            let previous_value = unsafe {
                ptr::read(get_unchecked_mut(
                    self,
                    SIZE::mask(self.capacity, self.readptr),
                ))
            };

            // make sure we drop whatever is being overwritten
            // SAFETY: the buffer is full, so this must be initialized
            //       : also, index has been masked
            // make sure we drop because it won't happen automatically
            unsafe {
                drop(previous_value);
            }

            self.readptr += 1;
        }

        let index = SIZE::mask(self.capacity, self.writeptr);

        unsafe {
            ptr::write(get_unchecked_mut(self, index), value);
        }

        self.writeptr += 1;
    }
}

impl<T, SIZE: RingbufferSize> RingBuffer<T> for AllocRingBuffer<T, SIZE> {
    #[inline]
    unsafe fn ptr_capacity(rb: *const Self) -> usize {
        (*rb).capacity
    }

    impl_ringbuffer!(readptr, writeptr);
}

impl<T, SIZE: RingbufferSize> AllocRingBuffer<T, SIZE> {
    /// Creates a `AllocRingBuffer` with a certain capacity. This capacity is fixed.
    /// for this ringbuffer to work, cap must be a power of two and greater than zero.
    ///
    /// # Safety
    /// Only safe if the capacity is greater than zero, and a power of two.
    /// Only if `MODE` == [`NonPowerOfTwo`](NonPowerOfTwo) can the capacity be not a power of two, in which case this function is also safe.
    #[inline]
    unsafe fn with_capacity_unchecked(cap: usize) -> Self {
        let layout = alloc::alloc::Layout::array::<T>(cap).unwrap();
        let buf = unsafe { alloc::alloc::alloc(layout) as *mut T };

        Self {
            buf,
            capacity: cap,
            readptr: 0,
            writeptr: 0,
            mode: PhantomData,
        }
    }
}

impl<T> AllocRingBuffer<T, NonPowerOfTwo> {
    /// Creates a `AllocRingBuffer` with a certain capacity. This capacity is fixed.
    /// for this ringbuffer to work, and must not be zero.
    ///
    /// Note, that not using a power of two means some operations can't be optimized as well.
    /// For example, bitwise ands might become modulos.
    ///
    /// For example, on push operations, benchmarks have shown that a ringbuffer with a power-of-two
    /// capacity constructed with `with_capacity_non_power_of_two` (so which don't get the same optimization
    /// as the ones constructed with `with_capacity`) can be up to 3x slower
    ///
    /// # Panics
    /// if the capacity is zero
    #[inline]
    #[must_use]
    pub fn with_capacity_non_power_of_two(cap: usize) -> Self {
        assert_ne!(cap, 0, "Capacity must be greater than 0");

        // Safety: Mode is NonPowerOfTwo and we checked above that the capacity isn't zero
        unsafe { Self::with_capacity_unchecked(cap) }
    }
}

impl<T> AllocRingBuffer<T, PowerOfTwo> {
    /// Creates a `AllocRingBuffer` with a certain capacity. The actual capacity is the input to the
    /// function raised to the power of two (effectively the input is the log2 of the actual capacity)
    #[inline]
    #[must_use]
    pub fn with_capacity_power_of_2(cap_power_of_two: usize) -> Self {
        // Safety: 1 << n is always a power of two, and nonzero
        unsafe { Self::with_capacity_unchecked(1 << cap_power_of_two) }
    }

    #[inline]
    /// Creates a `AllocRingBuffer` with a certain capacity. The capacity must be a power of two.
    /// # Panics
    /// Panics when capacity is zero or not a power of two
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        assert_ne!(cap, 0, "Capacity must be greater than 0");
        assert!(cap.is_power_of_two(), "Capacity must be a power of two");

        // Safety: assertions check that cap is a power of two and nonzero
        unsafe { Self::with_capacity_unchecked(cap) }
    }

    /// Creates an `AllocRingBuffer` with a capacity of [`RINGBUFFER_DEFAULT_CAPACITY`].
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Get a reference from the buffer without checking it is initialized.
/// Caller must be sure the index is in bounds, or this will panic.
#[inline]
unsafe fn get_unchecked<'a, T, SIZE: RingbufferSize>(
    rb: *const AllocRingBuffer<T, SIZE>,
    index: usize,
) -> &'a T {
    let p = (*rb).buf.add(index);
    // Safety: caller makes sure the index is in bounds for the ringbuffer.
    // All in bounds values in the ringbuffer are initialized
    &*p
}

/// Get a mut reference from the buffer without checking it is initialized.
/// Caller must be sure the index is in bounds, or this will panic.
#[inline]
unsafe fn get_unchecked_mut<T, SIZE: RingbufferSize>(
    rb: *mut AllocRingBuffer<T, SIZE>,
    index: usize,
) -> *mut T {
    let p = (*rb).buf.add(index);

    // Safety: caller makes sure the index is in bounds for the ringbuffer.
    // All in bounds values in the ringbuffer are initialized
    p.cast()
}

impl<RB, SIZE: RingbufferSize> FromIterator<RB> for AllocRingBuffer<RB, SIZE> {
    fn from_iter<T: IntoIterator<Item = RB>>(iter: T) -> Self {
        let mut res = Self::default();
        for i in iter {
            res.push(i);
        }

        res
    }
}

impl<T, SIZE: RingbufferSize> Default for AllocRingBuffer<T, SIZE> {
    /// Creates a buffer with a capacity of [`RINGBUFFER_DEFAULT_CAPACITY`].
    #[inline]
    fn default() -> Self {
        // SAFETY: RINGBUFFER_DEFAULT_CAPACITY is a power of two
        unsafe { AllocRingBuffer::with_capacity_unchecked(RINGBUFFER_DEFAULT_CAPACITY) }
    }
}

impl<T, SIZE: RingbufferSize> Index<isize> for AllocRingBuffer<T, SIZE> {
    type Output = T;

    fn index(&self, index: isize) -> &Self::Output {
        self.get(index).expect("index out of bounds")
    }
}

impl<T, SIZE: RingbufferSize> IndexMut<isize> for AllocRingBuffer<T, SIZE> {
    fn index_mut(&mut self, index: isize) -> &mut Self::Output {
        self.get_mut(index).expect("index out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::alloc::vec::Vec;
    use crate::with_alloc::alloc_ringbuffer::RingbufferSize;
    use crate::{
        AllocRingBuffer, RingBuffer, RingBufferExt, RingBufferRead, RingBufferWrite,
        RINGBUFFER_DEFAULT_CAPACITY,
    };

    // just test that this compiles
    #[test]
    fn test_generic_clone() {
        fn helper<SIZE: RingbufferSize>(
            a: &AllocRingBuffer<i32, SIZE>,
        ) -> AllocRingBuffer<i32, SIZE> {
            a.clone()
        }

        _ = helper(&AllocRingBuffer::with_capacity(2));
        _ = helper(&AllocRingBuffer::with_capacity_non_power_of_two(5));
    }

    #[test]
    fn test_not_power_of_two() {
        let mut rb = AllocRingBuffer::with_capacity_non_power_of_two(10);
        const NUM_VALS: usize = 1000;

        // recycle the ringbuffer a bunch of time to see if noneof the logic
        // messes up
        for _ in 0..100 {
            for i in 0..NUM_VALS {
                rb.enqueue(i);
            }
            assert!(rb.is_full());

            for i in 0..10 {
                assert_eq!(Some(i + NUM_VALS - rb.capacity()), rb.dequeue())
            }

            assert!(rb.is_empty())
        }
    }

    #[test]
    fn test_default() {
        let b: AllocRingBuffer<u32> = AllocRingBuffer::default();
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, b.capacity());
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, b.capacity);
        assert_eq!(b.capacity, b.capacity());
        assert_eq!(0, b.writeptr);
        assert_eq!(0, b.readptr);
        assert!(b.is_empty());
        assert_eq!(0, b.iter().count());
        assert_eq!(
            Vec::<u32>::with_capacity(RINGBUFFER_DEFAULT_CAPACITY),
            b.to_vec()
        );
    }

    #[test]
    fn test_default_capacity_constant() {
        // This is to prevent accidentally changing it.
        assert_eq!(RINGBUFFER_DEFAULT_CAPACITY, 1024)
    }

    #[test]
    fn test_with_capacity_power_of_two() {
        let b = AllocRingBuffer::<i32>::with_capacity_power_of_2(2);
        assert_eq!(b.capacity, 4);
    }

    #[test]
    #[should_panic]
    fn test_with_capacity_no_power_of_two() {
        let _ = AllocRingBuffer::<i32>::with_capacity(10);
    }

    #[test]
    #[should_panic]
    fn test_index_zero_length() {
        let b = AllocRingBuffer::<i32>::with_capacity(2);
        let _ = b[2];
    }

    #[test]
    fn test_extend() {
        let mut buf = AllocRingBuffer::<u8>::with_capacity(4);
        (0..4).for_each(|_| buf.push(0));

        let new_data = [0, 1, 2];
        buf.extend(new_data);

        let expected = [0, 0, 1, 2];

        for i in 0..4 {
            let actual = buf[i as isize];
            let expected = expected[i];
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn test_extend_with_overflow() {
        let mut buf = AllocRingBuffer::<u8>::with_capacity(8);
        (0..8).for_each(|_| buf.push(0));

        let new_data = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        buf.extend(new_data);

        let expected = [2, 3, 4, 5, 6, 7, 8, 9];

        for i in 0..8 {
            let actual = buf[i as isize];
            let expected = expected[i];
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn test_conversions() {
        // from &[T]
        let data: &[i32] = &[1, 2, 3, 4];
        let buf = AllocRingBuffer::from(data);
        assert_eq!(buf.capacity, 4);
        assert_eq!(buf.to_vec(), alloc::vec![1, 2, 3, 4]);

        // from &[T; N]
        let buf = AllocRingBuffer::from(&[1, 2, 3, 4]);
        assert_eq!(buf.capacity, 4);
        assert_eq!(buf.to_vec(), alloc::vec![1, 2, 3, 4]);

        // from [T; N]
        let buf = AllocRingBuffer::from([1, 2, 3, 4]);
        assert_eq!(buf.capacity, 4);
        assert_eq!(buf.to_vec(), alloc::vec![1, 2, 3, 4]);
    }
}
