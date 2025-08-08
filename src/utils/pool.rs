use std::{
    mem::ManuallyDrop,
    sync::{Mutex, mpsc},
};

pub struct Pool<T> {
    tx: mpsc::Sender<()>,
    rx: Mutex<mpsc::Receiver<()>>,
    stack: Mutex<Vec<T>>,
}

pub struct LifeGuard<'a, T> {
    pool: &'a Pool<T>,
    value: ManuallyDrop<T>,
}

impl<'a, T> std::ops::Deref for LifeGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<'a, T> Drop for LifeGuard<'a, T> {
    fn drop(&mut self) {
        self.pool
            .push(unsafe { ManuallyDrop::take(&mut self.value) });
    }
}

impl<T> Pool<T> {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let rx = Mutex::new(rx);
        let stack = Mutex::new(vec![]);
        Self { tx, rx, stack }
    }

    pub fn pull(&self) -> LifeGuard<'_, T> {
        let _ = self.rx.lock().unwrap().recv().unwrap();
        let v = self.stack.lock().unwrap().pop().unwrap();
        LifeGuard {
            pool: &self,
            value: ManuallyDrop::new(v),
        }
    }

    pub fn push(&self, v: T) {
        self.stack.lock().unwrap().push(v);
        self.tx.send(()).unwrap();
    }
}
