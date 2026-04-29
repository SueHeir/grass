//! Schedule phases for the preCICE step.
//!
//! Within one App tick, the order is:
//! 1. Whatever your physics already did (CFD step, etc.)
//! 2. **Write** — your systems gather outgoing data and call
//!    `Participant::write_data`
//! 3. **Advance** — the plugin's system calls `Participant::advance(dt)`
//!    (this **blocks** until every participant in the coupling reaches the
//!    same time)
//! 4. **Read** — your systems call `Participant::read_data` and apply the
//!    incoming values to App resources
//!
//! Use namespace 200 so this entire block runs after CFD (namespace 0) and
//! after toy_cfd's standard schedule (which lives in namespace 0 too — they
//! interleave by `(namespace, index)`, so a higher namespace runs strictly
//! later within an iteration).

/// preCICE schedule set. Use `app.set_schedule_namespace::<PreciceSchedule>(200)`
/// in your binary main if you want to enforce namespace ordering against
/// other ScheduleSets.
#[derive(Debug, Clone, Copy)]
pub enum PreciceSchedule {
    /// Your systems pack outgoing data here (call `Participant::write_data`).
    Write,
    /// The plugin's system calls `Participant::advance(dt)` here. **Do not
    /// add your own systems to this phase.**
    Advance,
    /// Your systems unpack incoming data here (call `Participant::read_data`).
    Read,
}

impl grass_scheduler::ScheduleSet for PreciceSchedule {
    fn to_index(&self) -> u32 {
        match self {
            PreciceSchedule::Write => 0,
            PreciceSchedule::Advance => 1,
            PreciceSchedule::Read => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            PreciceSchedule::Write => "Write",
            PreciceSchedule::Advance => "Advance",
            PreciceSchedule::Read => "Read",
        }
    }
}
