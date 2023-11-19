use std::ops::{Deref, DerefMut};

use arc_swap::RefCnt;
use triomphe::Arc;

pub(crate) struct ArcWrap<T>(pub Option<Arc<T>>);

unsafe impl<T> RefCnt for ArcWrap<T> {
    type Base = T;

    fn into_ptr(this: Self) -> *mut T {
        match this.0 {
            Some(this) => Arc::into_raw(this) as *mut T,
            None => std::ptr::null_mut(),
        }
    }

    fn as_ptr(this: &Self) -> *mut T {
        match &this.0 {
            Some(this) => Arc::as_ptr(this) as *mut T,
            None => std::ptr::null_mut(),
        }
    }

    unsafe fn from_ptr(ptr: *const T) -> Self {
        if ptr.is_null() {
            Self(None)
        } else {
            Self(Some(Arc::from_raw(ptr)))
        }
    }
}

impl<T> Clone for ArcWrap<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for ArcWrap<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Deref for ArcWrap<T> {
    type Target = Option<Arc<T>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for ArcWrap<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<T> From<Option<Arc<T>>> for ArcWrap<T> {
    fn from(value: Option<Arc<T>>) -> Self {
        Self(value)
    }
}

impl<T> From<Arc<T>> for ArcWrap<T> {
    fn from(value: Arc<T>) -> Self {
        Some(value).into()
    }
}
