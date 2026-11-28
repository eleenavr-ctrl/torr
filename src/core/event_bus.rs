use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum AppEvent {
    Toast(String),
    ConfigUpdated,
    SyncStatusChanged(String),
    SyncConflictDetected(String),
    DetachedWindowClosed(i32),
}

type Subscriber = Box<dyn Fn(&AppEvent) + Send + Sync>;

#[derive(Default, Clone)]
pub struct EventBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe<F>(&self, handler: F)
    where
        F: Fn(&AppEvent) + Send + Sync + 'static,
    {
        if let Ok(mut guard) = self.subscribers.lock() {
            guard.push(Box::new(handler));
        }
    }

    pub fn publish(&self, event: AppEvent) {
        if let Ok(guard) = self.subscribers.lock() {
            for sub in guard.iter() {
                sub(&event);
            }
        }
    }
}
