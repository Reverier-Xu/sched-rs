//! A runtime-agnostic task trigger.

use alloc::boxed::Box;
use alloc::string::ToString;
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
  generation: u64,
}

impl TaskId {
  /// The task's slot index.
  pub fn index(self) -> usize {
    self.index
  }

  /// The slot generation this id was issued for. Not part of the stable API
  /// surface beyond equality comparisons; exposed for diagnostics.
  pub fn generation(self) -> u64 {
    self.generation
  }
}

struct Task {
  schedule: BoundSchedule,
  expr: Box<str>,
  callback: Box<dyn FnMut(Timestamp) -> BoxTaskFuture + Send + 'static>,
  running: Arc<AtomicBool>,
  next: Option<Timestamp>,
}

/// Clears a task's in-flight flag if the callback panics before the
/// [`TaskRun`] that owns the flag is created.
struct RunningGuard {
  running: Arc<AtomicBool>,
  armed: bool,
}

impl RunningGuard {
  fn new(running: Arc<AtomicBool>) -> Self {
    Self {
      running,
      armed: true,
    }
  }

  /// Transfers the flag to the returned [`TaskRun`] without clearing it.
  fn disarm(mut self) -> Arc<AtomicBool> {
    self.armed = false;
    Arc::clone(&self.running)
  }
}

impl Drop for RunningGuard {
  fn drop(&mut self) {
    if self.armed {
      self.running.store(false, Ordering::Release);
    }
  }
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
/// trigger.register("{*}-{*}-{*}T00:00:00", |scheduled_at| async move {
///     // scheduled_at is the nominal trigger time
/// })?;
///
/// loop {
///     let now = Timestamp::now();
///     let Some(scheduled_at) = trigger.next(now) else { break };
///     if scheduled_at > now {
///         sleep_until(scheduled_at).await;  // tokio::time, smol::Timer, ...
///     }
///     let started_at = Timestamp::now();
///     for run in trigger.fire_due(scheduled_at, started_at) {
///         runtime_spawn(run);               // tokio::spawn, smol::spawn, ...
///     }
/// }
/// ```
///
/// Overlap policy: if a task's previous run is still in flight when its next
/// tick fires, the tick is skipped (and logged at `WARN`).
///
/// Each task's first occurrence is computed when it is registered, relative
/// to the clock (or to the explicit `now` passed to [`Trigger::register_at`]).
/// A task whose schedule is already exhausted stores no next occurrence, so
/// `None` from [`Trigger::next`] only means no task will fire again.
///
/// Missed-tick policy: a tick that is already stale when the scheduler
/// dispatches is skipped, not replayed. [`Trigger::next`] may return an
/// instant that is already in the past when the caller is late;
/// [`Trigger::fire_due`] skips it and advances the task past the dispatch
/// start time. Each run receives the nominal trigger time; the callback can
/// call [`Timestamp::now`] for the actual execution time.
#[derive(Default)]
pub struct Trigger {
  /// One slot per (possibly reused) task position. The generation is bumped
  /// on every reuse so stale [`TaskId`]s stop matching.
  tasks: Vec<Slot>,
}

#[derive(Default)]
struct Slot {
  generation: u64,
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
  ///
  /// The task's first occurrence is computed at registration time, relative
  /// to the current clock. Use [`Trigger::register_at`] for deterministic
  /// scheduling, e.g. in tests. In `no_std + alloc` environments there is no
  /// clock, so use [`Trigger::register_at`] instead.
  #[cfg(feature = "std")]
  pub fn register<F, Fut>(&mut self, schedule: &str, callback: F) -> Result<TaskId, RegisterError>
  where
    F: FnMut(Timestamp) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    self.register_at(Timestamp::now(), schedule, callback)
  }

  /// Parses and registers a schedule expression, computing its first
  /// occurrence relative to `now`.
  pub fn register_at<F, Fut>(
    &mut self, now: Timestamp, schedule: &str, callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut(Timestamp) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let parsed: Schedule = schedule.parse()?;
    self.register_schedule_with_expr_at(now, parsed, schedule, callback)
  }

  /// Registers an already-parsed [`Schedule`].
  ///
  /// The task's first occurrence is computed at registration time, relative
  /// to the current clock. Use [`Trigger::register_schedule_at`] for
  /// deterministic scheduling, e.g. in tests. In `no_std + alloc`
  /// environments there is no clock, so use
  /// [`Trigger::register_schedule_at`] instead.
  #[cfg(feature = "std")]
  pub fn register_schedule<F, Fut>(
    &mut self, schedule: Schedule, callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut(Timestamp) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    self.register_schedule_at(Timestamp::now(), schedule, callback)
  }

  /// Registers an already-parsed [`Schedule`], computing its first occurrence
  /// relative to `now`.
  pub fn register_schedule_at<F, Fut>(
    &mut self, now: Timestamp, schedule: Schedule, callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut(Timestamp) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let expr = schedule.to_string();
    self.register_schedule_with_expr_at(now, schedule, &expr, callback)
  }

  fn register_schedule_with_expr_at<F, Fut>(
    &mut self, now: Timestamp, schedule: Schedule, expr: &str, mut callback: F,
  ) -> Result<TaskId, RegisterError>
  where
    F: FnMut(Timestamp) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let bound = schedule.bind_owned()?;
    let next = bound.next_after(now).map(|zoned| zoned.timestamp());
    let task = Task {
      schedule: bound,
      expr: expr.into(),
      callback: Box::new(move |scheduled_at| Box::pin(callback(scheduled_at)) as BoxTaskFuture),
      running: Arc::new(AtomicBool::new(false)),
      next,
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
      next = ?next,
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
  /// Each task's first occurrence is computed when it is registered; `None`
  /// here means no registered task has a future occurrence. The returned
  /// instant may already be in the past if the caller is late; this method
  /// never fires anything and never advances past a pending tick.
  /// [`Trigger::fire_due`] is the only place a run is produced; it skips
  /// stale ticks instead of replaying them.
  pub fn next(&self, now: Timestamp) -> Option<Timestamp> {
    trace!(now = %now, "polling next trigger");
    let mut soonest = None;
    for slot in &self.tasks {
      let Some(task) = &slot.task else { continue };
      if let Some(at) = task.next {
        soonest = Some(soonest.map_or(at, |soonest: Timestamp| soonest.min(at)));
      }
    }
    soonest
  }

  /// Fires the tick scheduled for `scheduled_at` and returns the pending runs.
  ///
  /// `scheduled_at` must be the instant returned by [`Trigger::next`] for this
  /// dispatch round. `started_at` is the dispatch start time, captured once by
  /// the scheduler and used only for staleness checks and advancing tasks.
  /// Every callback receives the nominal trigger time.
  ///
  /// Only the tick at `scheduled_at` may fire. A task whose pending tick is
  /// older, or whose next occurrence is already due at `started_at`, is
  /// skipped and advanced past `started_at`: missed ticks are not replayed.
  /// A task whose previous run is still in flight is skipped and logged at
  /// `WARN`.
  pub fn fire_due(&mut self, scheduled_at: Timestamp, started_at: Timestamp) -> Vec<TaskRun> {
    let mut runs = Vec::new();
    for (index, slot) in self.tasks.iter_mut().enumerate() {
      let generation = slot.generation;
      let Some(task) = &mut slot.task else { continue };
      let Some(at) = task.next else { continue };
      if at > started_at {
        continue;
      }

      // Only the tick the scheduler actually waited for may fire. Any other
      // tick at or before `started_at` is either older or came due during
      // the dispatch delay; skip it without replaying it.
      let next_after_at = if at == scheduled_at {
        task.schedule.next_after(at).map(|zoned| zoned.timestamp())
      } else {
        None
      };
      let stale = matches!(next_after_at, Some(next) if next <= started_at);
      if at != scheduled_at || stale {
        task.next = task
          .schedule
          .next_after(started_at)
          .map(|zoned| zoned.timestamp());
        debug!(
          task_id = index,
          generation,
          schedule = &*task.expr,
          at = %at,
          started_at = %started_at,
          "task skipped: missed tick"
        );
        continue;
      }

      // Advance past the dispatched tick. The next occurrence is strictly
      // after `started_at`, so intermediate missed ticks are not replayed.
      task.next = next_after_at;
      if task.running.swap(true, Ordering::AcqRel) {
        warn!(
          task_id = index,
          generation,
          schedule = &*task.expr,
          at = %at,
          "task skipped: previous run still in flight"
        );
        continue;
      }
      info!(
        task_id = index,
        generation,
        schedule = &*task.expr,
        scheduled_at = %at,
        started_at = %started_at,
        delay = %started_at.duration_since(at),
        "task triggered"
      );
      let guard = RunningGuard::new(Arc::clone(&task.running));
      let future = (task.callback)(at);
      runs.push(TaskRun {
        id: TaskId { index, generation },
        running: guard.disarm(),
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
