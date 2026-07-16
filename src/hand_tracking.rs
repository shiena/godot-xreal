//! XREAL hand tracking → Godot `XRHandTracker`.
//!
//! **Hardware-gated to the XREAL Air 2 Ultra** (outward SLAM cameras + perception feature). The One Pro
//! returns `IsHandTrackingSupported() == false` and produces no data. See
//! `docs/plans/hand-tracking-plan.md`.
//!
//! ## Data path (Approach 2 — the SDK's own exported wrappers)
//!
//! We call the plugin's exported hand wrappers, which use the SDK's internal `InputManager` singleton
//! (so **no NR session handle is needed** — unlike the raw `NRHand*` flat API which takes a session we
//! don't hold). This mirrors how `EnableTearedFrameCount` etc. are called, and is exactly what the Unity
//! SDK's `XREALHandSubSystem` does per frame:
//!
//! - `bool IsHandTrackingSupported()`  (libXREALXRPlugin.so `0x47c08` → `InputManager::IsHandTrackingSupported`)
//! - `bool UpdateHandPose()`           (`0x47fe4` → `InputManager::UpdateHandPose`) — refresh both hands once/frame
//! - `bool GetHandJointsPose(int handType, HandJointsPose* out)` (`0x47ff4` → `InputManager::GetHandJointsPose`)
//!
//! `HandJointsPose` = `int32 isTracked` + `Pose[26]` (each `Pose` = position xyz + rotation xyzw, 7
//! floats). Poses are already converted to **Unity** space by the SDK; we convert Unity→Godot here
//! (position `(x, y, -z)`, quaternion `(-x, -y, z, w)`). The array is in **Unity `XRHandJointID` order**
//! (`[0]=Wrist, [1]=Palm, [2..25]=fingers`); Godot's `XRHandTracker` is `PALM=0, WRIST=1, [2..25]=fingers`
//! (same finger order), so we only swap the first two.
//!
//! ## Open bring-up item (verify on Air 2 Ultra)
//!
//! `UpdateHandPose` no-ops until hand tracking is **enabled** (guarded on `InputManager+0x290`). The SDK
//! enables it via `NRConfigSetHandTrackingEnabled(session, config, true)` during perception setup
//! (session + config handles held inside `NativePerception`, which we don't drive). Wiring that enable
//! (recover the config handle, or find a plugin path) is the first Air 2 Ultra bring-up task; until then
//! `update()` returns `false` and no hands are reported. Everything else here is ready.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use godot::classes::xr_hand_tracker::HandJoint;
use godot::classes::xr_positional_tracker::TrackerHand;
use godot::classes::{INode, Node, XrHandTracker, XrServer, XrTracker};
use godot::prelude::*;

use libloading::Library;

/// One Unity-space joint pose as written by `GetHandJointsPose` (Unity `Pose`: position then rotation).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UnityPose {
    position: [f32; 3],
    rotation: [f32; 4],
}

/// The out-struct filled by `GetHandJointsPose(handType, &mut HandJointsPose)`.
///
/// Matches the SDK's C# `HandJointsPose` under default P/Invoke marshalling: `bool` → 4-byte `BOOL`,
/// then a by-value `Pose[26]` (`SizeConst = XRHandJointID.EndMarker - 1 = 26`).
#[repr(C)]
struct HandJointsPose {
    is_tracked: i32,
    joints: [UnityPose; 26],
}

impl Default for HandJointsPose {
    fn default() -> Self {
        Self { is_tracked: 0, joints: [UnityPose::default(); 26] }
    }
}

type FnBool = unsafe extern "C" fn() -> bool;
type FnGetHandJointsPose = unsafe extern "C" fn(i32, *mut HandJointsPose) -> bool;

struct HandApi {
    _lib: Library, // keep libXREALXRPlugin.so mapped for the fn-pointers' lifetime
    is_supported: FnBool,
    update: FnBool,
    get_joints: FnGetHandJointsPose,
}

// SAFETY: the fn-pointers resolve into libXREALXRPlugin.so (kept mapped by `_lib`); the wrappers use the
// SDK's `InputManager` singleton and take no external state. Only touched under the Mutex.
unsafe impl Send for HandApi {}

static HAND_API: Mutex<Option<HandApi>> = Mutex::new(None);

/// `dlopen` libXREALXRPlugin.so and resolve the three exported hand wrappers. Idempotent; returns a
/// one-line diagnostic. Safe to call before the SDK is up (the wrappers no-op via the InputManager
/// singleton).
fn ensure_api_locked(slot: &mut Option<HandApi>) -> &'static str {
    if slot.is_some() {
        return "already loaded";
    }
    unsafe {
        let lib = match Library::new("libXREALXRPlugin.so") {
            Ok(l) => l,
            Err(_) => return "dlopen libXREALXRPlugin.so failed",
        };
        let is_supported = match lib.get::<FnBool>(b"IsHandTrackingSupported\0") {
            Ok(f) => *f,
            Err(_) => return "dlsym IsHandTrackingSupported failed",
        };
        let update = match lib.get::<FnBool>(b"UpdateHandPose\0") {
            Ok(f) => *f,
            Err(_) => return "dlsym UpdateHandPose failed",
        };
        let get_joints = match lib.get::<FnGetHandJointsPose>(b"GetHandJointsPose\0") {
            Ok(f) => *f,
            Err(_) => return "dlsym GetHandJointsPose failed",
        };
        *slot = Some(HandApi { _lib: lib, is_supported, update, get_joints });
        "loaded"
    }
}

/// `true` if the connected glasses support hand tracking (Air 2 Ultra). `false` on the One Pro or before
/// the SDK is up.
pub fn is_supported() -> bool {
    let mut slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    ensure_api_locked(&mut slot);
    slot.as_ref().map(|a| unsafe { (a.is_supported)() }).unwrap_or(false)
}

/// One converted hand: `tracked` plus 26 Godot-space joint transforms indexed by Godot `HandJoint` ord.
pub struct HandSnapshot {
    pub tracked: bool,
    /// `[godot_joint_ord] -> Transform3D` (Godot space). Index 0 = Palm, 1 = Wrist, 2..25 = fingers.
    pub joints: [Transform3D; 26],
}

/// Refresh both hands (once per frame) then read a hand. `hand_type` is 0 = left, 1 = right (matches the
/// SDK `HandType`). Returns `None` when the API is unavailable or `GetHandJointsPose` fails.
///
/// Call `update_frame()` once before polling both hands so `UpdateHandPose` runs a single time per frame.
pub fn poll(hand_type: i32) -> Option<HandSnapshot> {
    let slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    let api = slot.as_ref()?;
    let mut raw = HandJointsPose::default();
    if !unsafe { (api.get_joints)(hand_type, &mut raw) } {
        return None;
    }
    let mut joints = [Transform3D::IDENTITY; 26];
    for (i, p) in raw.joints.iter().enumerate() {
        // Unity `XRHandJointID` order -> Godot `HandJoint` order: swap Wrist(0)/Palm(1); fingers match.
        let godot_ord = match i {
            0 => 1, // Unity Wrist -> Godot WRIST
            1 => 0, // Unity Palm  -> Godot PALM
            n => n,
        };
        joints[godot_ord] = unity_pose_to_godot(p);
    }
    Some(HandSnapshot { tracked: raw.is_tracked != 0, joints })
}

/// Refresh both hands for this frame. Returns `false` when unavailable or hand tracking is not enabled
/// yet (the enable is attempted lazily via [`ensure_enabled`]).
pub fn update_frame() -> bool {
    ensure_enabled();
    let mut slot = HAND_API.lock().unwrap_or_else(|e| e.into_inner());
    ensure_api_locked(&mut slot);
    slot.as_ref().map(|a| unsafe { (a.update)() }).unwrap_or(false)
}

// --- Enable path (RE, internal plugin functions by `LIB_BASE + offset`) --------------------------------
//
// `UpdateHandPose` no-ops until hand tracking is enabled. The minimal enable is a single internal call:
// `NativePerception::SetHandTrackingEnabled(perception, true)` (`libXREALXRPlugin.so 0x97174`). The
// perception instance is `*(InputManager + 0x48)`, InputManager from `TSingleton::GetInstance` (0x47a10).
// We must NOT poke `+0x290`/`+0x204`/`+0x24c` — those are the STOP path, and `+0x290` is a one-shot latch
// that makes UpdateHandPose return false if set. See docs/plans/hand-tracking-plan.md
// ("Enable path RE 2026-07-16"). Guard on perception/session/config readiness and retry until they're up.

const OFF_GET_INPUT_MANAGER: usize = 0x47a10; // TSingleton<InputManager>::GetInstance()
const OFF_SET_HAND_TRACKING_ENABLED: usize = 0x97174; // NativePerception::SetHandTrackingEnabled(bool)
const IM_PERCEPTION_PTR: usize = 0x48; // InputManager + 0x48 = NativePerception*
const NP_STARTED: usize = 0x18; // NativePerception + 0x18 (non-zero once start succeeded)
const NP_SESSION: usize = 0x28; // NativePerception + 0x28 (NR session handle)
const NP_CONFIG: usize = 0x38; // NativePerception + 0x38 (NR config handle)

static HAND_ENABLED: AtomicBool = AtomicBool::new(false);

type FnGetInputManager = unsafe extern "C" fn() -> *mut u8;
type FnSetHandTrackingEnabled = unsafe extern "C" fn(*mut u8, bool);

/// Attempt the one-shot enable once the SDK's perception is up. Idempotent: does nothing after the first
/// success. Safe to call every frame — it early-returns once enabled and guards every pointer.
pub fn ensure_enabled() {
    if HAND_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let lib_base = crate::signal_guard::lib_base();
    if lib_base == 0 {
        return;
    }
    unsafe {
        let get_im: FnGetInputManager = std::mem::transmute(lib_base + OFF_GET_INPUT_MANAGER);
        let input_manager = get_im();
        if input_manager.is_null() {
            return;
        }
        let perception = (input_manager.add(IM_PERCEPTION_PTR) as *const *mut u8).read();
        if perception.is_null() {
            return;
        }
        let started = perception.add(NP_STARTED).read();
        let session = (perception.add(NP_SESSION) as *const u64).read();
        let config = (perception.add(NP_CONFIG) as *const u64).read();
        if started == 0 || session == 0 || config == 0 {
            return; // perception not fully brought up yet — retry next frame
        }
        let set_enabled: FnSetHandTrackingEnabled =
            std::mem::transmute(lib_base + OFF_SET_HAND_TRACKING_ENABLED);
        set_enabled(perception, true);
        HAND_ENABLED.store(true, Ordering::Relaxed);
        godot::global::godot_print!(
            "[xreal] hand tracking enabled (NativePerception::SetHandTrackingEnabled, session={session:#x} config={config:#x})"
        );
    }
}

/// Convert a Unity-space `Pose` to a Godot `Transform3D` for this port's display space.
///
/// Two steps: (1) Unity (LH, +Z forward) → Godot (RH, -Z forward) negates Z; (2) this port's eye cameras
/// render with an inverted Y (pose handedness `(x,-y,z,w)` — the head rig and phone pointer compensate the
/// same way), so we additionally negate Y. Net: position `(x, -y, -z)`, quaternion `(x, -y, -z, w)`.
/// Device-confirmed on the Air 2 Ultra: without the Y negation the hand rendered upside-down.
fn unity_pose_to_godot(p: &UnityPose) -> Transform3D {
    let pos = Vector3::new(p.position[0], -p.position[1], -p.position[2]);
    let rot = Quaternion::new(p.rotation[0], -p.rotation[1], -p.rotation[2], p.rotation[3]);
    Transform3D::new(Basis::from_quaternion(rot), pos)
}

/// The 26 Godot `HandJoint` ordinals in order (0..=25), for feeding `XrHandTracker`.
const GODOT_JOINTS: [HandJoint; 26] = [
    HandJoint::PALM,
    HandJoint::WRIST,
    HandJoint::THUMB_METACARPAL,
    HandJoint::THUMB_PHALANX_PROXIMAL,
    HandJoint::THUMB_PHALANX_DISTAL,
    HandJoint::THUMB_TIP,
    HandJoint::INDEX_FINGER_METACARPAL,
    HandJoint::INDEX_FINGER_PHALANX_PROXIMAL,
    HandJoint::INDEX_FINGER_PHALANX_INTERMEDIATE,
    HandJoint::INDEX_FINGER_PHALANX_DISTAL,
    HandJoint::INDEX_FINGER_TIP,
    HandJoint::MIDDLE_FINGER_METACARPAL,
    HandJoint::MIDDLE_FINGER_PHALANX_PROXIMAL,
    HandJoint::MIDDLE_FINGER_PHALANX_INTERMEDIATE,
    HandJoint::MIDDLE_FINGER_PHALANX_DISTAL,
    HandJoint::MIDDLE_FINGER_TIP,
    HandJoint::RING_FINGER_METACARPAL,
    HandJoint::RING_FINGER_PHALANX_PROXIMAL,
    HandJoint::RING_FINGER_PHALANX_INTERMEDIATE,
    HandJoint::RING_FINGER_PHALANX_DISTAL,
    HandJoint::RING_FINGER_TIP,
    HandJoint::PINKY_FINGER_METACARPAL,
    HandJoint::PINKY_FINGER_PHALANX_PROXIMAL,
    HandJoint::PINKY_FINGER_PHALANX_INTERMEDIATE,
    HandJoint::PINKY_FINGER_PHALANX_DISTAL,
    HandJoint::PINKY_FINGER_TIP,
];

/// Node that publishes XREAL hand tracking to Godot's `XRServer` as two `XRHandTracker`s
/// (`/user/hand_tracker/left` and `/user/hand_tracker/right`). Add it to the scene; then an
/// `XRHandModifier3D` (with the matching tracker name) animates a hand skeleton, or GDScript reads the
/// trackers via `XRServer.get_tracker(...)`.
///
/// Hardware-gated to the Air 2 Ultra (no-op elsewhere).
#[derive(GodotClass)]
#[class(base = Node)]
pub struct XrealHandTracker {
    base: Base<Node>,
    left: Option<Gd<XrHandTracker>>,
    right: Option<Gd<XrHandTracker>>,
    registered: bool,
    logged_first_tracked: bool,
}

#[godot_api]
impl INode for XrealHandTracker {
    fn init(base: Base<Node>) -> Self {
        Self { base, left: None, right: None, registered: false, logged_first_tracked: false }
    }

    fn ready(&mut self) {
        // Register the two hand trackers once. They are updated every frame in `process`; a hand simply
        // reports `has_tracking_data = false` until the device sees it.
        let left = make_tracker("/user/hand_tracker/left", TrackerHand::LEFT);
        let right = make_tracker("/user/hand_tracker/right", TrackerHand::RIGHT);
        let mut server = XrServer::singleton();
        server.add_tracker(&left.clone().upcast::<XrTracker>());
        server.add_tracker(&right.clone().upcast::<XrTracker>());
        self.left = Some(left);
        self.right = Some(right);
        self.registered = true;
        godot_print!("[xreal] XrealHandTracker: registered left/right hand trackers (supported={})", is_supported());
    }

    fn process(&mut self, _delta: f64) {
        if !self.registered {
            return;
        }
        // One `UpdateHandPose` per frame, then read each hand.
        let updated = update_frame();
        let left = if updated { poll(0) } else { None };
        let right = if updated { poll(1) } else { None };
        if !self.logged_first_tracked
            && (left.as_ref().is_some_and(|s| s.tracked) || right.as_ref().is_some_and(|s| s.tracked))
        {
            self.logged_first_tracked = true;
            godot_print!("[xreal] XrealHandTracker: first tracked hand — feeding XRHandTracker(s)");
        }
        if let Some(t) = self.left.as_mut() {
            feed_tracker(t, left);
        }
        if let Some(t) = self.right.as_mut() {
            feed_tracker(t, right);
        }
    }

    fn exit_tree(&mut self) {
        let mut server = XrServer::singleton();
        if let Some(t) = self.left.take() {
            server.remove_tracker(&t.upcast::<XrTracker>());
        }
        if let Some(t) = self.right.take() {
            server.remove_tracker(&t.upcast::<XrTracker>());
        }
        self.registered = false;
    }
}

fn make_tracker(name: &str, hand: TrackerHand) -> Gd<XrHandTracker> {
    let mut tracker = XrHandTracker::new_gd();
    tracker.set_tracker_name(name);
    tracker.set_tracker_hand(hand);
    tracker
}

/// Push one hand's snapshot into its `XrHandTracker` (or clear tracking when absent/untracked).
fn feed_tracker(tracker: &mut Gd<XrHandTracker>, snapshot: Option<HandSnapshot>) {
    let flags = hand_joint_flags_all();
    match snapshot {
        Some(s) if s.tracked => {
            tracker.set_has_tracking_data(true);
            for (i, joint) in GODOT_JOINTS.iter().enumerate() {
                tracker.set_hand_joint_transform(*joint, s.joints[i]);
                tracker.set_hand_joint_flags(*joint, flags);
            }
        }
        _ => tracker.set_has_tracking_data(false),
    }
}

/// Position + orientation valid & tracked (we don't get velocities from the SDK).
fn hand_joint_flags_all() -> godot::classes::xr_hand_tracker::HandJointFlags {
    use godot::classes::xr_hand_tracker::HandJointFlags;
    HandJointFlags::ORIENTATION_VALID
        | HandJointFlags::ORIENTATION_TRACKED
        | HandJointFlags::POSITION_VALID
        | HandJointFlags::POSITION_TRACKED
}
