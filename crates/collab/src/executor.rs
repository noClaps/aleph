use std::{future::Future, time::Duration};

#[derive(Clone)]
pub enum Executor {
    Production,
}

impl Executor {
    pub fn spawn_detached<F>(&self, future: F)
    where
        F: 'static + Send + Future<Output = ()>,
    {
        match self {
            Executor::Production => {
                tokio::spawn(future);
            }
        }
    }

    pub fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + use<> {
        let this = self.clone();
        async move {
            match this {
                Executor::Production => tokio::time::sleep(duration).await,
            }
        }
    }
}
