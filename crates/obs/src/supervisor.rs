//! Автопереподключение с backoff (F-20, §11 п. 33).

use std::sync::Arc;
use std::time::Duration;

use callscribe_core::config::Obs;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::client::{ObsClient, ObsEvent};
use crate::launch::connect_or_launch;

/// Состояние соединения для трея.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// Идёт (пере)подключение.
    Connecting,
    Connected,
}

/// Экспоненциальная задержка: 1, 2, 4, 8, 16, 30, 30… с.
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
    max: Duration,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            next: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(self.max);
        d
    }

    pub fn reset(&mut self) {
        self.next = Duration::from_secs(1);
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Держит соединение с OBS: подключается (при необходимости запускает OBS),
/// после обрыва переподключается с backoff. Поток событий общий и переживает
/// переподключения: `Disconnected` приходит при обрыве, затем снова `Connected`
/// виден через [`Supervisor::state`].
pub struct Supervisor {
    events: broadcast::Sender<ObsEvent>,
    client: Arc<watch::Sender<Option<ObsClient>>>,
    state: watch::Receiver<ConnState>,
    task: JoinHandle<()>,
}

impl Supervisor {
    /// Запустить фоновую задачу. `launch_wait` — сколько ждать websocket после
    /// запуска OBS (30 с по F-21). Запуск OBS пробуется при каждом цикле, если
    /// процесса нет и `launch_if_not_running`.
    pub fn spawn(cfg: Obs, secret: Option<String>, launch_wait: Duration) -> Supervisor {
        let (events, _) = broadcast::channel(256);
        let (client_tx, _) = watch::channel(None);
        let client_tx = Arc::new(client_tx);
        let (state_tx, state) = watch::channel(ConnState::Connecting);
        let task = tokio::spawn(run_with_reconnect(
            cfg,
            secret,
            launch_wait,
            events.clone(),
            client_tx.clone(),
            state_tx,
        ));
        Supervisor {
            events,
            client: client_tx,
            state,
            task,
        }
    }

    /// Текущий клиент, если соединение есть.
    pub fn client(&self) -> Option<ObsClient> {
        self.client.borrow().clone()
    }

    /// Подписка на события (общая для всех соединений).
    pub fn subscribe(&self) -> broadcast::Receiver<ObsEvent> {
        self.events.subscribe()
    }

    /// Наблюдатель за состоянием соединения.
    pub fn state(&self) -> watch::Receiver<ConnState> {
        self.state.clone()
    }

    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Цикл подключения: работает, пока не отменят задачу.
async fn run_with_reconnect(
    cfg: Obs,
    secret: Option<String>,
    launch_wait: Duration,
    events: broadcast::Sender<ObsEvent>,
    client_tx: Arc<watch::Sender<Option<ObsClient>>>,
    state: watch::Sender<ConnState>,
) {
    let mut backoff = Backoff::new();
    loop {
        // Первое подключение (и каждое следующее) может запустить OBS; события
        // идут в общий канал, поэтому подключаемся через `connect_or_launch`
        // и подменяем канал событий пересылкой.
        match connect_or_launch(&cfg, secret.clone(), launch_wait).await {
            Ok(client) => {
                backoff.reset();
                let mut rx = client.subscribe();
                let fwd = events.clone();
                let forward = tokio::spawn(async move {
                    while let Ok(ev) = rx.recv().await {
                        let _ = fwd.send(ev);
                    }
                });
                client_tx.send_replace(Some(client.clone()));
                let _ = state.send(ConnState::Connected);
                client.closed().await;
                client_tx.send_replace(None);
                let _ = state.send(ConnState::Connecting);
                // Дать пересылке донести `Disconnected`.
                let _ = tokio::time::timeout(Duration::from_millis(200), forward).await;
            }
            Err(e) => tracing::warn!("OBS недоступен: {e:#}"),
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new();
        let v: Vec<u64> = (0..8).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(v, [1, 2, 4, 8, 16, 30, 30, 30]);
        b.reset();
        assert_eq!(b.next_delay().as_secs(), 1);
    }
}
