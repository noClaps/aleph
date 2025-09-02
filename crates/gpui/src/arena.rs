use std::{
    alloc::{self, handle_alloc_error},
    cell::Cell,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::{self, NonNull},
    rc::Rc,
};

struct ArenaElement {
    value: *mut u8,
    drop: unsafe fn(*mut u8),
}

impl Drop for ArenaElement {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            (self.drop)(self.value);
        }
    }
}

struct Chunk {
    start: *mut u8,
    end: *mut u8,
    offset: *mut u8,
}

impl Drop for Chunk {
    fn drop(&mut self) {
        unsafe {
            let chunk_size = self.end.offset_from_unsigned(self.start);
            // SAFETY: This succeeded during allocation.
            let layout = alloc::Layout::from_size_align_unchecked(chunk_size, 1);
            alloc::dealloc(self.start, layout);
        }
    }
}

impl Chunk {
    fn new(chunk_size: NonZeroUsize) -> Self {
        unsafe {
            // this only fails if chunk_size is unreasonably huge
            let layout = alloc::Layout::from_size_align(chunk_size.get(), 1).unwrap();
            let start = alloc::alloc(layout);
            if start.is_null() {
                handle_alloc_error(layout);
            }
            let end = start.add(chunk_size.get());
            Self {
                start,
                end,
                offset: start,
            }
        }
    }

    fn allocate(&mut self, layout: alloc::Layout) -> Option<NonNull<u8>> {
        unsafe {
            let aligned = self.offset.add(self.offset.align_offset(layout.align()));
            let next = aligned.add(layout.size());

            if next <= self.end {
                self.offset = next;
                NonNull::new(aligned)
            } else {
                None
            }
        }
    }

    fn reset(&mut self) {
        self.offset = self.start;
    }
}

pub struct Arena {
    chunks: Vec<Chunk>,
    elements: Vec<ArenaElement>,
    valid: Rc<Cell<bool>>,
    current_chunk_index: usize,
    chunk_size: NonZeroUsize,
}

impl Drop for Arena {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Arena {
    pub fn new(chunk_size: usize) -> Self {
        let chunk_size = NonZeroUsize::try_from(chunk_size).unwrap();
        Self {
            chunks: vec![Chunk::new(chunk_size)],
            elements: Vec::new(),
            valid: Rc::new(Cell::new(true)),
            current_chunk_index: 0,
            chunk_size,
        }
    }

    pub fn capacity(&self) -> usize {
        self.chunks.len() * self.chunk_size.get()
    }

    pub fn clear(&mut self) {
        self.valid.set(false);
        self.valid = Rc::new(Cell::new(true));
        self.elements.clear();
        for chunk_index in 0..=self.current_chunk_index {
            self.chunks[chunk_index].reset();
        }
        self.current_chunk_index = 0;
    }

    #[inline(always)]
    pub fn alloc<T>(&mut self, f: impl FnOnce() -> T) -> ArenaBox<T> {
        #[inline(always)]
        unsafe fn inner_writer<T, F>(ptr: *mut T, f: F)
        where
            F: FnOnce() -> T,
        {
            unsafe {
                ptr::write(ptr, f());
            }
        }

        unsafe fn drop<T>(ptr: *mut u8) {
            unsafe {
                std::ptr::drop_in_place(ptr.cast::<T>());
            }
        }

        unsafe {
            let layout = alloc::Layout::new::<T>();
            let mut current_chunk = &mut self.chunks[self.current_chunk_index];
            let ptr = if let Some(ptr) = current_chunk.allocate(layout) {
                ptr.as_ptr()
            } else {
                self.current_chunk_index += 1;
                if self.current_chunk_index >= self.chunks.len() {
                    self.chunks.push(Chunk::new(self.chunk_size));
                    assert_eq!(self.current_chunk_index, self.chunks.len() - 1);
                    log::trace!(
                        "increased element arena capacity to {}kb",
                        self.capacity() / 1024,
                    );
                }
                current_chunk = &mut self.chunks[self.current_chunk_index];
                if let Some(ptr) = current_chunk.allocate(layout) {
                    ptr.as_ptr()
                } else {
                    panic!(
                        "Arena chunk_size of {} is too small to allocate {} bytes",
                        self.chunk_size,
                        layout.size()
                    );
                }
            };

            inner_writer(ptr.cast(), f);
            self.elements.push(ArenaElement {
                value: ptr,
                drop: drop::<T>,
            });

            ArenaBox {
                ptr: ptr.cast(),
                valid: self.valid.clone(),
            }
        }
    }
}

pub struct ArenaBox<T: ?Sized> {
    ptr: *mut T,
    valid: Rc<Cell<bool>>,
}

impl<T: ?Sized> ArenaBox<T> {
    #[inline(always)]
    pub fn map<U: ?Sized>(mut self, f: impl FnOnce(&mut T) -> &mut U) -> ArenaBox<U> {
        ArenaBox {
            ptr: f(&mut self),
            valid: self.valid,
        }
    }

    #[track_caller]
    fn validate(&self) {
        assert!(
            self.valid.get(),
            "attempted to dereference an ArenaRef after its Arena was cleared"
        );
    }
}

impl<T: ?Sized> Deref for ArenaBox<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.validate();
        unsafe { &*self.ptr }
    }
}

impl<T: ?Sized> DerefMut for ArenaBox<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.validate();
        unsafe { &mut *self.ptr }
    }
}
