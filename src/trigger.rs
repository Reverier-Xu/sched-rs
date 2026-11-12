//! A runtime-agnostic task trigger.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, Poll};

use jiff::Timestamp;
use tracing::{debug, info, trace, warn};

use crate::BindError;
use crate::bind::BoundSchedule;
use crate::error::ParseError;
use crate::schedule::Schedule;

/// A boxed task future, ready to spawn onto any async runtime.
type BoxTaskFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// A registered task's handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId(usize);

impl TaskId {
  /// The task's slot index.
  pub fn index(self) -> usize {
    self.0
  }
}

struct Task {
  schedule: BoundSchedule,
  expr: Box<str>,
  callback: Box<dyn FnMut() -> BoxTaskFuture + Send + 'static>,
  running: Arc<AtomicBool>,
  next: Option<Timestamp>,
}

/// A pending task run produced by [`Trigger::fire_due`].
///
/// `TaskRun` is itself a future — spawn it onto the host runtime. The task's
/// in-flight flag is cleared when the future completes *or* when the run is
/// dropped (e.g. the spawn was cancelled), so a cancelled run never wedges
/// the task.
pub struct TaskRun {
  id: TaskId,
  running: Arc<AtomicBool>,
  future: BoxTaskFuture,
}

impl TaskRun {
  /// The task this run belongs to.
  pub fn id(&self) -> TaskId {
    self.id
  }
}

impl Future for TaskRun {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    match self.future.as_mut().poll(cx) {
      Poll::Ready(()) => {
        self.running.store(false, Ordering::Release);
        trace!(task_id = self.id.0, "task run finished");
        Poll::Ready(())
      }
      Poll::Pending => Poll::Pending,
    }
  }
}

impl Drop for TaskRun {
  fn drop(&mut self) {
    self.running.store(false, Ordering::Release);
  }
}

impl fmt::Debug for TaskRun {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TaskRun")
      .field("id", &self.id)
      .finish_non_exhaustive()
  }
}

/// A table of scheduled tasks.
///
/// `Trigger` computes trigger times and produces [`TaskRun`] futures; it
/// never touches an async runtime itself. The host runtime drives it:
///
/// ```text
/// let mut trigger = Trigger::new();
/// trigger.register("{*}-{*}-{*}T00:00:00", || async { /* ... */ })?;
///
/// loop {
///     let Some(at) = trigger.next(Timestamp::now()) else { break };
///     sleep_until(at).await;              // tokio::time, smol::Timer, ...
///     for run in trigger.fire_due(Timestamp::now()) {
///         runtime_spawn(run);             // tokio::spawn, smol::spawn, ...
///     }
/// }
/// ```
///
/// Overlap policy: if a task's previous run is still in flight when its next
/// tick fires, the tick is skipped (and logged at `WARN`). Missed ticks
/// (e.g. the runtime slept past them) fire once — there is no catch-up.
#[derive(Default)]
pub struct Trigger {
  tasks: Vec<Option<Task>>,
}

impl Trigger {
  /// An empty trigger.
  pub fn new() -> Self {
    Self::default()
  }

  /// Number of registered tasks.
  pub fn len(&self) -> usize {
    self.tasks.iter().flatten().count()
  }

  /// Returns `true` if no tasks are registered.
  pub fn is_empty(&self) -> bool {
    self.tasks.iter().all(Option::is_none)
  }

  /// Parses and registers a schedule expression with an async callback.
  ///
  /// The callback is invoked when a tick fires; the future it returns
  /// performs the actual work once spawned onto the host runtime.
  pub fn register<F, Fut>(&mut self, schedule: &str, callback: F) -> Result<TaskId, RegisterError>
  where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let parsed: Schedule = schedule.parse()?;
    self.register_schedule_with_expr(parsed, schedule, callback)
  }

  /// Registers an already-parsed [`Schedule`].
  pub fn register_schedule<F, Fut>(
    &mut self, schedule: Schedule, callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    self.register_schedule_with_expr(schedule, "<unnamed>", callback)
  }

  fn register_schedule_with_expr<F, Fut>(
    &mut self, schedule: Schedule, expr: &str, mut callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let bound = schedule.bind()?;
    let task = Task {
      schedule: bound,
      expr: expr.into(),
      callback: Box::new(move || Box::pin(callback()) as BoxTaskFuture),
      running: Arc::new(AtomicBool::new(false)),
      next: None,
    };
    let id = match self.tasks.iter().position(Option::is_none) {
      Some(index) => {
        self.tasks[index] = Some(task);
        TaskId(index)
      }
      None => {
        self.tasks.push(Some(task));
        TaskId(self.tasks.len() - 1)
      }
    };
    debug!(task_id = id.0, schedule = expr, "task registered");
    Ok(id)
  }

  /// Removes a task. Returns `false` if the id is stale.
  pub fn deregister(&mut self, id: TaskId) -> bool {
    match self.tasks.get_mut(id.0) {
      Some(slot @ Some(_)) => {
        *slot = None;
        debug!(task_id = id.0, "task deregistered");
        true
      }
      _ => false,
    }
  }

  /// The soonest pending trigger time, if any.
  ///
  /// Computes each task's next occurrence lazily on the first call after
  /// registration. Note this never revisits elapsed ticks: once a task's
  /// cached time passes, only [`Trigger::fire_due`] fires and advances it.
  pub fn next(&mut self, now: Timestamp) -> Option<Timestamp> {
    let mut soonest = None;
    for task in self.tasks.iter_mut().flatten() {
      if task.next.is_none() {
        task.next = task.schedule.next_after(now).map(|zoned| zoned.timestamp());
        trace!(schedule = &*task.expr, next = ?task.next, "computed next occurrence");
      }
      if let Some(at) = task.next {
        soonest = Some(soonest.map_or(at, |soonest: Timestamp| soonest.min(at)));
      }
    }
    soonest
  }

  /// Fires every task due at `now` and returns the pending runs.
  ///
  /// Each due task's next occurrence is recomputed from `now` (missed
  /// intermediate ticks are skipped, not replayed). A task whose previous
  /// run is still in flight is skipped and logged at `WARN`.
  pub fn fire_due(&mut self, now: Timestamp) -> Vec<TaskRun> {
    let mut runs = Vec::new();
    for (index, slot) in self.tasks.iter_mut().enumerate() {
      let Some(task) = slot else { continue };
      if task.next.is_none() {
        task.next = task.schedule.next_after(now).map(|zoned| zoned.timestamp());
      }
      let Some(at) = task.next else { continue };
      if at > now {
        continue;
      }
      task.next = task.schedule.next_after(now).map(|zoned| zoned.timestamp());
      if task.running.swap(true, Ordering::AcqRel) {
        warn!(task_id = index, schedule = &*task.expr, at = %at, "task skipped: previous run still in flight");
        continue;
      }
      info!(task_id = index, schedule = &*task.expr, at = %at, "task triggered");
      let future = (task.callback)();
      runs.push(TaskRun {
        id: TaskId(index),
        running: Arc::clone(&task.running),
        future,
      });
    }
    runs
  }
}

/// An error that occurred while registering a task.
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
  /// The schedule expression is malformed.
  #[error(transparent)]
  Parse(#[from] ParseError),
  /// The schedule's time zone declaration could not be resolved.
  #[error(transparent)]
  Bind(#[from] BindError),
}
