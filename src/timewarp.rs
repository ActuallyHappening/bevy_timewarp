use crate::systems::*;
use bevy::prelude::*;

use super::*;
use std::{ops::Range, time::Duration};

#[derive(Component)]
pub struct NotRollbackable;

/// the marker component that tells us this entity is to be rendered in the past
/// based on snapshots received from the server.
#[derive(Component, Clone, Debug)]
pub struct Anachronous {
    pub frames_behind: FrameNumber,
    /// frame of last server snapshot update.
    /// used in calculation for frame lag for anachronous entities
    newest_snapshot_frame: FrameNumber,
    /// frame number of most recent input command for this entity
    /// used in calculation for frame lag for anachronous entities
    newest_input_frame: FrameNumber,
}
impl Anachronous {
    pub fn new(frames_behind: FrameNumber) -> Self {
        Self {
            frames_behind,
            newest_snapshot_frame: 0,
            newest_input_frame: 0,
        }
    }
    pub fn newest_snapshot_frame(&self) -> FrameNumber {
        self.newest_snapshot_frame
    }
    pub fn newest_input_frame(&self) -> FrameNumber {
        self.newest_input_frame
    }
    pub fn set_newest_snapshot_frame(&mut self, newest_frame: FrameNumber) -> bool {
        if newest_frame > self.newest_snapshot_frame {
            self.newest_snapshot_frame = newest_frame;
            true
        } else {
            false
        }
    }
    pub fn set_newest_input_frame(&mut self, newest_frame: FrameNumber) -> bool {
        if newest_frame > self.newest_input_frame {
            self.newest_input_frame = newest_frame;
            true
        } else {
            false
        }
    }
}

#[derive(Resource, Debug, Default)]
pub struct RollbackStats {
    pub num_rollbacks: u64,
}

/// if this resource exists, we are doing a rollback. Insert it to initate one.
#[derive(Resource, Debug, Clone)]
pub struct Rollback {
    /// the range of frames, start being the target we rollback to
    pub range: Range<FrameNumber>,
    /// we preserve the original FixedUpdate period here and restore after rollback completes.
    /// (during rollback, we set the FixedUpdate period to 0.0, to effect fast-forward resimulation)
    pub original_period: Option<Duration>,
}
impl Rollback {
    /// The range start..end contains all values with start <= x < end. It is empty if start >= end.
    pub fn new(start: FrameNumber, end: FrameNumber) -> Self {
        Self {
            range: Range { start, end },
            original_period: None,
        }
    }
}

#[derive(Resource, Debug)]
pub struct PreviousRollback(pub Rollback);

/// used to record component birth/death ranges in ComponentHistory.
/// (start, end) – can be open-ended if end is None.
pub type FrameRange = (FrameNumber, Option<FrameNumber>);

/// Used when you want to insert a component T, but for an older frame.
/// insert this to an entity for an older frame will trigger a rollback.
#[derive(Component, Debug)]
pub struct InsertComponentAtFrame<T: Component + Clone + std::fmt::Debug> {
    pub component: T,
    pub frame: FrameNumber,
}
impl<T: Component + Clone + std::fmt::Debug> InsertComponentAtFrame<T> {
    pub fn new(frame: FrameNumber, component: T) -> Self {
        Self { component, frame }
    }
}

/// entities with components that were registered with error correction logging will receive
/// one of these components, updated with before/after values when a simulation correction
/// resulting from a rollback and resimulate causes a snap.
/// ie, the values before and after the rollback differ.
/// in your game, look for Changed<TimewarpCorrection<T>> and use for any visual smoothing/interp stuff.
#[derive(Component, Debug, Clone)]
pub struct TimewarpCorrection<T: Component + Clone + std::fmt::Debug> {
    pub before: T,
    pub after: T,
    pub frame: FrameNumber,
}

/// Buffers the last few authoritative component values received from the server
#[derive(Component)]
pub struct ServerSnapshot<T: Component + Clone + std::fmt::Debug> {
    pub values: FrameBuffer<T>,
}
impl<T: Component + Clone + std::fmt::Debug> ServerSnapshot<T> {
    pub fn with_capacity(len: usize) -> Self {
        Self {
            values: FrameBuffer::with_capacity(len),
        }
    }
    pub fn at_frame(&self, frame: FrameNumber) -> Option<&T> {
        self.values.get(frame)
    }
    pub fn insert(&mut self, frame: FrameNumber, val: T) {
        // TODO this should never be allowed to fail?
        self.values.insert(frame, val);
    }
}

/// Buffers component values for the last few frames.
#[derive(Component)]
pub struct ComponentHistory<T: Component + Clone + std::fmt::Debug> {
    pub values: FrameBuffer<T>,        // not pub!
    pub alive_ranges: Vec<FrameRange>, // inclusive! unlike std:range
    /// when we insert at this frame, compute diff between newly inserted val and whatever already exists in the buffer.
    /// this is for visual smoothing post-rollback.
    /// (if the simulation is perfect the diff would be zero, but will be unavoidably non-zero when dealing with collisions between anachronous entities for example.)
    pub diff_at_frame: Option<FrameNumber>,
    pub correction_logging_enabled: bool,
}

// lazy first version - don't need a clone each frame if value hasn't changed!
// just store once and reference from each unchanged frame number.
impl<T: Component + Clone + std::fmt::Debug> ComponentHistory<T> {
    pub fn with_capacity(len: usize, birth_frame: FrameNumber) -> Self {
        let mut this = Self {
            values: FrameBuffer::with_capacity(len),
            alive_ranges: Vec::new(),
            diff_at_frame: None,
            correction_logging_enabled: false,
        };
        this.report_birth_at_frame(birth_frame);
        this
    }
    /// will compute and insert `TimewarpCorrection`s when snapping
    pub fn enable_correction_logging(&mut self) {
        self.correction_logging_enabled = true;
    }
    pub fn at_frame(&self, frame: FrameNumber) -> Option<&T> {
        self.values.get(frame)
    }
    // adding entity just for debugging print outs.
    pub fn insert(&mut self, frame: FrameNumber, val: T, entity: &Entity) {
        trace!("CH.Insert {entity:?} {frame} = {val:?}");
        self.values.insert(frame, val);
    }
    /// removes values buffered for this frame, and greater frames.
    pub fn remove_frame_and_beyond(&mut self, frame: FrameNumber) {
        self.values
            .remove_entries_newer_than(frame.saturating_sub(1));
    }
    pub fn alive_at_frame(&self, frame: FrameNumber) -> bool {
        for (start, maybe_end) in &self.alive_ranges {
            if *start <= frame && (maybe_end.is_none() || maybe_end.unwrap() > frame) {
                return true;
            }
        }
        false
    }
    pub fn report_birth_at_frame(&mut self, frame: FrameNumber) {
        debug!("component birth @ {frame} {:?}", std::any::type_name::<T>());
        self.alive_ranges.push((frame, None));
    }
    pub fn report_death_at_frame(&mut self, frame: FrameNumber) {
        debug!("component death @ {frame} {:?}", std::any::type_name::<T>());
        self.alive_ranges.last_mut().unwrap().1 = Some(frame);
    }
}

/// trait for registering components with the rollback system.
pub trait TimewarpTraits {
    /// register component for rollback
    fn register_rollback<T: Component + Clone + std::fmt::Debug>(&mut self) -> &mut Self;
    /// register component for rollback, and also update a TimewarpCorrection<T> component when snapping
    fn register_rollback_with_correction_logging<T: Component + Clone + std::fmt::Debug>(
        &mut self,
    ) -> &mut Self;
    /// register component for rollback with additional options
    fn register_rollback_with_options<
        T: Component + Clone + std::fmt::Debug,
        const CORRECTION_LOGGING: bool,
    >(
        &mut self,
    ) -> &mut Self;
}

impl TimewarpTraits for App {
    fn register_rollback<T: Component + Clone + std::fmt::Debug>(&mut self) -> &mut Self {
        self.register_rollback_with_options::<T, false>()
    }
    fn register_rollback_with_correction_logging<T: Component + Clone + std::fmt::Debug>(
        &mut self,
    ) -> &mut Self {
        self.register_rollback_with_options::<T, true>()
    }
    fn register_rollback_with_options<
        T: Component + Clone + std::fmt::Debug,
        const CORRECTION_LOGGING: bool,
    >(
        &mut self,
    ) -> &mut Self {
        self
            // we want to record frame values even if we're about to rollback -
            // we need values pre-rb to diff against post-rb versions.
            // ---
            // TimewarpSet::RecordComponentValues
            // * Runs always
            // ---
            .add_systems(
                FixedUpdate,
                (
                    add_timewarp_buffer_components::<T, CORRECTION_LOGGING>,
                    // Recording component births. this does the Added<> query, and bails if in rollback
                    // so that the Added query is refreshed.
                    record_component_birth::<T>,
                    record_component_history::<T>,
                    insert_components_at_prior_frames::<T>,
                    remove_components_from_despawning_entities::<T>
                        .after(record_component_history::<T>)
                        .after(add_frame_to_freshly_added_despawn_markers),
                )
                    .in_set(TimewarpSet::RecordComponentValues),
            )
            // ---
            // TimewarpSet::RollbackUnderwayComponents
            // * run_if(resource_exists(Rollback))
            // ---
            .add_systems(
                FixedUpdate,
                (
                    apply_snapshot_to_component_if_available::<T>,
                    rekill_components_during_rollback::<T>,
                    rebirth_components_during_rollback::<T>,
                    clear_removed_components_queue::<T>
                        .after(rekill_components_during_rollback::<T>),
                )
                    .in_set(TimewarpSet::RollbackUnderwayComponents),
            )
            // ---
            // TimewarpSet::RollbackInitiated
            // * run_if(resource_added(Rollback))
            // ---
            .add_systems(
                FixedUpdate,
                rollback_component::<T>
                    .after(rollback_initiated)
                    .in_set(TimewarpSet::RollbackInitiated),
            )
            // ---
            // TimewarpSet::NoRollback
            // * run_if(not(resource_exists(Rollback)))
            // ---
            .add_systems(
                FixedUpdate,
                (
                    record_component_death::<T>,
                    apply_snapshot_to_component_if_available::<T>,
                    trigger_rollback_when_snapshot_added::<T>,
                )
                    .before(consolidate_rollback_requests)
                    .in_set(TimewarpSet::NoRollback),
            )
    }
}
