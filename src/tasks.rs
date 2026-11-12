//! Operaciones lentas (adb, simctl, copias de archivos) en hilos aparte para que la interfaz
//! siga respondiendo mientras terminan.

use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskKind {
    DetectDevices,
    DetectApps,
    DetectDatabases,
    Pull,
    Push,
    CheckUpdate,
    InstallUpdate,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::DetectDevices => "Buscando dispositivos…",
            Self::DetectApps => "Detectando apps…",
            Self::DetectDatabases => "Buscando bases de datos…",
            Self::Pull => "Extrayendo base de datos…",
            Self::Push => "Enviando al dispositivo…",
            Self::CheckUpdate => "Buscando actualizaciones…",
            Self::InstallUpdate => "Instalando actualización…",
        }
    }
}

struct Running<T> {
    kind: TaskKind,
    receiver: Receiver<T>,
}

pub struct Tasks<T> {
    ctx: egui::Context,
    running: Vec<Running<T>>,
}

impl<T: Send + 'static> Tasks<T> {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            running: Vec::new(),
        }
    }

    /// Lanza `job` en un hilo nuevo y pide un repintado al terminar para recoger el resultado.
    pub fn spawn(&mut self, kind: TaskKind, job: impl FnOnce() -> T + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(job());
            ctx.request_repaint();
        });
        self.running.push(Running { kind, receiver });
    }

    pub fn is_running(&self, kind: TaskKind) -> bool {
        self.running.iter().any(|task| task.kind == kind)
    }

    pub fn running(&self) -> impl Iterator<Item = TaskKind> + '_ {
        self.running.iter().map(|task| task.kind)
    }

    /// Recoge las tareas terminadas. `None` indica que el hilo terminó sin resultado (panic).
    pub fn finished(&mut self) -> Vec<(TaskKind, Option<T>)> {
        let mut done = Vec::new();
        self.running.retain(|task| match task.receiver.try_recv() {
            Ok(output) => {
                done.push((task.kind, Some(output)));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((task.kind, None));
                false
            }
        });
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for<T: Send + 'static>(tasks: &mut Tasks<T>) -> Vec<(TaskKind, Option<T>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let done = tasks.finished();
            if !done.is_empty() || Instant::now() > deadline {
                return done;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn returns_the_job_result_and_forgets_the_task() {
        let mut tasks = Tasks::new(egui::Context::default());
        tasks.spawn(TaskKind::Pull, || 42);
        assert!(tasks.is_running(TaskKind::Pull));

        let done = wait_for(&mut tasks);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, TaskKind::Pull);
        assert_eq!(done[0].1, Some(42));
        assert!(!tasks.is_running(TaskKind::Pull));
    }

    #[test]
    fn a_panicking_job_is_reported_without_result() {
        let mut tasks = Tasks::<u8>::new(egui::Context::default());
        tasks.spawn(TaskKind::Push, || panic!("fallo simulado"));

        let done = wait_for(&mut tasks);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0], (TaskKind::Push, None));
    }
}
