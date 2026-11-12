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
///
/// Carries a generation counter alongside the slot index so that a stale id
/// (from a deregistered task whose slot was reused) can never address the
/// new occupant of that slot. [`Trigger::deregister`] compares both parts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId {
  index: usize,
  generation: u32,
}

impl TaskId {
  /// The task's slot index.
  pub fn index(self) -> usize {
    self.index
  }

  /// The slot generation this id was issued for. Not part of the stable API
  /// surface beyond equality comparisons; exposed for diagnostics.
  pub fn generation(self) -> u32 {
    self.generation
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
        trace!(task_id = self.id.index, "task run finished");
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
///
/// [`Trigger::next`] may return an instant that is already in the past when
/// the caller is late; [`Trigger::fire_due`] advances the task and fires that
/// overdue tick once.
#[derive(Default)]
pub struct Trigger {
  /// One slot per (possibly reused) task position. The generation is bumped
  /// on every reuse so stale [`TaskId`]s stop matching.
  tasks: Vec<Slot>,
}

#[derive(Default)]
struct Slot {
  generation: u32,
  task: Option<Task>,
}

impl Trigger {
  /// An empty trigger.
  pub fn new() -> Self {
    Self::default()
  }

  /// Number of registered tasks.
  pub fn len(&self) -> usize {
    self.tasks.iter().filter(|slot| slot.task.is_some()).count()
  }

  /// Returns `true` if no tasks are registered.
  pub fn is_empty(&self) -> bool {
    self.tasks.iter().all(|slot| slot.task.is_none())
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
    let id = match self.tasks.iter().position(|slot| slot.task.is_none()) {
      Some(index) => {
        let slot = &mut self.tasks[index];
        // Bump on reuse so any id handed out for the previous occupant is
        // stale from here on.
        slot.generation = slot.generation.wrapping_add(1);
        slot.task = Some(task);
        TaskId {
          index,
          generation: slot.generation,
        }
      }
      None => {
        let index = self.tasks.len();
        self.tasks.push(Slot {
          generation: 0,
          task: Some(task),
        });
        TaskId {
          index,
          generation: 0,
        }
      }
    };
    debug!(
      task_id = id.index,
      generation = id.generation,
      schedule = expr,
      "task registered"
    );
    Ok(id)
  }

  /// Removes a task. Returns `false` if the id is stale — either the slot is
  /// empty, or it has since been reused by a different registration.
  pub fn deregister(&mut self, id: TaskId) -> bool {
    match self.tasks.get_mut(id.index) {
      Some(slot) if slot.generation == id.generation && slot.task.is_some() => {
        slot.task = None;
        debug!(
          task_id = id.index,
          generation = id.generation,
          "task deregistered"
        );
        true
      }
      _ => false,
    }
  }

  /// The soonest pending trigger time, if any.
  ///
  /// Computes each task's next occurrence lazily on first use. The returned
  /// instant may already be in the past if the caller is late; this method
  /// never fires anything and never advances past a pending tick.
  /// [`Trigger::fire_due`] is the only place a run is produced, and it
  /// collapses all elapsed ticks into a single fire.
  pub fn next(&mut self, now: Timestamp) -> Option<Timestamp> {
    let mut soonest = None;
    for slot in &mut self.tasks {
      let Some(task) = &mut slot.task else { continue };
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
      let generation = slot.generation;
      let Some(task) = &mut slot.task else { continue };
      // Compute the pending instant lazily on first use. `next` may already
      // have done this; either way, only elapsed ticks are fired.
      if task.next.is_none() {
        task.next = task.schedule.next_after(now).map(|zoned| zoned.timestamp());
      }
      let Some(at) = task.next else { continue };
      if at > now {
        continue;
      }
      // Advance past `now` so a later `fire_due` does not re-fire this tick.
      // Missed intermediate ticks are collapsed, not replayed.
      task.next = task.schedule.next_after(now).map(|zoned| zoned.timestamp());
      if task.running.swap(true, Ordering::AcqRel) {
        warn!(task_id = index, generation, schedule = &*task.expr, at = %at, "task skipped: previous run still in flight");
        continue;
      }
      info!(task_id = index, generation, schedule = &*task.expr, at = %at, "task triggered");
      let future = (task.callback)();
      runs.push(TaskRun {
        id: TaskId { index, generation },
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
