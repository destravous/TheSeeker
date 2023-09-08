use bevy::asset::Asset;
use bevy::ecs::system::{StaticSystemParam, SystemParam};

use crate::prelude::*;

pub mod common;
pub mod label;

pub struct ScriptPlugin;

impl Plugin for ScriptPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            GameTickUpdate,
            (
                ScriptSet::Init.after(AssetsSet::ResolveKeysFlush),
                ScriptSet::InitFlush.after(ScriptSet::Init),
                ScriptSet::Run.after(ScriptSet::InitFlush),
                ScriptSet::RunFlush.after(ScriptSet::Run),
            ),
        );
        app.add_systems(
            GameTickUpdate,
            (
                apply_deferred.in_set(ScriptSet::InitFlush),
                apply_deferred.in_set(ScriptSet::RunFlush),
            ),
        );
        app.add_plugins((
            self::label::ScriptLabelPlugin,
            self::common::CommonScriptPlugin,
        ));
    }
}

/// Use this for system ordering relative to scripts
/// (within the `GameTickUpdate` schedule)
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptSet {
    /// This is when scripts get initialized (the `ScriptRuntime` component added to entities)
    Init,
    InitFlush,
    /// This is when scripts get run/updated
    Run,
    RunFlush,
}

/// Resource to track when the current game level was entered/loaded
#[derive(Resource)]
pub struct LevelLoadTime {
    /// The time since app startup, when the level was spawned
    pub time: Duration,
    /// The GameTime Tick
    pub tick: u64,
}

pub trait ScriptAppExt {
    fn add_script_runtime<T: ScriptAsset>(&mut self) -> &mut Self;
}

impl ScriptAppExt for App {
    fn add_script_runtime<T: ScriptAsset>(&mut self) -> &mut Self {
        self.add_systems(
            GameTickUpdate,
            (
                script_init_system::<T>.in_set(ScriptSet::Init),
                script_driver_system::<T>.in_set(ScriptSet::Run),
            ),
        );
        self
    }
}

pub type ActionId = usize;

pub trait ScriptAsset: Asset + Sized + Send + Sync + 'static {
    type Settings;
    type RunIf: ScriptRunIf<Tracker = Self::Tracker>;
    type Action: ScriptAction<Tracker = Self::Tracker>;
    type Tracker: ScriptTracker<RunIf = Self::RunIf, Settings = Self::Settings>;

    fn init<'w>(
        &self,
        entity: Entity,
        settings: &Self::Settings,
        param: &mut <<Self::Tracker as ScriptTracker>::InitParam as SystemParam>::Item<'w, '_>,
    ) -> ScriptRuntime<Self>;

    fn into_settings(&self) -> Self::Settings;
}

pub trait ScriptTracker: Default + Send + Sync + 'static {
    type RunIf: ScriptRunIf;
    type Settings;
    type InitParam: SystemParam + 'static;
    type UpdateParam: SystemParam + 'static;

    fn init<'w>(
        &mut self,
        entity: Entity,
        settings: &Self::Settings,
        param: &mut <Self::InitParam as SystemParam>::Item<'w, '_>,
    );
    fn track_action(&mut self, run_if: &Self::RunIf, action_id: ActionId);
    fn finalize(&mut self);
    fn update<'w>(
        &mut self,
        entity: Entity,
        param: &mut <Self::UpdateParam as SystemParam>::Item<'w, '_>,
        queue: &mut Vec<ActionId>,
    ) -> ScriptUpdateResult;
}

pub trait ScriptRunIf: Clone + Send + Sync + 'static {
    type Tracker: ScriptTracker;
}

pub trait ScriptAction: Clone + Send + Sync + 'static {
    type Tracker: ScriptTracker;
    type Param: SystemParam + 'static;
    fn run<'w>(
        &self,
        entity: Entity,
        tracker: &mut Self::Tracker,
        param: &mut <Self::Param as SystemParam>::Item<'w, '_>,
    ) -> ScriptUpdateResult;
}

/// Returned by `ScriptTracker::update` to indicate the status of a script
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScriptUpdateResult {
    /// Nothing unusual
    NormalRun,
    /// There may be more actions to run; do another update
    Loop,
    /// The script is done, no actions remain that can be run ever in the future
    Finished,
    /// The script wants to be forcefully finished, regardless of remaining actions
    Terminated,
}

impl ScriptUpdateResult {
    pub fn is_loop(self) -> bool {
        self == ScriptUpdateResult::Loop
    }

    pub fn is_end(self) -> bool {
        self == ScriptUpdateResult::Finished || self == ScriptUpdateResult::Terminated
    }
}

pub struct ScriptRuntimeBuilder<T: ScriptAsset> {
    runtime: ScriptRuntime<T>,
}

#[derive(Component)]
pub struct ScriptRuntime<T: ScriptAsset> {
    actions: Vec<T::Action>,
    tracker: T::Tracker,
}

impl<T: ScriptAsset> ScriptRuntimeBuilder<T> {
    pub fn new<'w>(
        entity: Entity,
        settings: &<T::Tracker as ScriptTracker>::Settings,
        param: &mut <<T::Tracker as ScriptTracker>::InitParam as SystemParam>::Item<'w, '_>,
    ) -> Self {
        let mut tracker = T::Tracker::default();
        tracker.init(entity, settings, param);
        ScriptRuntimeBuilder {
            runtime: ScriptRuntime {
                actions: vec![],
                tracker,
            },
        }
    }

    pub fn add_action(mut self, run_if: &T::RunIf, action: &T::Action) -> Self {
        let action_id = self.runtime.actions.len();
        self.runtime.actions.push(action.clone());
        self.runtime.tracker.track_action(run_if, action_id);
        self
    }

    pub fn build(mut self) -> ScriptRuntime<T> {
        self.runtime.tracker.finalize();
        self.runtime
    }
}

fn script_driver_system<T: ScriptAsset>(
    mut commands: Commands,
    mut q_script: Query<(Entity, &mut ScriptRuntime<T>)>,
    mut params: ParamSet<(
        StaticSystemParam<<T::Tracker as ScriptTracker>::UpdateParam>,
        StaticSystemParam<<T::Action as ScriptAction>::Param>,
    )>,
    mut action_queue: Local<Vec<ActionId>>,
) {
    // let mut tracker_param = tracker_param.into_inner();
    // let mut action_param = action_param.into_inner();
    for (e, mut script_rt) in &mut q_script {
        let mut is_loop = true;
        let mut is_end = false;
        while is_loop {
            is_loop = false;
            {
                let mut tracker_param = params.p0().into_inner();
                let r = script_rt
                    .tracker
                    .update(e, &mut tracker_param, &mut action_queue);
                is_loop |= r.is_loop();
                is_end |= r.is_end();
            }
            // trace!(
            //     "Script actions to run: {}",
            //     action_queue.len(),
            // );
            for action_id in action_queue.drain(..) {
                let mut action_param = params.p1().into_inner();
                let r = script_rt.actions[action_id].clone().run(
                    e,
                    &mut script_rt.tracker,
                    &mut action_param,
                );
                is_loop |= r.is_loop();
                is_end |= r.is_end();
            }
        }
        if is_end {
            commands.entity(e).despawn_recursive();
        }
    }
}

fn script_init_system<T: ScriptAsset>(
    mut commands: Commands,
    ass_script: Res<Assets<T>>,
    q_script_handle: Query<(Entity, &Handle<T>), Changed<Handle<T>>>,
    tracker_init_param: StaticSystemParam<<T::Tracker as ScriptTracker>::InitParam>,
) {
    let mut tracker_init_param = tracker_init_param.into_inner();
    for (e, handle) in &q_script_handle {
        if let Some(script) = ass_script.get(&handle) {
            let settings = script.into_settings();
            commands
                .entity(e)
                .insert(script.init(e, &settings, &mut tracker_init_param));
            debug!("Initialized new script.");
        }
    }
}
