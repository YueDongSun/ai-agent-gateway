//! Server completion ownership is independent of any individual stop caller.

use tokio::{
    sync::{oneshot, watch},
    task::{AbortHandle, JoinHandle},
};

#[allow(non_camel_case_types)]
pub(crate) struct Proxy_task {
    shutdown: Option<oneshot::Sender<()>>,
    shutdown_signal_sent: bool,
    completion: watch::Receiver<Option<Result<(), String>>>,
    server_status: AbortHandle,
    // Dropping this handle detaches the observer, which must finish joining the server.
    _completion_task: JoinHandle<()>,
}

impl Proxy_task {
    pub(crate) fn new(
        server_task: JoinHandle<Result<(), String>>,
        shutdown: oneshot::Sender<()>,
    ) -> Self {
        let server_status = server_task.abort_handle();
        let (completion_sender, completion) = watch::channel(None);
        let completion_task = tokio::spawn(async move {
            let outcome = server_task
                .await
                .unwrap_or_else(|_| Err("proxy_server_task_join_failed".into()));
            completion_sender.send_replace(Some(outcome));
        });
        Self {
            shutdown: Some(shutdown),
            shutdown_signal_sent: false,
            completion,
            server_status,
            _completion_task: completion_task,
        }
    }

    pub(crate) fn has_finished(&self) -> bool {
        self.server_status.is_finished()
    }

    /// Observe this exact server run without requesting shutdown.
    pub(crate) fn subscribe_completion(&self) -> Proxy_completion {
        Proxy_completion {
            completion: self.completion.clone(),
        }
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.shutdown.is_none()
    }

    pub(crate) fn request_shutdown(&mut self) -> Proxy_shutdown {
        if let Some(shutdown) = self.shutdown.take() {
            self.shutdown_signal_sent = shutdown.send(()).is_ok();
        }
        Proxy_shutdown {
            completion: self.completion.clone(),
            signal_sent: self.shutdown_signal_sent,
        }
    }

    pub(crate) fn matches_shutdown(&self, shutdown: &Proxy_shutdown) -> bool {
        self.completion.same_channel(&shutdown.completion)
    }
}

#[allow(non_camel_case_types)]
#[derive(Clone)]
pub(crate) struct Proxy_completion {
    completion: watch::Receiver<Option<Result<(), String>>>,
}

impl Proxy_completion {
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.completion.same_channel(&other.completion)
    }

    pub(crate) fn has_finished(&self) -> bool {
        self.completion.borrow().is_some() || self.completion.has_changed().is_err()
    }

    pub(crate) async fn wait(mut self) {
        loop {
            if self.completion.borrow().is_some() {
                return;
            }
            if self.completion.changed().await.is_err() {
                return;
            }
        }
    }
}

#[allow(non_camel_case_types)]
pub(crate) struct Proxy_shutdown {
    completion: watch::Receiver<Option<Result<(), String>>>,
    pub(crate) signal_sent: bool,
}

impl Proxy_shutdown {
    pub(crate) async fn wait(&mut self) -> Result<(), String> {
        loop {
            let outcome = self.completion.borrow().clone();
            if let Some(outcome) = outcome {
                return outcome;
            }
            self.completion
                .changed()
                .await
                .map_err(|_| "proxy_server_task_join_failed".to_string())?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn shutdown_waiters_share_the_terminal_result() {
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let (observed, shutdown_observed) = oneshot::channel();
        let (release, drain) = oneshot::channel();
        let server = tokio::spawn(async move {
            shutdown_receiver.await.unwrap();
            observed.send(()).unwrap();
            drain.await.unwrap();
            Err("proxy_server_failed".into())
        });
        let mut task = Proxy_task::new(server, shutdown);
        let mut first = task.request_shutdown();
        let mut second = task.request_shutdown();
        assert!(first.signal_sent && second.signal_sent);
        assert!(task.is_stopping());
        assert!(task.matches_shutdown(&first) && task.matches_shutdown(&second));
        tokio::time::timeout(Duration::from_secs(2), shutdown_observed)
            .await
            .unwrap()
            .unwrap();
        assert!(!task.has_finished());
        release.send(()).unwrap();
        let results = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(first.wait(), second.wait())
        })
        .await
        .unwrap();
        assert_eq!(results.0, Err("proxy_server_failed".into()));
        assert_eq!(results.0, results.1);
        assert!(task.has_finished());
    }

    #[tokio::test]
    async fn dropped_waiters_do_not_cancel_server_completion() {
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let (observed, shutdown_observed) = oneshot::channel();
        let (release, drain) = oneshot::channel();
        let server = tokio::spawn(async move {
            shutdown_receiver.await.unwrap();
            observed.send(()).unwrap();
            drain.await.unwrap();
            Ok(())
        });
        let mut task = Proxy_task::new(server, shutdown);
        drop(task.request_shutdown());
        tokio::time::timeout(Duration::from_secs(2), shutdown_observed)
            .await
            .unwrap()
            .unwrap();
        let mut later = task.request_shutdown();
        assert!(later.signal_sent);
        release.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), later.wait())
                .await
                .unwrap(),
            Ok(())
        );
    }

    #[tokio::test]
    async fn server_task_panics_have_a_fixed_join_failure_code() {
        let (shutdown, _shutdown_receiver) = oneshot::channel();
        let server = tokio::spawn(async { panic!("fixture server interruption") });
        let mut task = Proxy_task::new(server, shutdown);
        let result = tokio::time::timeout(Duration::from_secs(2), task.request_shutdown().wait())
            .await
            .unwrap();
        assert_eq!(result, Err("proxy_server_task_join_failed".into()));
        assert!(task.has_finished());
    }

    #[tokio::test]
    async fn completion_identity_distinguishes_replacement_servers() {
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let (shutdown, shutdown_receiver) = oneshot::channel();
            let server = tokio::spawn(async move {
                let _ = shutdown_receiver.await;
                Ok(())
            });
            tasks.push(Proxy_task::new(server, shutdown));
        }
        let mut first = tasks[0].request_shutdown();
        assert!(!tasks[1].matches_shutdown(&first));
        let mut second = tasks[1].request_shutdown();
        assert!(tasks[1].matches_shutdown(&second));
        tokio::time::timeout(Duration::from_secs(2), async {
            first.wait().await.unwrap();
            second.wait().await.unwrap();
        })
        .await
        .unwrap();
    }
}
